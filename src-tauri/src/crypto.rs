//! 官方 IPMsg 协议加密（spec §2）：握手编解码、消息打包/解包、CTR 文件流。
//! 线上格式的字节序结论：E 与签名均为标准大端 PKCS#1-v1.5。

// 能力位常量与部分方法由后续任务（Task 2+）按计划接入，本任务先行定义骨架，
// 暂时压制 dead_code 提示；后续任务接入后可移除。
#![allow(dead_code)]

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aes::cipher::generic_array::GenericArray;
use ctr::cipher::{StreamCipher, StreamCipherSeek};
use rand::RngCore;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::signature::{RandomizedSigner, SignatureEncoding, Verifier};
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::Sha256;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// 能力位（ipmsg.h Ver4.50）
pub const CAPA_RSA1024: u32 = 0x0000_0002;
pub const CAPA_RSA2048: u32 = 0x0000_0004;
pub const CAPA_BLOWFISH128: u32 = 0x0002_0000;
pub const CAPA_AES256: u32 = 0x0010_0000;
pub const CAPA_CAPFILEENC: u32 = 0x0004_0000;
pub const CAPA_SIGN_SHA1: u32 = 0x2000_0000;
pub const CAPA_SIGN_SHA256: u32 = 0x4000_0000;
/// 官方 IV 派生位（ipmsg.h）：置位时 CBC IV = 报文包号十进制 ASCII 左对齐
/// + 剩余零（官方 sprintf(iv, "%u", packet_no)）；未置位 IV 全零。
/// 官方客户端广告与消息都可能带此位（现场 65920006 含之），接收端必须支持。
pub const CAPA_PACKETNO_IV: u32 = 0x0080_0000;

pub const RSA_BITS: usize = 2048;
/// 加密封包组合：RSA2048+AES256+SHA256
pub const CAPA_OUR_SEND: u32 = CAPA_RSA2048 | CAPA_AES256 | CAPA_SIGN_SHA256;

#[derive(Clone)]
pub struct KeyPair {
    priv_key: RsaPrivateKey,
}

impl KeyPair {
    pub fn generate() -> Result<Self, String> {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, RSA_BITS)
            .map_err(|e| format!("RSA 密钥生成失败：{e}"))?;
        Ok(KeyPair { priv_key })
    }

    pub fn to_json(&self) -> String {
        use base64::Engine as _;
        use rsa::pkcs8::EncodePrivateKey;
        let pkcs8 = self.priv_key.to_pkcs8_der().unwrap();
        serde_json::json!({
            "der_b64": base64::engine::general_purpose::STANDARD.encode(pkcs8.as_bytes()),
        })
        .to_string()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        use base64::Engine as _;
        use rsa::pkcs8::DecodePrivateKey;
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| format!("密钥文件损坏：{e}"))?;
        let der = base64::engine::general_purpose::STANDARD
            .decode(v["der_b64"].as_str().unwrap_or_default())
            .map_err(|e| format!("密钥文件损坏：{e}"))?;
        let priv_key = RsaPrivateKey::from_pkcs8_der(&der)
            .map_err(|e| format!("密钥文件损坏：{e}"))?;
        // 尺寸绑定校验：模数必须恰好 RSA_BITS 位。错误尺寸（敌意或损坏）的
        // 密钥文件会让 modulus_be 的定长补齐逻辑下溢/越界 panic，入口直接拒绝。
        use rsa::traits::PublicKeyParts;
        if priv_key.n().bits() != RSA_BITS {
            return Err(format!("密钥模数不是 {RSA_BITS} 位，拒绝加载"));
        }
        Ok(KeyPair { priv_key })
    }

    pub fn public_key(&self) -> RsaPublicKey {
        RsaPublicKey::from(&self.priv_key)
    }

    /// 大端模数字节（2048 位 = 256 字节）
    pub fn modulus_be(&self) -> Vec<u8> {
        use rsa::traits::PublicKeyParts;
        let n = self.priv_key.n().to_bytes_be();
        // 补齐到定长，避免模数最高字节为 0 时长度漂移
        let mut out = vec![0u8; RSA_BITS / 8];
        out[RSA_BITS / 8 - n.len()..].copy_from_slice(&n);
        out
    }

    pub fn exponent_be(&self) -> Vec<u8> {
        use rsa::traits::PublicKeyParts;
        self.priv_key.e().to_bytes_be()
    }

    /// 模数 SHA-256 前 8 字节的冒号分隔大写 hex（设置页核对指纹）
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(self.modulus_be());
        d[..8]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// 小写 hex 编码（ANSPUBKEY 的 E/N 段）
pub(crate) fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 宽容 hex 解码：大小写通吃、容忍首尾空白；空串/奇数长度/非 hex 字符返回 None。
/// 按字节对解码而非按 &str 切片：对端字符串不可信，非 ASCII 输入不得 panic。
pub(crate) fn hex_decode_loose(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    (0..b.len())
        .step_by(2)
        .map(|i| {
            let chunk = std::str::from_utf8(&b[i..i + 2]).ok()?;
            u8::from_str_radix(chunk, 16).ok()
        })
        .collect()
}

/// ANSPUBKEY 扩展部："{capa:X}:{e:x}-{n:x}"（capa 大写 hex，E/N 小写 hex）。
///
/// 模数按官方线格式 **revendian**（字节逆序）输出：官方 Windows 端
/// （v3f_mainwin.cpp MsgGetPubKey）用 `bin2hexstr_revendian` 发送、解析端用
/// `hexstr2bin_revendian` 还原——线上惯例即逆序，我们此前按标准大端输出，
/// 导致官方客户端解析我们的 ANSPUBKEY 得到字节序错误的公钥（2026-08 现场
/// 复现：官方 v4 客户端回包带尾 \0 且全部握手失败）。
pub fn build_anspubkey(capa: u32, key: &KeyPair) -> String {
    // 官方线格式 E 为数值 hex、无前导零（`%X` of DWORD：65537 → "10001"），
    // 按 BigUint 数值输出而非字节 hex（字节形式会是 "010001"，带前导零）。
    // 模数直接输出标准大端 hex：官方发送端 bin2hexstr_revendian 先抵消其
    // CryptoAPI 小端 blob，线上即大端（2026-08 现场以真实样本实测定论）。
    let e_val = BigUint::from_bytes_be(&key.exponent_be());
    format!(
        "{capa:X}:{e_val:x}-{}",
        hex_lower(&key.modulus_be())
    )
}

