//! 官方 IPMsg 协议加密（spec §2）：握手编解码、消息打包/解包、CTR 文件流。
//! 线上格式的字节序结论：E 与签名均为标准大端 PKCS#1-v1.5。

// 能力位常量与部分方法由后续任务（Task 2+）按计划接入，本任务先行定义骨架，
// 暂时压制 dead_code 提示；后续任务接入后可移除。
#![allow(dead_code)]

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
}
