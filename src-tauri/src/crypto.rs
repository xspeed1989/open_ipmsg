//! 官方 IPMsg 协议加密（spec §2）：握手编解码、消息打包/解包、CTR 文件流。
//! 线上格式的字节序结论：E 与签名均为标准大端 PKCS#1-v1.5。

// 能力位常量与部分方法由后续任务（Task 2+）按计划接入，本任务先行定义骨架，
// 暂时压制 dead_code 提示；后续任务接入后可移除。
#![allow(dead_code)]

use rsa::{RsaPrivateKey, RsaPublicKey};

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