/// 宽容解析 ANSPUBKEY 扩展部："{capa}:{e}-{n}"，hex 大小写通吃；
/// N 为标准大端（线上惯例，勿做字节反转）；E 是数值 hex、可为奇数长度
/// （官方 65537 → "10001"）——按数值解析而非字节解码；
/// 容忍 C 字符串惯例的尾部 '\0'（官方及相关实现发送时带尾 \0，不剥离会
/// 使模数段变奇数长度而解析失败）；
/// N 允许被对端裁掉前导零字节（BigUint 按值还原，天然左补零语义）
pub fn parse_anspubkey(extra: &str) -> Option<(u32, RsaPublicKey)> {
    let extra = extra.trim_end_matches('\0').trim();
    let (capa_s, rest) = extra.split_once(':')?;
    let capa = u32::from_str_radix(capa_s.trim().trim_start_matches("0x"), 16).ok()?;
    let (e_s, n_s) = rest.split_once('-')?;
    // E 是数值 hex（官方将 65537 写作 "10001"——奇数长度、字节不对齐），
    // 按数值还原而非字节解码；奇数长度时左补零到偶数字节再解码
    let e_txt = e_s.trim_end_matches('\0').trim();
    let e_pad = if e_txt.len() % 2 == 1 {
        format!("0{e_txt}")
    } else {
        e_txt.to_string()
    };
    let e = BigUint::from_bytes_be(&hex_decode_loose(&e_pad)?);
    let n = hex_decode_loose(n_s.trim_end_matches('\0').trim())?;
    if n.len() > RSA_BITS / 8 * 2 {
        return None; // 超过 4096 位直接拒绝
    }
    let pubk = RsaPublicKey::new(BigUint::from_bytes_be(&n), e).ok()?;
    Some((capa, pubk))
}

// ---------------------------------------------------------------------------
// 消息打包/解包（spec §2.2 / §6）
// ---------------------------------------------------------------------------

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
// 本模块只解包 Blowfish（官方第二组合的打包仅在测试辅助里构造，见 tests）
type BlowfishCbcDec = cbc::Decryptor<blowfish::Blowfish>;

/// CBC 的 IV 恒为全零（官方实现如此，密钥每次随机所以 IV 不需要随机）。
/// 注意分组长度不同：AES 为 16B，Blowfish 分组为 64bit → IV 只有 8B。
const CBC_IV0_AES: [u8; 16] = [0u8; 16];
const CBC_IV0_BLOWFISH: [u8; 8] = [0u8; 8];

/// 加密封包（hex 文本）总长上限
pub const MAX_ENCRYPTED_PACKET: usize = 8000;
/// 明文预算：capa+分隔符 + E(512 hex) + ct(2×(n+padding)) + sig(512 hex)
/// ≤ MAX_ENCRYPTED_PACKET 的安全值（约 3.4KB）
const MAX_PLAIN_FOR_SEAL: usize = 3400;

/// 解包结果：`plain` 已剥掉尾部一个 `\0`；`sig_ok` 在无签名段或无对端公钥时为 true
/// （后者由调用方记 diag）。
#[derive(Debug)]
pub struct OpenMsg {
    pub plain: Vec<u8>,
    pub sig_ok: bool,
}

/// 签名哈希选择（打包管线共用）
enum SealHash {
    Sha1,
    Sha256,
}

/// 密封管线（spec §2.2）：随机 32B 会话钥 → RSA-PKCS1v15 加密到对端公钥；
/// 正文 AES-256-CBC（IV=0、PKCS#7）对象是完整明文；签名用我方私钥 `me`，
/// 哈希按调用方选择。输出 "{capa:X}:{E:x}:{ct:x}:{sig:x}"。
fn seal_with_hash(
    pub_key: &RsaPublicKey,
    me: &KeyPair,
    plain: &[u8],
    capa: u32,
    hash: SealHash,
) -> Result<String, String> {
    if plain.len() > MAX_PLAIN_FOR_SEAL {
        return Err("消息过长，加密模式下请分段".into());
    }
    let mut rng = rand::thread_rng();
    let mut skey = [0u8; 32];
    rng.fill_bytes(&mut skey);
    let ct_key = pub_key
        .encrypt(&mut rng, Pkcs1v15Encrypt, &skey)
        .map_err(|e| format!("会话钥加密失败：{e}"))?;

    let mut buf = plain.to_vec();
    let ct = Aes256CbcEnc::new_from_slices(&skey, &CBC_IV0_AES)
        .map_err(|_| "AES 会话钥初始化失败".to_string())?
        .encrypt_padded_vec_mut::<Pkcs7>(&mut buf);

    let sig = match hash {
        SealHash::Sha256 => SigningKey::<Sha256>::new(me.priv_key.clone()).sign_with_rng(&mut rng, plain),
        SealHash::Sha1 => SigningKey::<Sha1>::new(me.priv_key.clone()).sign_with_rng(&mut rng, plain),
    };

    Ok(format!(
        "{capa:X}:{}:{}:{}",
        hex_lower(&ct_key),
        hex_lower(&ct),
        hex_lower(&sig.to_vec()),
    ))
}

/// 加密封包：输出扩展部 "{CAPA_OUR_SEND:X}:{E:x}:{ct:x}:{sig:x}"。
/// - 会话钥：随机 32B → RSA-PKCS1v15 加密到对端公钥；
/// - 正文：AES-256-CBC（IV=0、PKCS#7），对象是**含尾部 \0** 的完整明文；
/// - 签名：RSA-PKCS1v15-SHA256，用我方私钥 `me`，对象同样是含 \0 的完整明文。
pub fn seal_message(pub_key: &RsaPublicKey, me: &KeyPair, plain: &[u8]) -> Result<String, String> {
    seal_with_hash(pub_key, me, plain, CAPA_OUR_SEND, SealHash::Sha256)
}

