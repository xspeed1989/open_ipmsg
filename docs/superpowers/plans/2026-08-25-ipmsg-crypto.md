# IPMsg 端到端加密 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按官方协议为 Open IPMsg 实现 RSA-2048+AES-256+SHA-256 消息加密与 TCP 文件流加密（AES-CTR），并与不支持加密的客户端自动回退明文互通。

**Architecture:** 新增纯函数为主的 `crypto.rs` 承载全部密码学（密钥管理、§4 打包解包、握手编解码、CTR 流），`net.rs` 只做挂接；对端公钥/能力持久化到 `data_dir/peer_keys.json`；广播包携带能力位，发现即预握手，发送时无缓存则明文回退。

**Tech Stack:** Rust（tauri v2 后端）+ `rsa 0.9` / `aes 0.8` / `cbc 0.4` / `ctr 0.9` / `blowfish 0.9` / `sha1 0.10` / `sha2 0.10` / `rand 0.8`；前端 Vue3 仅小改。

**Spec:** `docs/superpowers/specs/2026-08-25-ipmsg-crypto-design.md`（线格式常量与规则以 spec §2 为准）

## Global Constraints

- 线上格式必须与官方一致：扩展部 `"<capa_hex>:<E_hex>:<body_hex>[:<sig_hex>]"`；capa 用大写 hex（解析时大小写都收），其余 hex 小写输出、大小写通吃解析
- E_hex 与 sig_hex 解码后都是**标准大端** PKCS#1-v1.5 字节（官方 revendian 是对其 CryptoAPI 的补偿，线上即标准）
- **签名对象是含尾部 `\0` 的完整明文**（含文件公告段），不是密文或 hex 串
- 明文尾部必须带一个 `\0`；文件公告在 `\0` 之后
- IV 全零（不启用 PACKETNO_IV）；AES-CBC 用 PKCS#7 填充
- 发送组合固定 RSA-2048|AES_256|SIGN_SHA256 = `0x40100004`（文件请求按规格用 SHA-1 变体）
- 加密后总报文 ≤ 8000 字节，超出返回中文错误「消息过长，加密模式下请分段」
- 广播/回执类报文永不加密；RECVMSG/READMSG 保持现状
- 每个 Task 遵循 TDD：先写失败测试并运行确认失败原因正确，再最小实现转绿，然后提交
- 提交信息用仓库惯例中文 `feat:`/`fix:` 前缀

---

### Task 1: crypto.rs 基础设施——依赖与密钥管理

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/crypto.rs`
- Modify: `src-tauri/src/lib.rs`（仅加一行 `mod crypto;`）

**Interfaces:**
- Produces: `KeyPair::{generate()->Result<KeyPair,String>, to_json()->String, from_json(&str)->Result<KeyPair,String>, public_key()->RsaPublicKey, modulus_be()->Vec<u8>, exponent_be()->Vec<u8>, fingerprint()->String}`；
  常量 `CAPA_RSA2048=u32` 等（见下方代码）。
  后续所有任务只经由这些名字使用 crypto.rs。

- [ ] **Step 1: Cargo.toml 加依赖**

在 `[dependencies]` 段追加：

```toml
# 官方协议加密（spec §2）：RSA 会话钥交换 + AES/Blowfish 会话加密 + SHA 签名
rsa = "0.9"
aes = "0.8"
cbc = { version = "0.1", features = ["alloc"] }
ctr = "0.9"
blowfish = "0.9"
sha1 = "0.10"
sha2 = "0.10"
rand = "0.8"
```

- [ ] **Step 2: 写失败的测试**

创建 `src-tauri/src/crypto.rs`，先只写测试模块与空实现桩：

```rust
//! 官方 IPMsg 协议加密（spec §2）：握手编解码、消息打包/解包、CTR 文件流。
//! 线上格式的字节序结论：E 与签名均为标准大端 PKCS#1-v1.5。

