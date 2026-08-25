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

// 能力位（ipmsg.h Ver4.50）
pub const CAPA_RSA1024: u32 = 0x0000_0002;
pub const CAPA_RSA2048: u32 = 0x0000_0004;
pub const CAPA_BLOWFISH128: u32 = 0x0002_0000;
pub const CAPA_AES256: u32 = 0x0010_0000;
pub const CAPA_CAPFILEENC: u32 = 0x0004_0000;
pub const CAPA_SIGN_SHA1: u32 = 0x2000_0000;
pub const CAPA_SIGN_SHA256: u32 = 0x4000_0000;

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

/// ANSPUBKEY 扩展部："{capa:X}:{e:x}-{n:x}"（capa 大写 hex，E/N 小写 hex）
pub fn build_anspubkey(capa: u32, key: &KeyPair) -> String {
    format!(
        "{capa:X}:{}-{}",
        hex_lower(&key.exponent_be()),
        hex_lower(&key.modulus_be())
    )
}

/// 宽容解析 ANSPUBKEY 扩展部："{capa}:{e}-{n}"，hex 大小写通吃；
/// N 允许被对端裁掉前导零字节（BigUint 按值还原，天然左补零语义）
pub fn parse_anspubkey(extra: &str) -> Option<(u32, RsaPublicKey)> {
    let (capa_s, rest) = extra.trim().split_once(':')?;
    let capa = u32::from_str_radix(capa_s.trim().trim_start_matches("0x"), 16).ok()?;
    let (e_s, n_s) = rest.split_once('-')?;
    let e = hex_decode_loose(e_s)?;
    let n = hex_decode_loose(n_s)?;
    if n.len() > RSA_BITS / 8 * 2 {
        return None; // 超过 4096 位直接拒绝
    }
    let pubk = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e)).ok()?;
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