/// 解包扩展部 "{capa}:{E}:{ct}[:{sig}]"。线上不可信数据：任何畸形输入都只返回 Err，
/// 绝不 panic。受支持组合：
/// - `RSA_2048|AES_256` → AES-256-CBC（IV=0、PKCS#7）
/// - `RSA_1024|BLOWFISH_128` → Blowfish-CBC（同上；会话钥按变长 4~56B 处理）
///
/// 签名校验针对**剥掉尾部 \0 之前**的完整明文；带 SIGN_SHA256 用 SHA-256，
/// 带 SIGN_SHA1 用 SHA-1。无签名段、或调用方没给对端公钥（无法验签）→ `sig_ok=true`；
/// 签名段存在但 hex 坏/验签失败/未声明哈希算法且给了对端公钥 → `sig_ok=false`。
pub fn open_message(
    priv_kp: &KeyPair,
    extra: &str,
    peer_pubs: &[RsaPublicKey],
    pkt_no: u32,
) -> Result<OpenMsg, String> {
    // 官方扩展部按 C 字符串惯例带尾 '\0'（ANSPUBKEY/加密消息皆然，现场实测）；
    // 不剥会使签名段 hex 变奇数长度、验签恒败（sig_ok=false）
    let extra = extra.trim_end_matches('\0').trim();
    let segs: Vec<&str> = extra.split(':').collect();
    if !(3..=4).contains(&segs.len()) {
        return Err("报文段数非法（应为 3~4 段）".into());
    }
    let capa = u32::from_str_radix(segs[0].trim().trim_start_matches("0x"), 16)
        .map_err(|_| "坏的能力位".to_string())?;
    // 先选对称算法再解 RSA：不支持的组合直接拒绝，省一次昂贵的私钥运算
    let aes_mode = if capa & CAPA_AES256 != 0 {
        true
    } else if capa & CAPA_BLOWFISH128 != 0 {
        false
    } else {
        return Err("不支持加密组合".into());
    };

    let ct_key = hex_decode_loose(segs[1]).ok_or("会话钥 hex 坏")?;
    let skey = priv_kp
        .priv_key
        .decrypt(Pkcs1v15Encrypt, &ct_key)
        .map_err(|_| "会话钥解密失败（可能并非发给我方）".to_string())?;
    let ct = hex_decode_loose(segs[2]).ok_or("密文 hex 坏")?;

    // IV：未带 PACKETNO_IV 位时为全零；带位时按官方用包号十进制 ASCII
    // 左对齐（AES 16B / Blowfish 8B，剩余补零）
    let mut iv_aes = [0u8; 16];
    let mut iv_bf = [0u8; 8];
    if capa & CAPA_PACKETNO_IV != 0 {
        let s = pkt_no.to_string();
        let b = s.as_bytes();
        let n16 = b.len().min(16);
        iv_aes[..n16].copy_from_slice(&b[..n16]);
        let n8 = b.len().min(8);
        iv_bf[..n8].copy_from_slice(&b[..n8]);
    }

    let plain_full: Vec<u8> = if aes_mode {
        let k32: [u8; 32] = skey.try_into().map_err(|_| "AES 会话钥长度异常".to_string())?;
        Aes256CbcDec::new_from_slices(&k32, &iv_aes)
            .map_err(|_| "AES 初始化失败".to_string())?
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "AES 解密失败（填充校验不过）".to_string())?
    } else {
        BlowfishCbcDec::new_from_slices(&skey, &iv_bf)
            .map_err(|_| "Blowfish 会话钥/IV 异常".to_string())?
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "Blowfish 解密失败（填充校验不过）".to_string())?
    };

    // 签名校验：完整明文（含尾部 \0）。候选公钥依次尝试（最新+上一把备选：
    // 对端多客户端交替/多实例时任一命中即通过）；无候选（无法验签）→ true
    let mut sig_ok = true;
    if let Some(sig_hex) = segs.get(3) {
        sig_ok = if peer_pubs.is_empty() {
            true // 无候选公钥：无法验签，保持可信（调用方记 diag）
        } else {
            match hex_decode_loose(sig_hex).and_then(|b| Signature::try_from(&b[..]).ok()) {
            Some(sig) => {
                if capa & (CAPA_SIGN_SHA256 | CAPA_SIGN_SHA1) != 0 {
                    let mut ok = false;
                    for pp in peer_pubs {
                        ok = if capa & CAPA_SIGN_SHA256 != 0 {
                            VerifyingKey::<Sha256>::new(pp.clone())
                                .verify(&plain_full, &sig)
                                .is_ok()
                        } else if capa & CAPA_SIGN_SHA1 != 0 {
                            VerifyingKey::<Sha1>::new(pp.clone())
                                .verify(&plain_full, &sig)
                                .is_ok()
                        } else {
                            false
                        };
                        if ok {
                            break;
                        }
                    }
                    ok
                } else {
                    false // 有签名段却没声明哈希算法：无法验证，按不可信处理
                }
            }
            None => false, // 签名段不是合法 hex
        }};
    }

    let mut plain = plain_full;
    if plain.last() == Some(&0) {
        plain.pop(); // 只剥一个尾部 \0
    }
    Ok(OpenMsg { plain, sig_ok })
}

// ---------------------------------------------------------------------------
// 取文件请求的密封/解封（spec §7）
// ---------------------------------------------------------------------------

/// 文件请求钉死 SHA-1 变体组合：CAPA_RSA2048|CAPA_AES256|CAPA_SIGN_SHA1
pub const CAPA_FILE_REQUEST: u32 = CAPA_RSA2048 | CAPA_AES256 | CAPA_SIGN_SHA1;

/// 密封装文件取回请求（spec §7）：内层 `{pkt:x}:{id:x}[:{offset:x}]:(900000|4000000)[:key]`。
/// 打包格式与 seal_message 完全一致，仅组合与签名哈希按 §7 钉为
/// RSA-2048 + AES-256 + SHA-1（capa=CAPA_FILE_REQUEST=0x20100004）。
/// `pkt_no` 为语义预留（TCP 请求行头部的包号，CTR nonce 由它派生），不参与签名。
pub fn seal_file_request(
    pub_key: &RsaPublicKey,
    me: &KeyPair,
    _pkt_no: u32,
    inner: &str,
) -> Result<String, String> {
    seal_with_hash(pub_key, me, inner.as_bytes(), CAPA_FILE_REQUEST, SealHash::Sha1)
}

/// 解封装文件取回请求。签名核验此处跳过（服务端通常未缓存请求方公钥，
/// 无法验签；需要严格化时由调用方在缓存命中后另行校验）。
/// 返回 `(内层字符串, 正文是否加密)`：
/// - 内层末段为 `4000000` → enc_body=false（NOENC_FILEBODY，明文流）；
/// - 倒数第二段为 `900000`（末段是 64 位 hex 的 AES-256 钥）→ enc_body=true；
/// - 其它参数一律 Err（「不支持文件加密参数」）。
///
/// 兼容 SHA-256 变体的历史/前向请求：open_message 按能力位自适应。
pub fn open_file_request(
    priv_kp: &KeyPair,
    extra: &str,
    pkt_no: u32,
) -> Result<(String, bool), String> {
    let out = open_message(priv_kp, extra, &[], pkt_no)?;
    let inner = String::from_utf8_lossy(&out.plain).into_owned();
    let segs: Vec<&str> = inner.split(':').collect();
    let enc_body = if segs.last().copied() == Some("4000000") {
        false
    } else if segs.len() >= 2 && segs[segs.len() - 2] == "900000" {
        true
    } else {
        return Err("不支持文件加密参数".into());
    };
    Ok((inner, enc_body))
}