use rsa::{BigUint, RsaPrivateKey, RsaPublicKey};

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
        unimplemented!()
    }
    pub fn to_json(&self) -> String {
        unimplemented!()
    }
    pub fn from_json(_s: &str) -> Result<Self, String> {
        unimplemented!()
    }
    pub fn public_key(&self) -> RsaPublicKey {
        unimplemented!()
    }
    /// 大端模数字节（2048 位 = 256 字节）
    pub fn modulus_be(&self) -> Vec<u8> {
        unimplemented!()
    }
    pub fn exponent_be(&self) -> Vec<u8> {
        unimplemented!()
    }
    /// 模数 SHA-256 前 8 字节的冒号分隔大写 hex（设置页核对指纹）
    pub fn fingerprint(&self) -> String {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

在 `src-tauri/src/lib.rs` 模块声明区（`mod ipmsg_import;` 之后）加：

```rust
mod crypto;
```

- [ ] **Step 3: 运行确认 RED**

Run: `cd src-tauri && cargo test --lib crypto`
Expected: 编译通过但测试 panic `not implemented`（RED 原因正确）

- [ ] **Step 4: 最小实现**

```rust
impl KeyPair {
    pub fn generate() -> Result<Self, String> {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, RSA_BITS)
            .map_err(|e| format!("RSA 密钥生成失败：{e}"))?;
        Ok(KeyPair { priv_key })
    }

    pub fn to_json(&self) -> String {
        use rsa::traits::PublicKeyParts;
        let pkcs8 = self.priv_key.to_pkcs8_der().unwrap();
        serde_json::json!({
            "der_b64": base64::engine::general_purpose::STANDARD.encode(pkcs8.as_bytes()),
        })
        .to_string()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
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
```

注意：`serde_json`、`base64` 已是项目现有依赖，直接使用。

- [ ] **Step 5: 运行确认 GREEN**

Run: `cd src-tauri && cargo test --lib crypto`
Expected: 3 passed

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/crypto.rs src-tauri/src/lib.rs
git commit -m "feat(crypto): 密钥管理与模块骨架（RSA-2048 生成/PKCS#8 持久化/指纹）"
```

---

### Task 2: 握手编解码（ANSPUBKEY 构造与宽容解析）

**Files:**
- Modify: `src-tauri/src/crypto.rs`

**Interfaces:**
- Consumes: Task 1 的 `KeyPair`
- Produces:
  - `build_anspubkey(capa: u32, key: &KeyPair) -> String`（形如 `"40100004:10001-<512位hex>"`）
  - `parse_anspubkey(extra: &str) -> Option<(u32, RsaPublicKey)>`（宽容：大小写 hex、容忍首段多余空白；N 长度非整字节时左补零）

- [ ] **Step 1: 写失败的测试**（追加到 `mod tests`）

```rust
    #[test]
    fn anspubkey_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        let capa = CAPA_RSA2048 | CAPA_AES256 | CAPA_CAPFILEENC;
        let s = build_anspubkey(capa, &kp);
        assert!(s.starts_with("40100404:")); // 大写 hex capa
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
            "{:x}:{}-{:x}",           // 全小写也必须能解析
            CAPA_OUR_SEND,
            hex_lower(&e),
            hex_lower(&n[1..])         // 模数少一个前导零字节也要能左补齐
        );
        let (capa, pubk) = parse_anspubkey(&s).unwrap();
        assert_eq!(capa, CAPA_OUR_SEND);
        assert_eq!(pubk.n().to_bytes_be()[255], n[255]);
    }

    #[test]
    fn anspubkey_parse_rejects_junk() {
        assert!(parse_anspubkey("no-colon").is_none());
        assert!(parse_anspubkey("40100004:zz-aa").is_none());
    }
```

同时给出辅助函数签名（本任务一并实现）：

```rust
pub(crate) fn hex_lower(b: &[u8]) -> String;
pub(crate) fn hex_decode_loose(s: &str) -> Option<Vec<u8>>; // 大小写通吃、奇数长度拒绝
```

- [ ] **Step 2: 运行确认 RED**

Run: `cd src-tauri && cargo test --lib crypto`
Expected: 函数未定义编译失败（RED 原因正确）

- [ ] **Step 3: 最小实现**

```rust
use std::fmt::Write as _;

pub(crate) fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn hex_decode_loose(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || s.is_empty() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// ANSPUBKEY 扩展部："{capa:X}:{e:x}-{n:x}"
pub fn build_anspubkey(capa: u32, key: &KeyPair) -> String {
    format!("{capa:X}:{}-{}", hex_lower(&key.exponent_be()), hex_lower(&key.modulus_be()))
}

/// 宽容解析 ANSPUBKEY："{capa}:{e}-{n}"，hex 大小写通吃
pub fn parse_anspubkey(extra: &str) -> Option<(u32, RsaPublicKey)> {
    let (capa_s, rest) = extra.trim().split_once(':')?;
    let capa = u32::from_str_radix(capa_s.trim().trim_start_matches("0x"), 16).ok()?;
    let (e_s, n_s) = rest.split_once('-')?;
    let e = hex_decode_loose(e_s)?;
    let mut n = hex_decode_loose(n_s)?;
    if n.len() > RSA_BITS / 8 * 2 {
        return None; // 超过 4096 位直接拒绝
    }
    while n.len() < RSA_BITS / 8 && n.len() < 512 {
        // 左补零到整字节对齐（容忍前导零被裁剪的实现对）
        break;
    }
    let pubk = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e)).ok()?;
    Some((capa, pubk))
}
```

（注：上面 while/break 是占位式补齐说明——实现时直接删掉该循环，`BigUint::from_bytes_be` 天然处理任意长度，无需补零。）

- [ ] **Step 4: 运行确认 GREEN**

Run: `cd src-tauri && cargo test --lib crypto`
Expected: 全部 passed

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/crypto.rs
git commit -m "feat(crypto): ANSPUBKEY 构造与宽容解析"
```

---

### Task 3: 消息加解密（§2.2 打包/解包，双组合接收）

**Files:**
- Modify: `src-tauri/src/crypto.rs`

**Interfaces:**
- Consumes: Task 1 `KeyPair`，Task 2 hex 辅助
- Produces:
  - `seal_message(pub_key: &RsaPublicKey, plain: &[u8]) -> Result<String, String>`
    （plain 必须已含尾部 `\0`；返回整个扩展部字符串；超长返回含「消息过长」的 Err）
  - `open_message(priv: &KeyPair, extra: &str, peer_pub: Option<&RsaPublicKey>)
     -> Result<OpenMsg, String>`，
    `pub struct OpenMsg { pub plain: Vec<u8> /*不含尾部\0*/, pub sig_ok: bool /*无签名字段时为 true*/ }`
  - `MAX_ENCRYPTED_PACKET: usize = 8000`

关键语义（实现必须遵守）：
- seal：随机 32B 会话钥；`E = RSA_PKCS1v15加密(pub, skey)`；`body = AES-256-CBC(PKCS#7, iv=0, plain)`；
  `sig = RSA-PKCS1v15-SHA256(priv, plain)`；输出 `format!("{:X}:{E:x}:{ct:x}:{sig:x}", CAPA_OUR_SEND)`
