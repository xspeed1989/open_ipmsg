//! IPDict 序列化器（IP Messenger Next Generation 格式）。
//!
//! 官方定义（src/TLib/ipdict.h Ver4.50）：
//! ```text
//! Full:    "IP2:(contents_hexlen):(contents):Z"
//! Content: "(key):(value_hexlen):(value)" 依次拼接
//! int:     "(int_hex_string)"，负数加 '-' 前缀
//! str:     "(string_as_utf8)"
//! bytes:   "(bytes_as_bytes)"
//! list:    "(item1_hexlen):(item1_value):(item2_hexlen):(item2_value)…"
//! dict:    "(item1_key):(item1_hexlen):(item1_value):(item2_key):…"
//! ipdict:  嵌套 Full 格式
//! ```
//! 格式本身无类型信息：所有字段先按接收顺序保存原始字节，调用方通过
//! `get_int`、`get_str`、`get_bytes`、`get_dict` 或 `get_dict_list` 显式解码。

use serde::Serialize;

pub const IPDICT_HEAD: &str = "IP2:";
pub const IPDICT_FOOT: &str = ":Z";

/* 官方键名（ipmsg.h New Protocol Key，DIR 系列 / ANSLIST_DICT 共用） */
pub const DICT_VER: &str = "VER";
pub const DICT_PKT: &str = "PKT";
pub const DICT_DATE: &str = "DATE";
pub const DICT_UID: &str = "UID";
pub const DICT_HID: &str = "HID";
pub const DICT_CMD: &str = "CMD";
pub const DICT_FLG: &str = "FLG";
pub const DICT_CVER: &str = "CVER";
pub const DICT_GRP: &str = "GRP";
pub const DICT_NCK: &str = "NCK";
pub const DICT_STAT: &str = "STAT";
pub const DICT_START: &str = "START";
pub const DICT_TOTAL: &str = "TOTAL";
pub const DICT_NUM: &str = "NUM";
pub const DICT_HLST: &str = "HLST";
pub const DICT_IPAD: &str = "IPAD";
pub const DICT_PORT: &str = "PORT";
pub const DICT_NADDRS: &str = "NADRS";
pub const DICT_ADDR: &str = "ADR";
pub const DICT_MASK: &str = "MASK";
pub const DICT_AGS: &str = "AGS";
pub const DICT_DIRECT: &str = "DRCT";
pub const DICT_TARG: &str = "TADR";
pub const DICT_SIGN: &str = "SIGN";
pub const DICT_PUBE: &str = "PUBE";
pub const DICT_PUBN: &str = "PUBN";
pub const DICT_EF: &str = "EF";
pub const DICT_EC: &str = "EC";
/// 官方签名标志：SIGN_SHA256 = 0x40000000（DIR 主签名必用）
pub const DICT_EF_SHA256: i64 = 0x4000_0000;
/// v5 密文消息（EncIPDict）与消息体字段
pub const DICT_ENCIV: &str = "EI";
pub const DICT_ENCKEY: &str = "EK";
pub const DICT_ENCBODY: &str = "EB";
pub const DICT_BODY: &str = "BODY";
pub const DICT_FILE: &str = "FILE";
pub const DICT_FID: &str = "FI";
pub const DICT_FNAME: &str = "FN";
pub const DICT_FSIZE: &str = "FS";
pub const DICT_MTIME: &str = "MT";
pub const DICT_FATTR: &str = "FA";
pub const DICT_CLIPPOS: &str = "CP";
/// 官方 EncIPDict 的 EF 组合：RSA2048|AES256|IPDICT_CTR = 0x500004
pub const ENCIPDICT_EF: i64 = 0x0050_0004;

/// 有序字典。每个值都保留为收到或写入时的原始字节，类型由 getter 决定。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Dict {
    pub items: Vec<(String, Vec<u8>)>,
}

impl Dict {
    pub fn new() -> Self {
        Self::default()
    }

    fn set_raw(&mut self, key: &str, value: Vec<u8>) -> &mut Self {
        if let Some((_, current)) = self.items.iter_mut().find(|(k, _)| k == key) {
            *current = value;
        } else {
            self.items.push((key.to_string(), value));
        }
        self
    }