// ---------------------------------------------------------------------------
// CTR 文件流原语（spec §7）：nonce 规则 + 断点续传偏移对齐
// ---------------------------------------------------------------------------

/// CTR 初始计数器：包号十进制 ASCII 左对齐进前 10 字节，其余字节全零
/// （官方实现如此，保证收发双方对同一包号推出同一密钥流）。
/// 十进制超过 10 位时截取前 10 位，绝不 panic。
pub fn ctr_nonce(pkt_no: u32) -> [u8; 16] {
    let mut n = [0u8; 16];
    let s = pkt_no.to_string();
    let take = s.len().min(10);
    n[..take].copy_from_slice(&s.as_bytes()[..take]);
    n
}

/// AES-256-CTR 文件流加解密器（CTR 模式加解密同操作）。
pub struct CtrCipher {
    cipher: ctr::Ctr128BE<aes::Aes256>,
}

impl CtrCipher {
    pub fn new(key: &[u8; 32], pkt_no: u32) -> Self {
        let nonce = ctr_nonce(pkt_no);
        Self {
            cipher: ctr::Ctr128BE::new(key.into(), GenericArray::from_slice(&nonce)),
        }
    }

    /// 密钥流位置 = 流绝对偏移（断点续传对齐，spec §7）：
    /// 在 `abs_pos` 处续写分片，结果与整流一次性处理逐字节一致。
    pub fn seek(&mut self, abs_pos: u64) {
        self.cipher.seek(abs_pos);
    }

    /// 对 buf 原地施加（或解除）密钥流。
    pub fn apply(&mut self, buf: &mut [u8]) {
        self.cipher.apply_keystream(buf);
    }
}

// ---------------------------------------------------------------------------
// TCP 文件流加密包装（spec §7）：双向过同一 CTR 密钥流
// ---------------------------------------------------------------------------

/// 把任意 AsyncRead+AsyncWrite 流包进 AES-256-CTR 密钥流：
/// 读方向对每次新读入的字节解密，写方向对写出字节加密，
/// 密钥流位置 = 流绝对偏移（构造时 seek 到 start_pos，断点续传免费对齐）。
pub struct EncStream<S> {
    pub inner: S,
    pub c: CtrCipher,
    /// 已加密、尚未被底层完全接受的密文（写路径缓冲）
    outbuf: Vec<u8>,
    /// outbuf 中底层已接受的前缀长度
    outpos: usize,
}