/// 加密封包：输出扩展部 "{CAPA_OUR_SEND:X}:{E:x}:{ct:x}:{sig:x}"。
/// - 会话钥：随机 32B → RSA-PKCS1v15 加密到对端公钥；
/// - 正文：AES-256-CBC（IV=0、PKCS#7），对象是**含尾部 \0** 的完整明文；
/// - 签名：RSA-PKCS1v15-SHA256，用我方私钥 `me`，对象同样是含 \0 的完整明文。
pub fn seal_message(pub_key: &RsaPublicKey, me: &KeyPair, plain: &[u8]) -> Result<String, String> {
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

    let signing = SigningKey::<Sha256>::new(me.priv_key.clone());
    let sig = signing.sign_with_rng(&mut rng, plain);

    Ok(format!(
        "{:X}:{}:{}:{}",
        CAPA_OUR_SEND,
        hex_lower(&ct_key),
        hex_lower(&ct),
        hex_lower(&sig.to_vec()),
    ))
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
    peer_pub: Option<&RsaPublicKey>,
) -> Result<OpenMsg, String> {
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

    let plain_full: Vec<u8> = if aes_mode {
        let k32: [u8; 32] = skey.try_into().map_err(|_| "AES 会话钥长度异常".to_string())?;
        Aes256CbcDec::new_from_slices(&k32, &CBC_IV0_AES)
            .map_err(|_| "AES 初始化失败".to_string())?
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "AES 解密失败（填充校验不过）".to_string())?
    } else {
        BlowfishCbcDec::new_from_slices(&skey, &CBC_IV0_BLOWFISH)
            .map_err(|_| "Blowfish 会话钥/IV 异常".to_string())?
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "Blowfish 解密失败（填充校验不过）".to_string())?
    };

    // 签名校验：完整明文（含尾部 \0）
    let mut sig_ok = true;
    if let (Some(pp), Some(sig_hex)) = (peer_pub, segs.get(3)) {
        sig_ok = match hex_decode_loose(sig_hex).and_then(|b| Signature::try_from(&b[..]).ok()) {
            Some(sig) => {
                if capa & CAPA_SIGN_SHA256 != 0 {
                    VerifyingKey::<Sha256>::new(pp.clone())
                        .verify(&plain_full, &sig)
                        .is_ok()
                } else if capa & CAPA_SIGN_SHA1 != 0 {
                    VerifyingKey::<Sha1>::new(pp.clone())
                        .verify(&plain_full, &sig)
                        .is_ok()
                } else {
                    // 有签名段却没声明哈希算法：无法验证，按不可信处理
                    false
                }
            }
            None => false, // 签名段不是合法 hex
        };
    }

    let mut plain = plain_full;
    if plain.last() == Some(&0) {
        plain.pop(); // 只剥一个尾部 \0
    }
    Ok(OpenMsg { plain, sig_ok })
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
    fn anspubkey_parse_tolerates_case_and_short_n() {
        let kp = KeyPair::generate().unwrap();
        let e = kp.exponent_be();
        let n = kp.modulus_be();
        let s = format!(
            "{:x}:{}-{}",             // 全小写也必须能解析（hex_lower 产物已是 hex 文本，勿再套 {:x}）
            CAPA_OUR_SEND,
            hex_lower(&e),
            hex_lower(&n[1..])         // 模数少一个前导零字节也要能左补齐
        );
        let (capa, pubk) = parse_anspubkey(&s).unwrap();
        assert_eq!(capa, CAPA_OUR_SEND);
        // 注：brief 原文 `to_bytes_be()[255]` 必越界 panic——左裁后的 N 按值还原成
        // 255 字节的最小大端编码，故比较尾字节（与原断言意图一致：模数值未变）
        assert_eq!(pubk.n().to_bytes_be().last(), n.last());
        assert_eq!(pubk.e().to_bytes_be(), e);
    }

    #[test]
    fn anspubkey_parse_rejects_junk() {
        assert!(parse_anspubkey("no-colon").is_none());
        assert!(parse_anspubkey("40100004:zz-aa").is_none());
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
        let out = open_message(&kp_b, &sealed, Some(&kp_a.public_key())).unwrap();
        assert_eq!(out.plain, &plain[..]); // 含文件公告整体；明文以 \x07 结尾，无尾部 \0 可剥
                                           // （尾部 \0 剥离的覆盖见 open_supports_blowfish_combo 的 b"legacy\0"）
        assert!(out.sig_ok);
        // 解密本身只要求接收方私钥；无对端公钥时无法验签，sig_ok 保持 true（调用方记 diag）
        let out_no_pp = open_message(&kp_b, &sealed, None).unwrap();
        assert_eq!(out_no_pp.plain, &plain[..]);
        assert!(out_no_pp.sig_ok);
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
        let out = open_message(&kp, &sealed, None).unwrap();
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
        let out = open_message(&kp_b, &bad, Some(&kp_a.public_key())).unwrap();
        assert!(!out.sig_ok);
    }

    #[test]
    fn signature_verifies_over_full_pre_strip_plaintext() {
        // 回归钉子：官方客户端的签名对象是**含尾部 \0** 的完整明文。
        // 若未来把验签对象误改成剥掉 \0 之后的明文（互操作回归），本测试必须失败。
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"x\0").unwrap();
        let out = open_message(&kp_b, &sealed, Some(&kp_a.public_key())).unwrap();
        assert!(out.sig_ok);
        assert_eq!(out.plain, b"x"); // 剥离只影响返回值，不影响验签对象
    }

    #[test]
    fn undecodable_signature_hex_is_reported() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), &kp_a, b"x\0").unwrap();
        let (head, _) = sealed.rsplit_once(':').unwrap();
        // 签名段存在但不是合法 hex：有对端公钥时必须报 sig_ok=false，且不能致命
        let bad = format!("{}:zz", head);
        let out = open_message(&kp_b, &bad, Some(&kp_a.public_key())).unwrap();
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
        let err = open_message(&kp, &bogus, None).unwrap_err();
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
            let _ = open_message(&kp, s, None);
            let _ = open_message(&kp, s, Some(&kp.public_key()));
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