    pub fn put_int(&mut self, key: &str, value: i64) -> &mut Self {
        let raw = if value < 0 {
            format!("-{:x}", value.unsigned_abs()).into_bytes()
        } else {
            format!("{value:x}").into_bytes()
        };
        self.set_raw(key, raw)
    }

    pub fn put_str(&mut self, key: &str, value: &str) -> &mut Self {
        self.set_raw(key, value.as_bytes().to_vec())
    }

    pub fn put_bytes(&mut self, key: &str, value: &[u8]) -> &mut Self {
        self.set_raw(key, value.to_vec())
    }

    /// dict 值只包含内容段；完整 IP2 外壳属于单独的 ipdict 值类型。
    pub fn put_dict(&mut self, key: &str, sub: &Dict) -> &mut Self {
        self.set_raw(key, sub.pack_content_prefix(sub.items.len()))
    }

    pub fn put_dict_list(&mut self, key: &str, list: &[Dict]) -> &mut Self {
        let mut raw = Vec::new();
        for (index, dict) in list.iter().enumerate() {
            if index > 0 {
                raw.push(b':');
            }
            let item = dict.pack_content_prefix(dict.items.len());
            raw.extend_from_slice(format!("{:x}:", item.len()).as_bytes());
            raw.extend_from_slice(&item);
        }
        self.set_raw(key, raw)
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_slice())
    }

    pub fn get_bytes(&self, key: &str) -> Option<&[u8]> {
        self.get(key)
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        let raw = self.get(key)?;
        let (negative, digits) = match raw.first() {
            Some(b'-') => (true, &raw[1..]),
            _ => (false, raw),
        };
        if digits.is_empty() || digits.len() > 16 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let value = u64::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
        if negative {
            let min_magnitude = i64::MAX as u64 + 1;
            if value > min_magnitude {
                return None;
            }
            if value == min_magnitude {
                Some(i64::MIN)
            } else {
                Some(-(value as i64))
            }
        } else if value <= i64::MAX as u64 {
            Some(value as i64)
        } else {
            None
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        std::str::from_utf8(self.get(key)?).ok()
    }

    pub fn get_dict(&self, key: &str) -> Option<Dict> {
        parse_content(self.get(key)?)
    }

    pub fn get_dict_list(&self, key: &str) -> Vec<Dict> {
        self.get(key)
            .and_then(parse_dict_list)
            .unwrap_or_default()
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Serialize the full `IP2:(len):(content):Z` packet with an ordered item prefix.
    pub fn pack_prefix(&self, max_items: usize) -> Vec<u8> {
        let content = self.pack_content_prefix(max_items.min(self.items.len()));
        let mut out = format!("IP2:{:x}:", content.len()).into_bytes();
        out.extend_from_slice(&content);
        out.extend_from_slice(IPDICT_FOOT.as_bytes());
        out
    }

    /// Serialize the full `IP2:(len):(content):Z` packet.
    pub fn pack(&self) -> Vec<u8> {
        self.pack_prefix(self.items.len())
    }

    fn pack_content_prefix(&self, max_items: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, (key, value)) in self.items.iter().take(max_items).enumerate() {
            if index > 0 {
                out.push(b':');
            }
            out.extend_from_slice(key.as_bytes());
            out.extend_from_slice(format!(":{:x}:", value.len()).as_bytes());
            out.extend_from_slice(value);
        }
        out
    }

    /// Parse a full packet and return the consumed byte count.
    pub fn unpack(data: &[u8]) -> Option<(Dict, usize)> {
        if !data.starts_with(IPDICT_HEAD.as_bytes()) {
            return None;
        }
        let mut index = IPDICT_HEAD.len();
        let len_start = index;
        while index < data.len() && data[index] != b':' {
            index += 1;
        }
        if index == len_start || index == data.len() {
            return None;
        }
        let content_len = parse_hex_len(&data[len_start..index])?;
        index += 1;
        let content_end = index.checked_add(content_len)?;
        let total = content_end.checked_add(IPDICT_FOOT.len())?;
        if total > data.len() || &data[content_end..total] != IPDICT_FOOT.as_bytes() {
            return None;
        }
        Some((unpack_content(&data[index..content_end])?, total))
    }
}