impl<S> EncStream<S> {
    /// `start_pos` 为流的绝对起始偏移（整文件传输传 0；断点续传传已收字节数）
    pub fn new(inner: S, key: &[u8; 32], pkt_no: u32, start_pos: u64) -> Self {
        let mut c = CtrCipher::new(key, pkt_no);
        if start_pos > 0 {
            c.seek(start_pos);
        }
        Self { inner, c, outbuf: Vec::new(), outpos: 0 }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for EncStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            // 只解密本次新读入的段落：密钥流随流绝对偏移连续推进
            Poll::Ready(Ok(())) => {
                this.c.apply(&mut buf.filled_mut()[before..]);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> EncStream<S> {
    /// 排空 outbuf 中尚未被底层接受的剩余密文。
    /// 返回本次排空新接受的字节数；Pending 原样上抛（此时未消耗任何新明文，
    /// 调用方之后会用同一缓冲重新 poll，密钥流绝不因此二次推进）。
    fn poll_drain_outbuf(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut accepted_now = 0usize;
        while self.outpos < self.outbuf.len() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.outbuf[self.outpos..]) {
                Poll::Ready(Ok(n)) => {
                    if n == 0 {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "EncStream 底层零写入",
                        )));
                    }
                    self.outpos += n;
                    accepted_now += n;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(accepted_now))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for EncStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        // 残留密文在途：本次只推进一格并按实际接受量记账（Ok(n) 让调用方
        // 前移切片），绝不在残留未清空时对新 buf 加密——否则同一段明文会
        // 既已随残留上线、又被重新加密重发，密钥流与线上长度双重错位。
        // AsyncWrite 契约保证 Pending/短写后调用方以剩余切片重试，因此残留
        // 恰好始终对应「当前未记账明文的后缀」。
        if this.outpos < this.outbuf.len() {
            return match Pin::new(&mut this.inner).poll_write(cx, &this.outbuf[this.outpos..]) {
                Poll::Ready(Ok(n)) => {
                    this.outpos += n;
                    Poll::Ready(Ok(n))
                }
                other => other,
            };
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // 无残留：整段新明文只加密一次，随后立即尝试推送一次；
        // 短写/Pending 的余量留给上面的残留分支续传
        let mut ct = buf.to_vec();
        this.c.apply(&mut ct);
        this.outbuf = ct;
        this.outpos = 0;
        match Pin::new(&mut this.inner).poll_write(cx, &this.outbuf) {
            Poll::Ready(Ok(n)) => {
                this.outpos += n;
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // 先把未落盘的密文全部推给底层，再让底层刷
        match this.poll_drain_outbuf(cx) {
            Poll::Ready(Ok(_)) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // 关闭前必须排空缓存密文，否则尾部密文丢失、对端校验失败
        match this.poll_drain_outbuf(cx) {
            Poll::Ready(Ok(_)) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts;

    #[test]
    fn keypair_json_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        let kp2 = KeyPair::from_json(&kp.to_json()).unwrap();
        assert_eq!(kp.modulus_be(), kp2.modulus_be());
        assert_eq!(kp.exponent_be(), vec![0x01, 0x00, 0x01]); // 65537
        assert_eq!(kp.modulus_be().len(), 256);
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(KeyPair::from_json("not json").is_err());
    }

    #[test]
    fn from_json_rejects_wrong_size_key() {
        // 敌意/错误尺寸的密钥文件必须被拒绝：非 2048 位模数会让 modulus_be
        // 的定长补齐逻辑越界。加载入口就挡掉，绝不能让坏钥匙混进来。
        let mut rng = rand::thread_rng();
        let priv_1024 = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let kp = KeyPair { priv_key: priv_1024 };
        assert!(
            KeyPair::from_json(&kp.to_json()).is_err(),
            "非 {RSA_BITS} 位密钥的 from_json 必须 Err"
        );
    }

    #[test]
    fn fingerprint_is_stable_colon_hex() {
        let kp = KeyPair::generate().unwrap();
        let fp = kp.fingerprint();
        assert_eq!(fp.split(':').count(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
    }

    #[test]
    fn anspubkey_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        let capa = CAPA_RSA2048 | CAPA_AES256 | CAPA_CAPFILEENC;
        let s = build_anspubkey(capa, &kp);
        // 大写 hex capa。注：brief 原文断言 "40100404:"，但按 Task 1 已提交常量
        // RSA2048|AES256|CAPFILEENC = 0x4|0x100000|0x40000 = 0x140004，正确前缀为 "140004:"
        assert!(s.starts_with("140004:"));
        let (got_capa, pubk) = parse_anspubkey(&s).unwrap();
        assert_eq!(got_capa, capa);
        assert_eq!(pubk.n().to_bytes_be(), kp.modulus_be());
        assert_eq!(pubk.e().to_bytes_be(), vec![1, 0, 1]);
    }

    #[test]
    fn anspubkey_parse_tolerates_case_and_build_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        // 全小写 hex（含 E 数值小写）也必须能解析；官方样本再叠尾 \0
        let s_lower = format!(
            "{:x}:{}-{}\0",
            CAPA_OUR_SEND,
            "10001",
            hex_lower(&kp.modulus_be())
        );
        let (capa, pubk) = parse_anspubkey(&s_lower).unwrap();
        assert_eq!(capa, CAPA_OUR_SEND);
        assert_eq!(pubk.n().to_bytes_be(), kp.modulus_be());
        assert_eq!(pubk.e().to_bytes_be(), vec![1, 0, 1]);
        // build 产物（e 无前导零大端）roundtrip
        let s2 = build_anspubkey(CAPA_OUR_SEND, &kp);
        assert!(s2.starts_with(&format!("{:X}:10001-", CAPA_OUR_SEND)));
        let (_, pubk2) = parse_anspubkey(&s2).unwrap();
        assert_eq!(pubk2.n().to_bytes_be(), kp.modulus_be());
    }

    #[test]
    fn anspubkey_parse_rejects_junk() {
        assert!(parse_anspubkey("no-colon").is_none());
        assert!(parse_anspubkey("40100004:zz-aa").is_none());
    }

    /// 官方客户端 ANSPUBKEY 现场样本（2026-08 排查捕获，10.200.231.11）：
    /// extra 全长 528B = capa8 ':' e5 '-' n512 + 尾部 '\0'（C 字符串惯例）。
    /// N 为标准大端 hex（官方 revendian 已在其发送端抵消 CryptoAPI 小端）。
    /// 修复前该样本解析失败（尾 \0 使 n 段变 513 字符奇数长度；E "10001"
    /// 5 字符按字节解码亦失败）。
    #[test]
    fn anspubkey_accepts_official_sample_with_trailing_nul() {
        let kp = KeyPair::generate().unwrap();
        let s = format!(
            "{:X}:{}-{}\0",
            0x6592_0006u32,
            "10001",
            hex_lower(&kp.modulus_be())
        );
        assert_eq!(s.len(), 528);
        let (capa, pubk) = parse_anspubkey(&s).unwrap();
        assert_eq!(capa, 0x6592_0006);
        assert_eq!(pubk.n().to_bytes_be(), kp.modulus_be());
        assert_eq!(pubk.e().to_bytes_be(), vec![1, 0, 1]);
    }

    #[test]
    fn hex_decode_loose_rejects_bad_input_without_panic() {
        // 对端发来的字符串不可信：非 ASCII（偶数字节长）也不得 panic
        assert!(hex_decode_loose("").is_none()); // 空串
        assert!(hex_decode_loose("abc").is_none()); // 奇数长度
        assert!(hex_decode_loose("zz").is_none()); // 非 hex 字符
        assert!(hex_decode_loose("中中").is_none()); // 多字节 UTF-8，字节长为偶数
        assert_eq!(
            hex_decode_loose("AbCd"),
            Some(vec![0xab, 0xcd]) // 大小写通吃
        );
        assert_eq!(hex_decode_loose(" 01  "), Some(vec![0x01])); // 容忍首尾空白
    }

    #[test]
    fn seal_open_roundtrip_with_files_section() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        // 接口修正后的签名：加密目标是对方公钥，签名者是我方密钥对
        let (me, peer_pub) = (&kp_a, &kp_b.public_key());
        // 模拟「正文\0文件公告」整体进密文（spec §6）。
        // 注：brief 原文用 `\a`，Rust 无此转义——文件公告段的结束符就是 BEL(0x07)，字节串里写作 \x07
        let plain = b"hello\nworld\0report.zip:100:20:1:\x07";
        let sealed = seal_message(peer_pub, me, plain).unwrap();
        assert!(sealed.starts_with(&format!("{:X}:", CAPA_OUR_SEND)));
        let out = open_message(&kp_b, &sealed, &[kp_a.public_key()], 0).unwrap();
        assert_eq!(out.plain, &plain[..]); // 含文件公告整体；明文以 \x07 结尾，无尾部 \0 可剥
                                           // （尾部 \0 剥离的覆盖见 open_supports_blowfish_combo 的 b"legacy\0"）
        assert!(out.sig_ok);
        // 解密本身只要求接收方私钥；无对端公钥时无法验签，sig_ok 保持 true（调用方记 diag）
        let out_no_pp = open_message(&kp_b, &sealed, &[], 0).unwrap();
        assert_eq!(out_no_pp.plain, &plain[..]);
        assert!(out_no_pp.sig_ok);
    }

    /// 官方 PACKETNO_IV（ipmsg.h 0x00800000）：CBC IV = 报文包号十进制 ASCII
    /// 左对齐 + 剩余零。官方客户端广告位含它（现场 65920006），其消息与文件
    /// 请求可能带此位——接收端必须按包号派生 IV，否则首块乱、验签必败。
    #[test]
    fn open_message_respects_packetno_iv() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let plain = b"packetno-iv-test\0";
        // 手工构造官方风格密文：IV = 包号 ASCII（10 位内），CBC AES256，带 SHA-256 签名
        let mut rng = rand::thread_rng();
        let skey: [u8; 32] = rand::random();
        let ct_key = kp_b
            .public_key()
            .encrypt(&mut rng, Pkcs1v15Encrypt, &skey)
            .unwrap();
        let pkt_no: u32 = 987654321;
        let mut iv = [0u8; 16];
        let s = pkt_no.to_string();
        iv[..s.len()].copy_from_slice(s.as_bytes());
        let mut buf = plain.to_vec();
        let ct = Aes256CbcEnc::new_from_slices(&skey, &iv)
            .unwrap()
            .encrypt_padded_vec_mut::<Pkcs7>(&mut buf);
        let sk = rsa::pkcs1v15::SigningKey::<Sha256>::new(kp_a.priv_key.clone());
        let sig = sk.sign_with_rng(&mut rng, plain).to_vec();
        let capa = CAPA_RSA2048 | CAPA_AES256 | CAPA_SIGN_SHA256 | CAPA_PACKETNO_IV;
        let extra = format!(
            "{:X}:{}:{}:{}",
            capa,
            hex_lower(&ct_key),
            hex_lower(&ct),
            hex_lower(&sig)
        );
        // 正确包号：IV 对齐 → 全文还原 + 验签通过
        let out = open_message(&kp_b, &extra, &[kp_a.public_key()], pkt_no).unwrap();
        assert_eq!(out.plain, b"packetno-iv-test");
        assert!(out.sig_ok);
        // 错包号：IV 错 → 首块乱，验签失败（机制：sig_ok=false 而非 Err）
        let out_bad = open_message(&kp_b, &extra, &[kp_a.public_key()], 1).unwrap();
        assert!(!out_bad.sig_ok);
        assert_ne!(out_bad.plain, b"packetno-iv-test");
    }

    #[test]
    fn open_supports_blowfish_combo() {
        // 官方第二组合样本的接收方是 RSA-1024。KeyPair::generate 固定 2048 位，
        // 故按 brief 在测试内直接构造 1024 位私钥（仅限测试），再包成 KeyPair。
        let mut rng = rand::thread_rng();
        let priv_1024 = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let kp = KeyPair { priv_key: priv_1024 };
        let plain = b"legacy\0";
        let sealed = seal_message_compat_rsa1024_blowfish(&kp.public_key(), plain).unwrap();
        assert!(sealed.starts_with(&format!("{:X}:", CAPA_RSA1024 | CAPA_BLOWFISH128)));
        let out = open_message(&kp, &sealed, &[], 0).unwrap();
        assert_eq!(out.plain, b"legacy");
        assert_eq!(out.sig_ok, true); // 该组合无签名段
    }

    #[test]
    fn tampered_signature_is_reported_not_fatal() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"x\0").unwrap();
        // 破坏签名段最后两个字符（保证与原值不同的确定性替换）
        let (head, sig) = sealed.rsplit_once(':').unwrap();
        let mut bad_tail = sig[..sig.len() - 2].to_owned();
        bad_tail.push_str(if sig.ends_with("00") { "11" } else { "00" });
        let bad = format!("{}:{}", head, bad_tail);
        let out = open_message(&kp_b, &bad, &[kp_a.public_key()], 0).unwrap();
        assert!(!out.sig_ok);
    }

    #[test]
    fn signature_accepts_any_candidate_key() {
        // 对端多客户端交替/多实例（2026-08-26 实测场景）：同一 IP 轮换密钥对
        // 发言，缓存保留最新+上一把备选；任一候选命中即验签通过
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let kp_a_old = KeyPair::generate().unwrap();
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"hi\0").unwrap();
        // 候选 = [旧钥, 当前签名钥]（备选命中）
        let out = open_message(
            &kp_b,
            &sealed,
            &[kp_a_old.public_key(), kp_a.public_key()],
            0,
        )
        .unwrap();
        assert!(out.sig_ok);
        // 候选都不匹配 → 验签失败
        let out_bad = open_message(&kp_b, &sealed, &[kp_a_old.public_key()], 0).unwrap();
        assert!(!out_bad.sig_ok);
    }