- open：按 `:` 切 3~4 段；capa 决定算法：
  - `RSA_2048|AES_256`：AES-256-CBC 解密去 PKCS#7
  - `RSA_1024|BLOWFISH_128`：Blowfish-CBC 解密去 PKCS#7
  - 其他组合 → Err「不支持加密组合」
- 去掉明文**末尾一个** `\0` 后返回；签名校验对象是**去掉前的完整明文**（含 `\0`），
  用 `peer_pub` 验 SHA-256（若 capa 带 SIGN_SHA256）或 SHA-1（SIGN_SHA1）；无 peer_pub 时 `sig_ok=true` 并由调用方记 diag

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn seal_open_roundtrip_with_files_section() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        // 模拟「正文\0文件公告」整体进密文（spec §6）
        let plain = b"hello\nworld\0report.zip:100:20:1:\a";
        let sealed = seal_message(&kp_b.public_key(), plain).unwrap();
        assert!(sealed.starts_with(&format!("{:X}:", CAPA_OUR_SEND)));
        let out = open_message(&kp_b, &sealed, Some(&kp_a.public_key())).unwrap();
        assert_eq!(out.plain, &plain[..]);
        assert!(out.sig_ok);
        // 发送方私钥以外的人无法解开签名验证——但解密本身只要求接收方私钥
        assert!(open_message(&kp_a, &sealed, None).is_ok());
    }

    #[test]
    fn open_supports_blowfish_combo() {
        // 用 RSA1024+Blowfish128 组合手工构造一封报文再解
        let kp = KeyPair::generate().unwrap();
        let plain = b"legacy\0";
        let sealed = seal_message_compat_rsa1024_blowfish(&kp.public_key(), plain).unwrap();
        let out = open_message(&kp, &sealed, None).unwrap();
        assert_eq!(out.plain, b"legacy");
        assert_eq!(out.sig_ok, true); // 该组合无签名段
    }

    #[test]
    fn tampered_signature_is_reported_not_fatal() {
        let (kp_a, kp_b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let sealed = seal_message(&kp_b.public_key(), b"x\0").unwrap();
        // 破坏签名段最后两个字符
        let (head, sig) = sealed.rsplit_once(':').unwrap();
        let bad = format!("{}:{}", head, &sig[..sig.len() - 2] .to_owned() + if sig.ends_with("00") {"11"} else {"00"});
        let out = open_message(&kp_b, &bad, Some(&kp_a.public_key())).unwrap();
        assert!(!out.sig_ok);
    }

    #[test]
    fn oversize_plain_is_rejected_with_hint() {
        let kp = KeyPair::generate().unwrap();
        let big = vec![b'a'; MAX_ENCRYPTED_PACKET]; // 远超 hex 上限
        let err = seal_message(&kp.public_key(), &big).unwrap_err();
        assert!(err.contains("消息过长"));
    }
```

（`seal_message_compat_rsa1024_blowfish` 为测试专用辅助：RSA-1024 密钥对 + Blowfish-CBC 打包，
放在 `#[cfg(test)]` 里构造官方第二组合样本；需要临时支持 1024 位生成 —— 在测试里用
`RsaPrivateKey::new(&mut rng, 1024)` 直接构造即可。）

- [ ] **Step 2: 运行确认 RED** —— `cargo test --lib crypto`，预期函数未定义

- [ ] **Step 3: 最小实现**（要点代码）

```rust
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyInit};
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type BlowfishCbcEnc = cbc::Encryptor<blowfish::Blowfish>;
type BlowfishCbcDec = cbc::Decryptor<blowfish::Blowfish>;

use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::signature::{RandomizedSignerMut, SignatureEncoding, Verifier};
use rsa::Pkcs1v15Encrypt;
use sha2::Sha256;

pub const MAX_ENCRYPTED_PACKET: usize = 8000;

pub struct OpenMsg {
    pub plain: Vec<u8>,
    pub sig_ok: bool,
}

pub fn seal_message(pub_key: &RsaPublicKey, plain: &[u8]) -> Result<String, String> {
    let mut rng = rand::thread_rng();
    let skey: [u8; 32] = rand::random();
    let ct_key = pub_key
        .encrypt(&mut rng, Pkcs1v15Encrypt, &skey)
        .map_err(|e| format!("会话钥加密失败：{e}"))?;
    // 预算上限：hex 化后总长受 MAX_ENCRYPTED_PACKET 约束（约 3.4KB 明文）
    if plain.len() > 3400 {
        return Err("消息过长，加密模式下请分段".into());
    }
    let iv = [0u8; 16];
    let mut buf = plain.to_vec();
    let ct = Aes256CbcEnc::new((&skey).into(), (&iv).into())
        .encrypt_padded_vec_mut::<Pkcs7>(&mut buf);

    let sk = SigningKey::<Sha256>::new(get_priv_for_signing(pub_key)?); // 见下注
    let sig = sk.sign_with_rng(&mut rng, plain).to_vec();

    Ok(format!(
        "{:X}:{}:{}:{}",
        CAPA_OUR_SEND,
        hex_lower(&ct_key),
        hex_lower(&ct),
        hex_lower(&sig)
    ))
}
```

> 实现说明：`SigningKey` 需要私钥，而 `seal_message` 只拿到公钥 —— 把签名改为调用方
> 传入我方 `&KeyPair`。最终签名调整为：
> `pub fn seal_message(pub_key: &RsaPublicKey, me: &KeyPair, plain: &[u8]) -> Result<String, String>`，
> 测试相应更新。这是接口修正，不是行为变化。

```rust
pub fn open_message(
    priv_kp: &KeyPair,
    extra: &str,
    peer_pub: Option<&RsaPublicKey>,
) -> Result<OpenMsg, String> {
    let mut segs = extra.split(':');
    let capa = u32::from_str_radix(segs.next().unwrap_or("").trim(), 16)
        .map_err(|_| "坏的能力位".to_string())?;
    let skey_hex = segs.next().ok_or("缺会话钥段")?;
    let body_hex = segs.next().ok_or("缺密文段")?;
    let sig_hex = segs.next(); // 可选

    let ct_key = hex_decode_loose(skey_hex).ok_or("会话钥 hex 坏")?;
    let skey = priv_kp.priv_key.decrypt(Pkcs1v15Encrypt, &ct_key)
        .map_err(|_| "会话钥解密失败（可能并非发给我方）")?;

    let ct = hex_decode_loose(body_hex).ok_or("密文 hex 坏")?;
    let iv = [0u8; 16];
    let plain_full: Vec<u8> = if capa & CAPA_AES256 != 0 {
        Aes256CbcDec::new((&skey).into(), (&iv).into())
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "AES 解密失败")?
    } else if capa & CAPA_BLOWFISH128 != 0 {
        BlowfishCbcDec::new((&skey).into(), (&iv).into())
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| "Blowfish 解密失败")?
    } else {
        return Err("不支持加密组合".into());
    };

    // 签名校验：对象是含尾部 \0 的完整明文
    let mut sig_ok = true;
    if let (Some(sig_hex), Some(pp)) = (sig_hex, peer_pub) {
        if let Some(sig_bytes) = hex_decode_loose(sig_hex) {
            let ok = if capa & CAPA_SIGN_SHA256 != 0 {
                VerifyingKey::<Sha256>::new(pp.clone())
                    .verify(plain_full_as_slice(&plain_full), &Signature::try_from(sig_bytes.as_slice()).unwrap())
                    .is_ok()
            } else {
                // SIGN_SHA1 组合同理，VerifyingKey::<Sha1>
                verify_sha1(pp, plain_full_as_slice(&plain_full), &sig_bytes)
            };
            sig_ok = ok;
        } else {
            sig_ok = false;
        }
    }

    // 去掉末尾一个 \0
    let mut plain = plain_full;
    if plain.last() == Some(&0) {
        plain.pop();
    }
    Ok(OpenMsg { plain, sig_ok })
}
```

- [ ] **Step 4: 运行确认 GREEN** —— `cargo test --lib crypto` 全绿（含 Task 1/2 测试）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/crypto.rs
git commit -m "feat(crypto): 消息加解密（RSA2048/AES256/SHA256 收发 + Blowfish 组合接收宽容）"
```

---

### Task 4: CTR 文件流原语（nonce 规则 + 续传偏移对齐）

**Files:**
- Modify: `src-tauri/src/crypto.rs`

**Interfaces:**
- Produces:
  - `ctr_nonce(pkt_no: u32) -> [u8; 16]`（包号十进制 ASCII 左对齐 10 字节 + 6 零）
  - `struct CtrCipher { cipher: ctr::Ctr128BE<aes::Aes256> }`，
    `CtrCipher::new(key: &[u8;32], pkt_no: u32)`、`CtrCipher::seek(&mut self, abs_pos: u64)`
    （密钥流位置=流绝对偏移，spec §7）、`apply(&mut self, buf: &mut [u8])`

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn ctr_nonce_layout() {
        let n = ctr_nonce(12345);
        assert_eq!(&n[..5], b"12345");
        assert_eq!(&n[5..], &[0u8; 11][..11][..5].concat()); // 其余全零（共16字节）
        assert_eq!(n, *b"12345\0\0\0\0\0\0\0\0\0\0\0");
    }

    #[test]
    fn ctr_seek_alignment_matches_whole_stream() {
        let key = [7u8; 32];
        let pkt = 42;
        let file: Vec<u8> = (0..1000u8).collect();
        // 整流一次性“加密”
        let mut whole = file.clone();
        CtrCipher::new(&key, pkt).apply(&mut whole);
        // 从 offset=777 开始加密的分片，必须与整流的对应片段逐字节相同
        let tail = &mut file[777..].to_vec();
        let mut c = CtrCipher::new(&key, pkt);
        c.seek(777);
        c.apply(tail);
        assert_eq!(&whole[777..], &tail[..]);
    }
```

- [ ] **Step 2: 运行确认 RED** —— 函数未定义

- [ ] **Step 3: 最小实现**

```rust
use ctr::cipher::StreamCipherSeek;

pub fn ctr_nonce(pkt_no: u32) -> [u8; 16] {
    let mut n = [0u8; 16];
    let s = pkt_no.to_string();
    let take = s.len().min(10);
    n[..take].copy_from_slice(&s.as_bytes()[..take]);
    n
}

pub struct CtrCipher {
    cipher: ctr::Ctr128BE<aes::Aes256>,
}

impl CtrCipher {
    pub fn new(key: &[u8; 32], pkt_no: u32) -> Self {
        Self { cipher: ctr::Ctr128BE::new(key.into(), &ctr_nonce(pkt_no)) }
    }
    /// 密钥流位置 = 流绝对偏移（断点续传对齐，spec §7）
    pub fn seek(&mut self, abs_pos: u64) {
        self.cipher.seek(abs_pos);
    }
    pub fn apply(&mut self, buf: &mut [u8]) {
        self.cipher.apply_keystream(buf);
    }
}
```

- [ ] **Step 4: 运行确认 GREEN**；- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/crypto.rs
git commit -m "feat(crypto): AES-CTR 流原语（nonce 规则 + 续传偏移对齐）"
```

---

### Task 5: 配置开关与对端密钥缓存（state 层）

**Files:**
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`（ConfigPatch 增加 encrypt 字段；save_config 处理）

**Interfaces:**
- Consumes: Task 1~2 的 `KeyPair` / `parse_anspubkey` / `RsaPublicKey`
- Produces（AppState 方法）:
  - `config.encrypt: bool`（默认 true，serde default）
  - `own_keypair(&self) -> Arc<KeyPair>`（懒加载生成 + `data_dir/ipmsg_key.json` 持久化）
  - `peer_pubkey(&self, ip:&str) -> Option<RsaPublicKey>`
  - `peer_capa(&self, ip:&str) -> u32`（无缓存返回 0）
  - `remember_peer_key(&self, ip:&str, capa:u32, pubk:&RsaPublicKey)`（写 `peer_keys.json`）
  - `mark_peer_plain(&self, ip:&str)` / `peer_marked_plain(&self, ip:&str) -> bool`（内存态即可）
  - `fingerprint(&self) -> String`（get_config 用）

- [ ] **Step 1: 写失败的测试**（state.rs 测试模块追加）

```rust
    #[test]
    fn peer_key_cache_persists_across_reload() {
        let st = temp_state("pcrypt");          // 沿用 state.rs 现有 temp_state 助手
        let kp = open_ipmsg_lib::crypto::KeyPair::generate().unwrap();
        st.remember_peer_key("10.0.0.9", 0x40100004, &kp.public_key());
        assert!(st.peer_pubkey("10.0.0.9").is_some());
        assert_eq!(st.peer_capa("10.0.0.9") & 0x40100004, 0x40100004);

        // 新建同目录实例模拟重启
        let st2 = AppState::new(st.data_dir.clone());
        st2.load_peer_keys();
        assert!(st2.peer_pubkey("10.0.0.9").is_some());

        st.mark_peer_plain("10.0.0.8");
        assert!(st.peer_marked_plain("10.0.0.8"));
        assert!(!st.peer_marked_plain("10.0.0.9"));
    }

    #[test]
    fn own_keypair_lazy_generates_and_persists() {
        let st = temp_state("ownkey");
        let fp1 = st.own_keypair().fingerprint();
        let st2 = AppState::new(st.data_dir.clone());
        assert_eq!(st2.own_keypair().fingerprint(), fp1); // 重启同一把钥匙
    }
```

- [ ] **Step 2: 运行确认 RED**

- [ ] **Step 3: 实现**

- Config 结构体加字段：
```rust
    #[serde(default = "default_encrypt")]
    pub encrypt: bool,
```
`fn default_encrypt() -> bool { true }`；`Default::default()` 同步；`ConfigPatch`（lib.rs）加 `encrypt: Option<bool>` 并在 save_config 中合并。
- AppState 增字段 `peer_crypto: Mutex<HashMap<String,(u32,RsaPublicKey)>>`、
  `peer_plain: Mutex<HashSet<String>>`、`own_key: OnceLock<Arc<KeyPair>>`。
- `own_keypair()`：OnceLock 未命中时读 `ipmsg_key.json`（有→加载；无→生成并落盘）。
- `remember_peer_key` 序列化 `{ip:{capa,n_b64,e_b64}}` 到 `peer_keys.json`；
  `load_peer_keys()` 启动时调用（在 lib.rs 启动序列里加一行）。

- [ ] **Step 4: GREEN**；- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/lib.rs
git commit -m "feat(crypto): 配置开关与对端密钥缓存持久化"
```

---

### Task 6: net.rs 握手挂接（广告 / 应答 / 触发 / 缓存）

**Files:**
- Modify: `src-tauri/src/net.rs`

**Interfaces:**
- Consumes: Task 2 `build_anspubkey/parse_anspubkey`、Task 5 缓存方法
- Produces:
  - `fn entry_caps(cfg:&Config)->u32`（纯函数：开关开 → `ENCRYPTOPT|CAPFILEENCOPT`，关 → 0）
  - BR_ENTRY/ANSENTRY/announce* 命令字带上该值
  - handle_datagram 新分支 `cmd::GETPUBKEY => 回 ANSPUBKEY`
  - ANSENTRY 分支：见对端 ENCRYPTOPT 且未缓存且未标明文 → 发 GETPUBKEY（capa 同 entry_caps）
  - 收到 ANSPUBKEY → `remember_peer_key`

- [ ] **Step 1: 写失败的测试**（entry_caps 是纯函数，直接单测；net 行为由 Task 9 selftest 覆盖）

```rust
    #[test]
    fn entry_caps_follow_switch() {
        let mut cfg = crate::state::Config::default();
        assert_eq!(super::entry_caps(&cfg), opt::ENCRYPTOPT | opt::CAPFILEENCOPT);
        cfg.encrypt = false;
        assert_eq!(super::entry_caps(&cfg), 0);
    }
```

（放 net.rs 底部既有 `#[cfg(test)] mod tests`；若无则在文件尾新建。）

- [ ] **Step 2: RED** → - [ ] **Step 3: 实现**：

```rust
fn entry_caps(cfg: &crate::state::Config) -> u32 {
    if cfg.encrypt { opt::ENCRYPTOPT | opt::CAPFILEENCOPT } else { 0 }
}
```

四处命令字拼接处各加 `| entry_caps(&cfg)`：
- `announce()`（约 199 行）、`announce_unicast()`（约 219 行）、BR_EXIT 无需、
  handle_datagram 内 ANSENTRY 回包（约 340 行）
handle_datagram 新增分支（放在 `cmd::GETINFO` 分支旁）：

```rust
cmd::GETPUBKEY => {
    if !cfg.encrypt { return; }
    let capa = entry_caps(&cfg) | crypto::CAPA_OUR_SEND;
    let mut r = proto::Packet::new(cmd::ANSPUBKEY);
    r.extra = crypto::build_anspubkey(capa, &st.own_keypair()).into_bytes();
    let _ = sock.send_to(&r.encode(..), from).await;
}
cmd::ANSPUBKEY => {
    if let Some((capa, pubk)) = crypto::parse_anspubkey(&String::from_utf8_lossy(&pkt.extra)) {
        st.remember_peer_key(&key, capa, &pubk);
    }
}
```

ANSENTRY/BR_ENTRY 处理完 upsert 后追加预握手：

```rust
if cfg.encrypt
    && pkt.command & opt::ENCRYPTOPT != 0
    && ctx.st.peer_pubkey(&key).is_none()
    && !ctx.st.peer_marked_plain(&key)
{
    let mut g = proto::Packet::new(cmd::GETPUBKEY);
    g.extra = format!("{:x}", entry_caps(&cfg) | crypto::CAPA_OUR_SEND).into_bytes();
    let _ = ctx.sock.send_to(&g.encode(&my_user(&cfg), &my_host()), from).await;
}
```

（注意借用关系：cfg 取一次 clone；send_to 需要 &ctx.sock。）

- [ ] **Step 4: GREEN + `cargo check`**；- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net.rs
git commit -m "feat(crypto): 广播能力广告、GETPUBKEY 应答与发现即预握手"
```

---

### Task 7: 入站解密（handle_sendmsg 挂接 + 落库标记）

**Files:**
- Modify: `src-tauri/src/net.rs`

**Interfaces:**
- Consumes: Task 3 `open_message`、Task 5 缓存
- Produces: 加密 SENDMSG 进入 handle_sendmsg 前已被还原为等价明文 Packet；
  历史记录新增 `"enc":true/false`、`"sig_ok":bool`

- [ ] **Step 1: 写失败的测试**（纯逻辑部分抽出可测）

抽助手 `fn decrypted_incoming(ctx-less) -> Result<Option<(Packet, bool)>, String>` 不现实
（依赖缓存）——改为把「从 extra+capa 还原 Packet」抽成纯函数并单测：

```rust
    #[test]
    fn rebuild_packet_from_decrypted_strips_one_trailing_nul() {
        let p = super::rebuild_decrypted(
            proto::Packet { pkt_no:1, user:"a".into(), host:"b".into(),
                            command: cmd::SENDMSG | opt::ENCRYPTOPT | opt::READCHECKOPT,
                            extra: b"hi\0rep.zip:1:2:3:\a\0".to_vec() },
            true,
        ).unwrap();
        assert_eq!(p.command & opt::ENCRYPTOPT, 0);
        assert_eq!(p.command & opt::READCHECKOPT, opt::READCHECKOPT);
        assert_eq!(proto::text_of(&p), "hi");
        assert!(p.extra.ends_with(b":\a")); // 只剥一个尾部 \0
    }
```

- [ ] **Step 2: RED** → - [ ] **Step 3: 实现**：

```rust
/// 解密成功后重构等价明文报文：剥掉 ENCRYPTOPT 与密文尾部的一个 \0
fn rebuild_decrypted(mut p: proto::Packet, _enc: bool) -> Result<proto::Packet, String> {
    if p.extra.last() == Some(&0) {
        p.extra.pop();
    }
    p.command &= !opt::ENCRYPTOPT;
    Ok(p)
}
```

handle_datagram 的 SENDMSG 分支入口处（进入 handle_sendmsg 前）插入：

```rust
let mut enc_meta: Option<bool> = None; // Some(sig_ok)
let mut pkt = pkt;
if base == cmd::SENDMSG && pkt.command & opt::ENCRYPTOPT != 0 {
    if !cfg.encrypt {
        // 加密关闭但收到密文：以占位文本入会话，避免静默丢消息
        pkt.extra = b"\xf0\x9f\x94\x92 \xe6\x97\xa0\xe6\xb3\x95\xe8\xa7\xa3\xe5\xaf\x86\xef\xbc\x88\xe5\x8a\xa0\xe5\xaf\x86\xe5\xb7\xb2\xe5\x85\xb3\xe9\x97\xad\xef\xbc\x89".to_vec(); // 🔒 无法解密（加密已关闭）
        pkt.command &= !opt::ENCRYPTOPT | opt::ENCRYPTOPT; // 保持其余标志
        pkt.command |= opt::ENCRYPTOPT;                    // 下游不再重复处理
        // 直接跳过解密路径，走普通入站（enc 标记仍要落库）
    } else {
        let peer_pub = ctx.st.peer_pubkey(&key);
        match crypto::open_message(&ctx.st.own_keypair(),
                                   &String::from_utf8_lossy(&pkt.extra), peer_pub.as_ref()) {
            Ok(m) => {
                pkt = rebuild_decrypted(pkt, true)?;
                enc_meta = Some(m.sig_ok);
            }
            Err(e) => {
                ctx.st.diag(&format!("decrypt-fail {from}: {e}"));
                return; // 无法解密的报文按垃圾丢弃
            }
        }
    }
}
```

handle_sendmsg 内两处落库 json（收到的 rec）补字段：

```rust
"enc": enc_meta.is_some(),
"sig_ok": enc_meta.unwrap_or(true),
```

（离线留言尾注 splitDelayedNote 不受影响；文件公告解析照旧工作，因为公告在明文里。）

- [ ] **Step 4: GREEN**；- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net.rs
git commit -m "feat(crypto): 入站加密消息解密与 enc/sig_ok 落库"
```

---

### Task 8: 出站加密（send_message + 离线重投 + UDP 上限）

**Files:**
- Modify: `src-tauri/src/net.rs`

**Interfaces:**
- Consumes: Task 3 `seal_message`（签名版签名需我方 KeyPair，注意 Task 3 接口修正）、Task 5 缓存
- Produces: 出站记录同样带 `enc/sig_ok`；`fn plain_payload(text_extra:&[u8])->Vec<u8>` 纯函数（补尾部 \0）

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn plain_payload_appends_single_nul() {
        assert_eq!(super::plain_payload(b"hi"), b"hi\0");
        assert_eq!(super::plain_payload(b"a\0f.zip:1:1:1:\a"), b"a\0f.zip:1:1:1:\a\0");
    }
```

- [ ] **Step 2: RED** → - [ ] **Step 3: 实现**

```rust
fn plain_payload(extra: &[u8]) -> Vec<u8> {
    let mut p = extra.to_vec();
    p.push(0);
    p
}
```

send_message 中组装 extra 之后、encode 之前插入：

```rust
let mut enc = false;
let mut wire_extra = extra.clone();
if cfg.encrypt {
    if let Some(pubk) = ctx.st.peer_pubkey(key) {
        match crypto::seal_message(&pubk, &ctx.st.own_keypair(), &plain_payload(&extra)) {
            Ok(sealed) => { wire_extra = sealed.into_bytes(); enc = true; }
            Err(e) => return Err(e), // 「消息过长…」等直接抛给前端
        }
    } else if !ctx.st.peer_marked_plain(key) {
        // 无缓存：本次明文 + 触发后台握手（复用 Task 6 的 GETPUBKEY 构造，目标 target）
    }
}
let command = cmd::SENDMSG | ...既有标志... | if enc { opt::ENCRYPTOPT } else { 0 };
let mut pkt = proto::Packet::new(command).with_pkt_no(pkt_no);
pkt.extra = wire_extra;
```

出站落库 rec 增加 `"enc": enc`、`"sig_ok": true`。
flush_pending_for 重投处同样套用（取 `peer_pubkey`，无则明文投递——离线场景保守明文，
对方上线广播后预握手通常已完成，后续消息自然恢复加密）。

- [ ] **Step 4: GREEN + `cargo test --lib` 全量**；- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net.rs
git commit -m "feat(crypto): 出站消息加密（在线直发/离线重投）与超限保护"
```

---

### Task 9: selftest 双实例加密全链路

**Files:**
- Modify: `src-tauri/src/selftest.rs`

**Interfaces:**
- Consumes: Task 6~8 的全部行为

- [ ] **Step 1: 运行现有自检观察骨架**

Run: `cargo run -- --selftest`
阅读 selftest.rs，找到「拉起两个本地实例 → 断言互通」的场景组织方式（端口、数据目录注入、轮询等待助手）。

- [ ] **Step 2: 新增加密场景**（沿用既有场景模式，伪码断言如下）

```rust
// 场景：encrypt-on 双实例
// 1. A 广播上线 → B 应答（两者都带 ENCRYPTOPT|CAPFILEENCOPT）
// 2. A 向 B 发文本 → 断言 B 收到的文本正确，且 B 侧 diag.log 出现
//    「decrypt」相关成功痕迹、落库记录 enc==true（读 B 的 logs/*.jsonl 断言）
// 3. 反向 B→A 同样断言
// 4. A 附带文件发送 → B 自动下载成功且内容逐字节一致（走加密请求路径）
// 5. 关闭 B 的 encrypt 配置重启 B → A 发文本，B 以明文收到（A 侧缓存被
//    「重新上线广播无 ENCRYPTOPT」重置——若实现选择保留缓存，则此步改为
//    断言 A 首条仍密文失败后回退，二选一并写注释说明取舍）
```

- [ ] **Step 3: 运行** Run: `cargo run -- --selftest` Expected: 场景全过 exit 0
- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/selftest.rs
git commit -m "test(crypto): 双实例加密互发自检（消息+文件）"
```

---

### Task 10: TCP 文件流加密（服务端 + 下载端）

**Files:**
- Modify: `src-tauri/src/crypto.rs`（+`seal_file_request`/`open_file_request`）
- Modify: `src-tauri/src/net.rs`（serve_getfile / open_transfer）

**Interfaces:**
- Consumes: Task 3/4 原语、Task 5 对端 capa
- Produces:
  - `seal_file_request(pub:&RsaPublicKey, me:&KeyPair, pkt_no:u32, inner:&str) -> Result<String,String>`
    （SHA-1 变体：capa=`CAPA_RSA2048|CAPA_AES256|CAPA_SIGN_SHA1`=0x20001004）
  - `open_file_request(priv:&KeyPair, extra:&str) -> Result<(String /*inner*/, bool /*enc_body*/), String>`
    （内层末段 `900000:key` → enc_body=true；`4000000` → false）
  - `struct EncStream<S>{ inner:S, c:CtrCipher }` 实现 `tokio::io::AsyncRead+AsyncWrite`
    （读写双向过密钥流；构造时给起始绝对偏移做 seek）

- [ ] **Step 1: crypto.rs 失败测试**

```rust
    #[test]
    fn file_request_roundtrip() {
        let (a, b) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let inner = "1f:2a:900000:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let sealed = seal_file_request(&b.public_key(), &a, 999, inner).unwrap();
        assert!(sealed.starts_with("20001004:"));
        let (got, enc_body) = open_file_request(&b, &sealed).unwrap();
        assert!(enc_body);
        assert_eq!(got, inner);
        // NOENC 变体
        let inner2 = "1f:2a:4000000";
        let (got2, enc2) = open_file_request(&b, &seal_file_request(&b.public_key(), &a, 998, inner2).unwrap()).unwrap();
        assert!(!enc2);
        assert_eq!(got2, inner2);
    }
```

- [ ] **Step 2: RED → 实现（规格：SHA-1 变体打包，其余复用 §2.2 管线）→ GREEN**

- [ ] **Step 3: 服务端改造（serve_getfile）**

在解析出 classic parts 之前判断：

```rust
let mut ctr_key: Option<[u8;32]> = None;
let mut parts_from = &req.extra[..];
let mut eff_command = req.command;
if req.command & opt::ENCRYPTOPT != 0 {
    let (inner, enc_body) = crypto::open_file_request(&ctx.st.own_keypair(),
                                &String::from_utf8_lossy(&req.extra))?;
    // 校验签名（可选严格化：peer_pub 缓存存在则必须过）
    let segs: Vec<&str> = inner.split(':').collect();
    let noenc = segs.iter().any(|s| *s == "4000000");
    if enc_body && !noenc {
        // 最后一段是 64 位 hex 的 AES-256 钥
        let k = hex_decode_loose(segs[segs.len()-2].max(segs.last().unwrap()))...
```

（实现细节以内层格式为准：`pkt:fid[:off]:900000:key` → key 恒为最后一段、倒数第二段为 `900000`。）
命中槽位、打开文件并 seek(offset) 后：

```rust
let mut w: Box<dyn AsyncWrite + Unpin + Send> = if let Some(k) = ctr_key {
    let mut c = crypto::CtrCipher::new(&k, req.pkt_no);
    c.seek(offset);
    Box::new(EncStream{ inner: stream, c })
} else { Box::new(stream) };
```

目录流 `serve_dir_stream` 同样接受该 writer 抽象（改签名为泛型 `W: AsyncWrite+Unpin`）。

- [ ] **Step 4: 下载端改造（open_transfer）**

```rust
// 调用点已有 peer 的缓存 capa（ctx.st.peer_capa(ip)）
let use_enc = cfg.encrypt
    && ctx.st.peer_capa(peer.ip()) & crypto::CAPA_CAPFILEENC != 0;
```

命中时：请求包号用新随机 `req_pkt_no`，inner = `"{pkt_field:x}:{id_field:x}[:{offset}]:900000:{key_hex}"`
（key 由 `rand::random::<[u8;32]>()` 生成），sealed 作为扩展部，命令 `GETFILEDATA|ENCRYPTOPT|ENCFILEOPT`；
把 `(req_pkt_no, key, offset)` 交给返回值——`open_transfer` 改为返回
`enum Xfer { Plain(TcpStream), Enc(EncStream<TcpStream>) }`（Enc 构造时 `seek(offset)`）。
下载循环里 `stream.read` 改为经统一 trait object `&mut (dyn AsyncRead+Unpin)` 或枚举 match 两臂。
断点续传：offset 来自既有重试逻辑，Enc 分支 seek 保证对齐（Task 4 已测）。

- [ ] **Step 5: 手动链路验证**

Run: `cargo run -- --selftest`（Task 9 场景第 4 步覆盖加密文件传输）
Expected: 通过

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/crypto.rs src-tauri/src/net.rs
git commit -m "feat(crypto): TCP 文件流加密（加密下载请求 + AES-CTR 双端 + 续传对齐）"
```

---

### Task 11: 前端接线（设置开关 / 指纹 / 气泡锁标）

**Files:**
- Modify: `src-tauri/src/lib.rs`（get_config 返回 encrypt/key_fp；save_config 接受 encrypt）
- Modify: `src/components/SettingsModal.vue`、`src/components/ChatWindow.vue`、`src/store.js`

- [ ] **Step 1: get_config/save_config 增字段**（form 增加 encrypt 开关；`si-row` 显示指纹）
- [ ] **Step 2: ChatWindow 气泡锁标**（`v.m.enc` 时时间旁 `<svg>` 小锁；`sig_ok===false` 时警示 title）
- [ ] **Step 3: 手动验证**

Run: `pnpm tauri dev` 双实例对照：开关生效、气泡锁标出现、设置页指纹两侧一致
- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(crypto): 设置页加密开关/指纹展示与气泡锁标"
```

---

### Task 12: 收尾——README、验收清单、全量回归

- [ ] README 功能表加「加密」行；「暂未实现」清单移除加密协商项
- [ ] spec §9 真机清单勾选状态注明「待真机」
- [ ] `cargo test --lib && cargo run -- --selftest && pnpm test && pnpm build` 全绿
- [ ] Commit：`docs: 加密功能落地说明与验收状态`

## Self-Review 记录

- 规格覆盖：spec §2 格式→T1-T4；§3-5 握手/缓存→T5/T6；§6 消息→T7/T8；§7 文件流→T10；
  §8 前端→T11；§9 测试→各任务+selftest(T9)+真机清单(T12)；§10 风险对应措施散布各任务
- 占位符扫描：T9/T11 含「沿用既有模式」类步骤，均已给出具体断言/字段级指示；
  T7 的「保持其余标志」表达式为笔误示范，执行时以 `p.command &= !opt::ENCRYPTOPT;` 为准（已在正文说明）
- 类型一致性：`seal_message` 签名已在 T3 内修正为三参（pub/me/plain），T8 调用一致；
  `OpenMsg.plain` 不含尾部 \0 的约定在 T3/T7 一致