/// Serialize a content segment without the IP2 envelope.
pub fn pack_content(d: &Dict) -> Vec<u8> {
    d.pack_content_prefix(d.items.len())
}

/// Parse an exact content segment without stripping an envelope or padding.
pub fn unpack_content(data: &[u8]) -> Option<Dict> {
    parse_content(data)
}

fn parse_hex_len(raw: &[u8]) -> Option<usize> {
    if raw.is_empty() || !raw.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    usize::from_str_radix(std::str::from_utf8(raw).ok()?, 16).ok()
}

/// Parse exact `key:len:value` entries with official inter-entry colons.
fn parse_content(data: &[u8]) -> Option<Dict> {
    let mut dict = Dict::new();
    let mut index = 0usize;
    let mut first = true;

    while index < data.len() {
        if !first {
            if data[index] != b':' {
                return None;
            }
            index += 1;
            if index == data.len() {
                return None;
            }
        }
        first = false;

        let key_start = index;
        while index < data.len() && data[index] != b':' {
            index += 1;
        }
        if index == key_start || index == data.len() {
            return None;
        }
        let key = std::str::from_utf8(&data[key_start..index]).ok()?;
        index += 1;

        let len_start = index;
        while index < data.len() && data[index] != b':' {
            index += 1;
        }
        if index == len_start || index == data.len() {
            return None;
        }
        let value_len = parse_hex_len(&data[len_start..index])?;
        index += 1;
        let value_end = index.checked_add(value_len)?;
        if value_end > data.len() {
            return None;
        }
        dict.set_raw(key, data[index..value_end].to_vec());
        index = value_end;
    }
    Some(dict)
}