    #[test]
    fn signature_verifies_over_full_pre_strip_plaintext() {
        // 回归钉子：官方客户端的签名对象是**含尾部 \0** 的完整明文。
        // 若未来把验签对象误改成剥掉 \0 之后的明文（互操作回归），本测试必须失败。
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"x\0").unwrap();
        let out = open_message(&kp_b, &sealed, &[kp_a.public_key()], 0).unwrap();
        assert!(out.sig_ok);
        assert_eq!(out.plain, b"x"); // 剥离只影响返回值，不影响验签对象
    }

    #[test]
    fn trailing_c_string_nul_on_extra_does_not_break_signature() {
        // 官方扩展部带 C 字符串尾 \0（现场 2026-08 实测 10.200.230.3 "123" 消息）：
        // 不剥会使签名段 hex 变 513 字符奇数长度 → 解码失败 → 验签恒败。
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let mut sealed = seal_message(&kp_b.public_key(), &kp_a, b"123\0").unwrap();
        sealed.push('\0'); // 模拟官方线格式的尾 \0
        let out = open_message(&kp_b, &sealed, &[kp_a.public_key()], 0).unwrap();
        assert_eq!(out.plain, b"123");
        assert!(out.sig_ok, "剥尾 \\0 后签名必须验证通过");
    }

    #[test]
    fn undecodable_signature_hex_is_reported() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"x\0").unwrap();
        let (head, _) = sealed.rsplit_once(':').unwrap();
        // 签名段存在但不是合法 hex：有对端公钥时必须报 sig_ok=false，且不能致命
        let bad = format!("{}:zz", head);
        let out = open_message(&kp_b, &bad, &[kp_a.public_key()], 0).unwrap();
        assert!(!out.sig_ok);
    }

    #[test]
    fn oversize_plain_is_rejected_with_hint() {
        let (me, peer) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let big = vec![b'a'; MAX_ENCRYPTED_PACKET]; // 远超 hex 上限
        let err = seal_message(&peer.public_key(), &me, &big).unwrap_err();
        assert!(err.contains("消息过长"));
    }

    #[test]
    fn open_rejects_unsupported_combination() {
        let kp = KeyPair::generate().unwrap();
        // 只有 RSA/签名位、没有任何受支持的对称算法位
        let bogus = format!(
            "{:X}:{}:{}",
            CAPA_RSA2048 | CAPA_SIGN_SHA256,
            hex_lower(&[0x42u8; 8]),
            hex_lower(&[0u8; 16])
        );
        let err = open_message(&kp, &bogus, &[], 0).unwrap_err();
        assert!(err.contains("不支持加密组合"));
    }

    #[test]
    fn open_survives_hostile_input_without_panic() {
        // 解析的是线上不可信数据：任何垃圾输入都只能得到 Err/结果，绝不允许 panic
        let kp = KeyPair::generate().unwrap();
        let s_capa_only = format!("{:X}:", CAPA_OUR_SEND);
        let s_max_nums = format!("{:X}:{:x}:{:x}", u32::MAX, u64::MAX, u64::MAX);
        let samples = [
            "",
            ":",
            "::",
            "::::",
            "zz:zz:zz",
            "4:x:y:z",
            "40001004:::",
            "40001004:ffff:ffff:zz",
            "😀😀😀",
            s_capa_only.as_str(),
            s_max_nums.as_str(),
        ];
        for s in samples {
            let _ = open_message(&kp, s, &[], 0);
            let _ = open_message(&kp, s, &[kp.public_key()], 0);
        }
    }

    #[test]
    fn ctr_nonce_layout() {
        let n = ctr_nonce(12345);
        assert_eq!(&n[..5], b"12345"); // 十进制 ASCII 左对齐
                                       // 其余全零（共 16 字节）——整组比较一次钉死布局
        assert_eq!(n, *b"12345\0\0\0\0\0\0\0\0\0\0\0");
    }

    #[test]
    fn ctr_nonce_fills_ten_digits_at_u32_max() {
        // u32 最大值恰好 10 位十进制：占满前 10 字节 + 6 零；截断护栏不得 panic
        assert_eq!(ctr_nonce(u32::MAX), *b"4294967295\0\0\0\0\0\0");
    }

    #[test]
    fn ctr_seek_alignment_matches_whole_stream() {
        let key = [7u8; 32];
        let pkt = 42;
        let file: Vec<u8> = (0..1000usize).map(|i| i as u8).collect();
        // 整流一次性“加密”
        let mut whole = file.clone();
        CtrCipher::new(&key, pkt).apply(&mut whole);
        // 从 offset=777 开始加密的分片，必须与整流的对应片段逐字节相同
        let mut tail = file[777..].to_vec();
        let mut c = CtrCipher::new(&key, pkt);
        c.seek(777);
        c.apply(&mut tail);
        assert_eq!(&whole[777..], &tail[..]);
    }

    #[test]
    fn ctr_multi_chunk_resume_aligns_with_whole_stream() {
        // 模拟多次断点续传：在任意绝对偏移处重新 seek 续写，
        // 各分片拼接结果必须与整流一次性处理逐字节相同（spec §7 对齐契约）
        let key = [9u8; 32];
        let pkt = 7;
        let file: Vec<u8> = (0..600usize).map(|i| (i * 7 % 251) as u8).collect();
        let mut whole = file.clone();
        CtrCipher::new(&key, pkt).apply(&mut whole);
        let mut out = Vec::with_capacity(file.len());
        for (start, len) in [(0usize, 1usize), (1, 199), (200, 5), (205, 395)] {
            let mut c = CtrCipher::new(&key, pkt);
            c.seek(start as u64);
            let mut chunk = file[start..start + len].to_vec();
            c.apply(&mut chunk);
            out.extend_from_slice(&chunk);
        }
        assert_eq!(out, whole);
    }

    /* ---------------- Task 10：文件请求密封/解封 与 TCP 流加密（spec §7） ---------------- */

    /// 线上常量钉死：文件请求组合的 hex 值一旦漂移，真机互通即静默失败
    #[test]
    fn wire_constant_pins() {
        assert_eq!(CAPA_FILE_REQUEST, 0x2010_0004); // RSA2048|AES256|SIGN_SHA1
    }

    #[test]
    fn file_request_roundtrip() {
        let (a, b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let inner = "1f:2a:900000:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let sealed = seal_file_request(&b.public_key(), &a, 999, inner).unwrap();
        // 注：brief 原文断言 "20001004:"，但按已提交常量
        // RSA2048|AES256|SIGN_SHA1 = 0x4|0x100000|0x20000000 = 0x20100004，
        // 正确前缀为 "20100004:"（与 Task 1 对 brief 数值笔误的处理先例一致）
        assert!(sealed.starts_with(&format!("{:X}:", CAPA_FILE_REQUEST)));
        let (got, enc_body) = open_file_request(&b, &sealed, 0).unwrap();
        assert!(enc_body);
        assert_eq!(got, inner);
        // NOENC 变体
        let inner2 = "1f:2a:4000000";
        let (got2, enc2) =
            open_file_request(&b, &seal_file_request(&b.public_key(), &a, 998, inner2).unwrap(), 0)
                .unwrap();
        assert!(!enc2);
        assert_eq!(got2, inner2);
    }

    #[test]
    fn file_request_accepts_sha256_variant_for_forward_compat() {
        // 前向兼容：沿用 SHA-256 组合的文件请求也必须能解封
        let (a, b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let inner = "1f:2a:4000000";
        let sealed = seal_message(&b.public_key(), &a, inner.as_bytes()).unwrap();
        assert!(sealed.starts_with(&format!("{:X}:", CAPA_OUR_SEND)));
        let (got, enc_body) = open_file_request(&b, &sealed, 0).unwrap();
        assert!(!enc_body);
        assert_eq!(got, inner);
    }

    #[test]
    fn file_request_rejects_unknown_enc_param() {
        let (a, b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_file_request(&b.public_key(), &a, 7, "1f:2a:777777").unwrap();
        let err = open_file_request(&b, &sealed, 0).unwrap_err();
        assert!(err.contains("不支持文件加密参数"));
    }

    #[test]
    fn file_request_survives_hostile_input_without_panic() {
        // 解封的是线上不可信数据：垃圾输入只允许 Err，绝不允许 panic
        let kp = KeyPair::generate().unwrap();
        let max_nums = format!("{:X}:{:x}:{:x}", u32::MAX, u64::MAX, u64::MAX);
        for s in ["", ":", "zz", "1:2", max_nums.as_str()] {
            let _ = open_file_request(&kp, s, 0);
        }
    }

    /// 写方向：明文过 EncStream 后线上字节必须等于整流一次性 CTR 的密文
    #[tokio::test]
    async fn enc_stream_write_side_matches_ctr_whole_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key = [11u8; 32];
        let pkt = 555;
        let data: Vec<u8> = (0..70_000usize).map(|i| ((i * 13 + 9) % 253) as u8).collect();
        let (c, mut raw_peer) = tokio::io::duplex(128 * 1024);
        let mut w = EncStream::new(c, &key, pkt, 0);
        w.write_all(&data).await.unwrap();
        w.flush().await.unwrap();
        w.shutdown().await.unwrap();

        let mut wire = Vec::new();
        raw_peer.read_to_end(&mut wire).await.unwrap();
        let mut want_ct = data.clone();
        CtrCipher::new(&key, pkt).apply(&mut want_ct);
        assert_eq!(wire, want_ct, "写方向逐字节等于 AES-CTR 整流密文");
    }

    /// 读方向：对端发来的 CTR 密文经 EncStream 必须还原成明文
    #[tokio::test]
    async fn enc_stream_read_side_recovers_plaintext() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key = [22u8; 32];
        let pkt = 556;
        let data: Vec<u8> = (0..70_000usize).map(|i| ((i * 7 + 3) % 251) as u8).collect();
        let mut ct = data.clone();
        CtrCipher::new(&key, pkt).apply(&mut ct);

        let (c, mut raw_feed) = tokio::io::duplex(128 * 1024);
        let mut r = EncStream::new(c, &key, pkt, 0);
        raw_feed.write_all(&ct).await.unwrap();
        raw_feed.shutdown().await.unwrap();

        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, data, "读方向必须还原明文");
    }

    /// 断点续传对齐（spec §7）：写端/读端各自从同一绝对偏移起步。
    /// 线上字节必须等于「整流一次性加密后的对应片段」（写方向 seek 生效），
    /// 读端还原结果必须等于原始明文尾部（读方向 seek 生效）。
    #[tokio::test]
    async fn enc_stream_seek_aligns_like_ctr_whole_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key = [5u8; 32];
        let pkt = 88;
        let start = 123u64;
        let payload: Vec<u8> = (0..5000usize).map(|i| (i % 249) as u8).collect();
        let mut whole = payload.clone();
        CtrCipher::new(&key, pkt).apply(&mut whole);

        // 写方向：EncStream 从 start 起步，线上字节 == 整流密文的对应片段
        let (c1, mut raw_peer) = tokio::io::duplex(64 * 1024);
        let mut w = EncStream::new(c1, &key, pkt, start);
        w.write_all(&payload[start as usize..]).await.unwrap();
        w.flush().await.unwrap();
        w.shutdown().await.unwrap();
        let mut wire = Vec::new();
        raw_peer.read_to_end(&mut wire).await.unwrap();
        assert_eq!(wire, &whole[start as usize..], "续传片段的密文须与整流片段一致");

        // 读方向：同一密文喂给从 start 起步的 EncStream，必须还原明文尾部
        let (c2, mut raw_feed) = tokio::io::duplex(64 * 1024);
        let mut r = EncStream::new(c2, &key, pkt, start);
        raw_feed.write_all(&wire).await.unwrap();
        raw_feed.shutdown().await.unwrap();
        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, &payload[start as usize..], "读端 seek 后必须还原明文尾部");
    }

    /// 测试专用写端：每次 poll_write 只接受 1 字节，且「接受一次、Pending 一次」交替，
    /// 强制触发 AsyncWrite 短写/Pending 契约 —— tokio write_all 会用剩余明文切片
    /// 反复重新 poll。回环自检（duplex 全量接受）永远暴露不了这类失步。
    struct TrickleWriter {
        /// 已被底层接受的字节（即真正的线上密文）
        accepted: Vec<u8>,
        calls: u64,
    }

    impl AsyncWrite for TrickleWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.calls += 1;
            if self.calls % 2 == 0 {
                // 模拟背压：本次一个字节都不接受；立即唤醒以便下次重试
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            assert!(!buf.is_empty(), "底层不应被以空切片轮询");
            self.accepted.push(buf[0]);
            Poll::Ready(Ok(1))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// 回归：短写/Pending 下密钥流绝不能二次推进。
    /// EncStream 必须把整段新明文只加密一次并缓存密文，按底层接受量排空；
    /// 若每次 poll 都对传入切片重新 apply 密钥流，重 poll 会让 CTR 位置错乱，
    /// 线上密文不再等于整流一次性 CTR。
    #[tokio::test]
    async fn enc_stream_short_writes_keep_ctr_aligned() {
        use tokio::io::AsyncWriteExt;
        let key = [33u8; 32];
        let pkt = 909;
        let data: Vec<u8> = (0..300usize).map(|i| ((i * 31 + 17) % 254) as u8).collect();

        let sink = TrickleWriter { accepted: Vec::new(), calls: 0 };
        let mut w = EncStream::new(sink, &key, pkt, 0);
        w.write_all(&data).await.unwrap();
        w.flush().await.unwrap();

        let wire = w.inner.accepted.clone();
        assert_eq!(wire.len(), data.len(), "线上字节数必须等于明文长度");
        let mut want_ct = data.clone();
        CtrCipher::new(&key, pkt).apply(&mut want_ct);
        assert_eq!(
            wire, want_ct,
            "短写+Pending 反复重 poll 后，线上密文仍须逐字节等于整流一次性 CTR"
        );
    }

    /// 测试专用辅助：构造官方第二组合（RSA-1024 + Blowfish-128-CBC，IV=0，PKCS#7）
    /// 的报文，无签名段。仅用于验证接收端对该组合的宽容解码。
    fn seal_message_compat_rsa1024_blowfish(
        peer_pub: &RsaPublicKey,
        plain: &[u8],
    ) -> Result<String, String> {
        type BlowfishCbcEnc = cbc::Encryptor<blowfish::Blowfish>;
        use aes::cipher::{KeyIvInit, block_padding::Pkcs7};
        use aes::cipher::BlockEncryptMut;
        use rsa::Pkcs1v15Encrypt;

        let mut rng = rand::thread_rng();
        // 官方该组合的会话钥为 Blowfish-128（16 字节）
        let mut skey = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rng, &mut skey);
        let ct_key = peer_pub
            .encrypt(&mut rng, Pkcs1v15Encrypt, &skey)
            .map_err(|e| format!("会话钥加密失败：{e}"))?;
        let iv = [0u8; 8]; // Blowfish 分组 64bit，CBC IV 为 8B 全零
        let mut buf = plain.to_vec();
        let ct = BlowfishCbcEnc::new_from_slices(&skey, &iv)
            .expect("测试内 16 字节会话钥合法")
            .encrypt_padded_vec_mut::<Pkcs7>(&mut buf);
        Ok(format!(
            "{:X}:{}:{}",
            CAPA_RSA1024 | CAPA_BLOWFISH128,
            hex_lower(&ct_key),
            hex_lower(&ct)
        ))
    }
}
