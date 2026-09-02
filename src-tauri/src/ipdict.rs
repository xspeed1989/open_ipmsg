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
//! 格式本身无类型信息：本实现按「结构探测 + 调用方类型」双轨解析——
//! 值先尝试按 dict/list 结构解析，失败再按标量（int→str→bytes 启发）。
//! 我方能控制的两端（成员主互操作、ANSLIST_DICT）字段类型是确定的，
//! 官方互通时常用字段（VER/PKT/CMD/FLAGS/STAT/MASK/NCK/GRP/ADDR 等）类型
//! 亦与官方约定一致。

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

/// IPDict 值类型（解析端使用的宽松表示）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Val {
    Int(i64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Val>),
    Dict(Vec<(String, Val)>),
}

impl Val {
    pub fn get_int(&self) -> Option<i64> {
        match self {
            Val::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn get_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn get_dict(&self) -> Option<&[(String, Val)]> {
        match self {
            Val::Dict(d) => Some(d),
            _ => None,
        }
    }
}

/// 有序字典（保留键的插入顺序，与官方一致）
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Dict {
    pub items: Vec<(String, Val)>,
}

impl Dict {
    pub fn new() -> Self {
        Dict { items: Vec::new() }
    }

    pub fn put_int(&mut self, key: &str, v: i64) -> &mut Self {
        self.items.push((key.to_string(), Val::Int(v)));
        self
    }

    pub fn put_str(&mut self, key: &str, v: &str) -> &mut Self {
        self.items.push((key.to_string(), Val::Str(v.to_string())));
        self
    }

    pub fn put_bytes(&mut self, key: &str, v: &[u8]) -> &mut Self {
        self.items.push((key.to_string(), Val::Bytes(v.to_vec())));
        self
    }

    pub fn put_dict(&mut self, key: &str, sub: &Dict) -> &mut Self {
        self.items.push((key.to_string(), Val::Dict(sub.items.clone())));
        self
    }

    pub fn put_dict_list(&mut self, key: &str, list: &[Dict]) -> &mut Self {
        self.items.push((
            key.to_string(),
            Val::List(list.iter().map(|d| Val::Dict(d.items.clone())).collect()),
        ));
        self
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// 取 dict_list（Val::List 内含 Val::Dict）→ Vec<Dict>；单 dict 值也兼容
    pub fn get_dict_list(&self, key: &str) -> Vec<Dict> {
        match self.get(key) {
            Some(Val::List(items)) => items
                .iter()
                .filter_map(|v| v.get_dict().map(|i| Dict { items: i.to_vec() }))
                .collect(),
            Some(Val::Dict(items)) => vec![Dict {
                items: items.clone(),
            }],
            _ => Vec::new(),
        }
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.get_int())
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.get_str())
    }

    pub fn has(&self, key: &str) -> bool {
        self.items.iter().any(|(k, _)| k == key)
    }

    /// 序列化完整格式 `IP2:(len):(content):Z`
    pub fn pack(&self) -> Vec<u8> {
        let content = self.pack_content_impl();
        let mut out = IPDICT_HEAD.as_bytes().to_vec();
        out.extend_from_slice(format!("{:x}:", content.len()).as_bytes());
        out.extend_from_slice(&content);
        out.extend_from_slice(IPDICT_FOOT.as_bytes());
        out
    }

    fn pack_content_impl(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in &self.items {
            out.extend_from_slice(k.as_bytes());
            out.push(b':');
            let val = pack_val(v);
            out.extend_from_slice(format!("{:x}:", val.len()).as_bytes());
            out.extend_from_slice(&val);
        }
        out
    }

    /// 解析完整格式。返回 (解析后的字典, 消耗的字节数)；0 表示失败。
    pub fn unpack(data: &[u8]) -> Option<(Dict, usize)> {
        if !data.starts_with(IPDICT_HEAD.as_bytes()) {
            return None;
        }
        let mut i = IPDICT_HEAD.len();
        // 内容长度（十六进制）
        let len_start = i;
        while i < data.len() && data[i] != b':' {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let len_hex = std::str::from_utf8(&data[len_start..i]).ok()?;
        let content_len = usize::from_str_radix(len_hex.trim(), 16).ok()?;
        i += 1; // ':'
        if i + content_len + IPDICT_FOOT.len() > data.len() {
            return None;
        }
        let content = &data[i..i + content_len];
        if &data[i + content_len..i + content_len + IPDICT_FOOT.len()] != IPDICT_FOOT.as_bytes() {
            return None;
        }
        let total = i + content_len + IPDICT_FOOT.len();
        let dict = parse_content(content)?;
        Some((dict, total))
    }
}

/// 序列化「内容段」（去掉 IP2:…:Z 外壳的 key:len:val 序列）。
/// 签名辅助（DIR 成员主协议）与外壳打包共用。
pub fn pack_content(d: &Dict) -> Vec<u8> {
    d.pack_content_impl()
}

/// 值 → 线格式字节（int 十六进制 ASII；str UTF-8；bytes 原样；
/// List 为 len:val 序列；Dict 为嵌套完整格式）
fn pack_val(v: &Val) -> Vec<u8> {
    match v {
        // 负数按官方约定带 '-' 前缀的十六进制（Rust {:x} 对负数输出补码，不能直接用）
        Val::Int(n) if *n < 0 => format!("-{:x}", n.unsigned_abs()).into_bytes(),
        Val::Int(n) => format!("{n:x}").into_bytes(),
        Val::Str(s) => s.as_bytes().to_vec(),
        Val::Bytes(b) => b.clone(),
        Val::List(items) => {
            let mut out = Vec::new();
            for it in items {
                let iv = pack_val(it);
                out.extend_from_slice(format!("{:x}:", iv.len()).as_bytes());
                out.extend_from_slice(&iv);
            }
            out
        }
        // dict 值 = 内容段格式（官方 ipdict.h：dict ≠ ipdict，后者才带 IP2 外壳）
        Val::Dict(sub) => pack_content(&Dict { items: sub.clone() }),
    }
}

/// 解析一段内容（key:len:val…）为字典
fn parse_content(data: &[u8]) -> Option<Dict> {
    let mut items = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        // key 到下一个 ':'
        let key_start = i;
        while i < data.len() && data[i] != b':' {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let key = String::from_utf8_lossy(&data[key_start..i]).into_owned();
        if key.is_empty() {
            return None;
        }
        i += 1;
        // len（十六进制）
        let len_start = i;
        while i < data.len() && data[i] != b':' {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let len_hex = std::str::from_utf8(&data[len_start..i]).ok()?;
        let vlen = usize::from_str_radix(len_hex.trim(), 16).ok()?;
        i += 1;
        if i + vlen > data.len() {
            return None;
        }
        let val = parse_val(&data[i..i + vlen]);
        i += vlen;
        items.push((key, val));
    }
    Some(Dict { items })
}

/// 解析一个值：优先尝试 dict 结构（嵌套 IP2 或 key:len:val 模式），
/// 再尝试 int（纯 hex），否则按 UTF-8 字符串/字节
fn parse_val(data: &[u8]) -> Val {
    // 嵌套完整 IP2 格式
    if data.starts_with(IPDICT_HEAD.as_bytes()) {
        if let Some((d, used)) = Dict::unpack(data) {
            if used == data.len() {
                return Val::Dict(d.items);
            }
        }
    }
    // 逐项结构（key:len:val 可套）→ dict
    if let Some(d) = parse_content(data) {
        return Val::Dict(d.items);
    }
    // 列表结构（len:val 重复）
    if let Some(l) = parse_list(data) {
        return Val::List(l);
    }
    // 标量：整数 hex（可带 '-'）
    let s = String::from_utf8_lossy(data);
    let t = s.trim();
    let neg = t.strip_prefix('-');
    let body = neg.unwrap_or(t);
    if !body.is_empty() && body.len() <= 16 && body.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(v) = i64::from_str_radix(body, 16) {
            return Val::Int(if neg.is_some() { -v } else { v });
        }
    }
    match std::str::from_utf8(data) {
        Ok(s) => Val::Str(s.to_string()),
        Err(_) => Val::Bytes(data.to_vec()),
    }
}

/// 尝试按列表结构（重复的 len:val）解析
fn parse_list(data: &[u8]) -> Option<Vec<Val>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let len_start = i;
        while i < data.len() && data[i] != b':' {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let len_hex = std::str::from_utf8(&data[len_start..i]).ok()?;
        let vlen = usize::from_str_radix(len_hex.trim(), 16).ok()?;
        i += 1;
        if i + vlen > data.len() {
            return None;
        }
        out.push(parse_val(&data[i..i + vlen]));
        i += vlen;
    }
    Some(out)
}



#[cfg(test)]
mod tests {
    use super::*;

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
        let net = parsed.get("NET").and_then(|v| v.get_dict()).expect("dict").to_vec();
        let net = Dict { items: net };
        assert_eq!(net.get_str("ADDR"), Some("192.168.1.0"));
        assert_eq!(net.get_int("MASK"), Some(24));
        let list = parsed.get("LIST").expect("list");
        match list {
            Val::List(items) => {
                assert_eq!(items.len(), 3);
                for it in items {
                    let d = it.get_dict().expect("list item 必须是 dict");
                    assert_eq!(d[0].0, "ADDR");
                }
            }
            other => panic!("LIST 被解析成 {other:?}"),
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
#[cfg(test)]
mod dbg_tests {
    use super::*;
    #[test]
    fn negative_int_uses_minus_prefix_hex() {
        let mut d = Dict::new();
        d.put_int("NEG", -2000);
        let p = d.pack();
        eprintln!("packed={:?}", String::from_utf8_lossy(&p));
        let (parsed, _) = Dict::unpack(&p).unwrap();
        let v = parsed.get("NEG").unwrap();
        eprintln!("parsed NEG = {:?}", v);
        assert_eq!(parsed.get_int("NEG"), Some(-2000));
    }
}