/// Parse exact `len:dict-content` list items with required inter-item colons.
fn parse_dict_list(data: &[u8]) -> Option<Vec<Dict>> {
    let mut list = Vec::new();
    let mut index = 0usize;
    let mut first = true;

    while index < data.len() {
        if !first {
            if data[index] != b':' {
                return None;
            }
            index += 1;
            if index == data.len() {
                return None;
            }
        }
        first = false;

        let len_start = index;
        while index < data.len() && data[index] != b':' {
            index += 1;
        }
        if index == len_start || index == data.len() {
            return None;
        }
        let item_len = parse_hex_len(&data[len_start..index])?;
        index += 1;
        let item_end = index.checked_add(item_len)?;
        if item_end > data.len() {
            return None;
        }
        list.push(parse_content(&data[index..item_end])?);
        index = item_end;
    }
    Some(list)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encipdict_ef_matches_official_rsa2048_aes256_ctr_bits() {
        assert_eq!(ENCIPDICT_EF, 0x0050_0004);
    }

    #[test]
    fn unpack_keeps_values_raw_until_the_requested_getter() {
        let wire = b"IP2:20:VER:1:3:BODY:7:1111111:BIN:3:\x00:\xff:Z";
        let (d, used) = Dict::unpack(wire).expect("official IPDict");

        assert_eq!(used, wire.len());
        assert_eq!(d.get_int("VER"), Some(3));
        assert_eq!(d.get_str("BODY"), Some("1111111"));
        assert_eq!(d.get_int("BODY"), Some(0x1111111));
        assert_eq!(d.get_bytes("BIN"), Some(&b"\x00:\xff"[..]));
        assert_eq!(d.pack(), wire);
    }

    #[test]
    fn empty_text_is_a_valid_string_value() {
        let wire = Dict::new().put_str(DICT_BODY, "").pack();
        let (d, used) = Dict::unpack(&wire).expect("empty BODY");

        assert_eq!(used, wire.len());
        assert_eq!(d.get_str(DICT_BODY), Some(""));
        assert_eq!(d.get_bytes(DICT_BODY), Some(&b""[..]));
    }

    #[test]
    fn official_dict_list_uses_colons_between_items() {
        let mut a = Dict::new();
        a.put_str("UID", "a");
        let mut b = Dict::new();
        b.put_str("UID", "b");
        let wire = Dict::new().put_dict_list("LIST", &[a, b]).pack();
        let (d, _) = Dict::unpack(&wire).expect("dict list");

        let list = d.get_dict_list("LIST");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].get_str("UID"), Some("a"));
        assert_eq!(list[1].get_str("UID"), Some("b"));
    }

    #[test]
    fn pack_prefix_signs_only_the_requested_ordered_items() {
        let wire = Dict::new()
            .put_int("VER", 3)
            .put_str("UID", "alice")
            .pack_prefix(1);

        assert_eq!(wire, b"IP2:7:VER:1:3:Z");
    }

    #[test]
    fn unpack_content_requires_exact_dict_content() {
        assert!(unpack_content(b"VER:1:3").is_some());
        assert!(unpack_content(b"VER:1:3:Z").is_none());
    }

    #[test]
    fn get_int_rejects_noncanonical_raw_values() {
        let d = Dict {
            items: vec![
                ("EMPTY".into(), b"".to_vec()),
                ("SIGN_ONLY".into(), b"-".to_vec()),
                ("TOO_LONG".into(), b"00000000000000003".to_vec()),
                ("NON_HEX".into(), b"10g".to_vec()),
            ],
        };

        for key in ["EMPTY", "SIGN_ONLY", "TOO_LONG", "NON_HEX"] {
            assert_eq!(d.get_int(key), None, "{key} 不能作为官方整数");
        }
    }

    #[test]
    fn roundtrip_scalars() {
        let mut d = Dict::new();
        d.put_int("VER", 3).put_int("PKT", 123456).put_str("NCK", "小明")
            .put_str("GRP", "研发部").put_int("NEG", -2000);
        let packed = d.pack();
        assert!(packed.starts_with(b"IP2:"));
        assert!(packed.ends_with(b":Z"));
        let (parsed, used) = Dict::unpack(&packed).expect("unpack");
        assert_eq!(used, packed.len());
        assert_eq!(parsed.get_int("VER"), Some(3));
        assert_eq!(parsed.get_int("PKT"), Some(123456));
        assert_eq!(parsed.get_str("NCK"), Some("小明"));
        assert_eq!(parsed.get_str("GRP"), Some("研发部"));
        assert_eq!(parsed.get_int("NEG"), Some(-2000));
    }

    #[test]
    fn roundtrip_nested() {
        let mut sub = Dict::new();
        sub.put_str("ADDR", "192.168.1.0").put_int("MASK", 24);
        let mut d = Dict::new();
        d.put_dict("NET", &sub);
        d.put_dict_list("LIST", &[sub.clone(), sub.clone(), sub.clone()]);
        let packed = d.pack();
        let (parsed, _) = Dict::unpack(&packed).expect("unpack");
        let net = parsed.get_dict("NET").expect("dict");
        assert_eq!(net.get_str("ADDR"), Some("192.168.1.0"));
        assert_eq!(net.get_int("MASK"), Some(24));
        let list = parsed.get_dict_list("LIST");
        assert_eq!(list.len(), 3);
        for item in list {
            assert_eq!(item.get_str("ADDR"), Some("192.168.1.0"));
        }
    }

    #[test]
    fn content_len_is_exact() {
        let mut d = Dict::new();
        d.put_str("key1", "str1").put_int("key2", 1000);
        let packed = d.pack();
        // 手解头部：IP2:x:... 的 x 必须等于 content 字节数
        let s = String::from_utf8_lossy(&packed);
        let rest = s.strip_prefix("IP2:").unwrap();
        let (len_hex, body) = rest.split_once(':').unwrap();
        let content_len = usize::from_str_radix(len_hex, 16).unwrap();
        let content = &body[..content_len];
        assert!(body.ends_with(":Z"));
        let (parsed, _) = Dict::unpack(&packed).unwrap();
        assert_eq!(parsed.get_str("key1"), Some("str1"));
        assert_eq!(parsed.get_int("key2"), Some(1000));
        assert_eq!(content, &body[..content_len]);
    }

    #[test]
    fn garbage_rejected() {
        assert!(Dict::unpack(b"not-ipdict").is_none());
        assert!(Dict::unpack(b"IP2:zz:xx:Z").is_none());
        assert!(Dict::unpack(b"IP2:5:abc:Z").is_none(), "长度与实际内容不符");
        // 截断
        let packed = Dict::new().put_str("a", "b").pack();
        assert!(Dict::unpack(&packed[..packed.len() - 1]).is_none());
    }
}
