//! 目录流的**格式**（架构评审候选 5 抽出的 interface）。
//!
//! 背景：GETDIRFILES 的那条流（`<头部长度16进制>:<名称>:<大小16进制>:<属性16进制>:` 与
//! 内容逐条相接）原来有两份实现 —— 生产侧在 `net.rs` 里逐字段拼、夹具在 `selftest.rs`
//! 里又拼一份（`dir_head`/`fake_dir_stream`，注释还自述「与被测代码不共享逻辑」）。
//! 协议一改要改两处，而且夹具会替被测代码一起犯同一个错。
//!
//! 现在格式只有这一份：生产侧的写端（`serve_dir_stream`）、读端（`StreamReader`），
//! 与自检夹具的假对端，都坐在这里。两个 adapter（真客户端 / 假对端）才让这个 seam
//! 成立。
//!
//! 注意这里只管**格式**：目录树怎么遍历（`collect_dir_ops`）、流怎么过加密密钥流、
//! 接收端怎么落盘（`fetch_dir_tree`），都留在 net.rs —— 那些不跨这个 seam。

/// 目录流条目的类型（低 8 位；官方 ipmsg.h 的类型值）。
pub mod attr {
    // 官方属性全表：只用其子集，其余留作对账依据
    #![allow(dead_code)]
    pub const REGULAR: u32 = 1;
    pub const ENTER: u32 = 2;
    pub const RETPARENT: u32 = 3;
    // 官方 ipmsg.h 还定义了这些类型；我们不落盘，但必须把内容读掉，
    // 否则流会错位，后面的条目全部解析失败
    pub const SYMLINK: u32 = 4;
    pub const CDEV: u32 = 5;
    pub const BDEV: u32 = 6;
    pub const FIFO: u32 = 7;
    pub const RESFORK: u32 = 0x10;
}

/// 组装一条头部（长度字段自身也计入总长）。
///
/// 名称里的 `:` `/` `\` NUL CR LF 一律换成 `_`（它们会破坏字段切分）；
/// 官方用 4 位十六进制，超长文件名时自然扩展位数并迭代收敛。
pub fn header(name: &str, size: u64, attr: u32) -> Vec<u8> {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | '\0' | '\r' | '\n' => '_',
            _ => c,
        })
        .collect();
    let body = format!(":{cleaned}:{size:x}:{attr:x}:");
    let mut width = 4usize;
    loop {
        let total = width + body.len();
        let head = format!("{:0width$x}", total, width = width);
        if head.len() == width {
            return format!("{head}{body}").into_bytes();
        }
        width = head.len();
    }
}

/// 解析一条**完整**头部（长度字段 + 名称 + 大小 + 属性[:扩展]）。
///
/// 字段从右往左定位，所以名称里含冒号也能还原；尾部的扩展段（`key=value`）剥掉。
/// 大小与属性按十六进制解析（官方写法），并容忍 `0x` 前缀。
pub fn parse_header(head: &[u8]) -> Option<(String, u64, u32)> {
    let first = head.iter().position(|&b| b == b':')?;
    let body = &head[first + 1..];
    let body = body.strip_suffix(b":").unwrap_or(body);
    let mut fields: Vec<&[u8]> = body.split(|&b| b == b':').collect();
    if fields.len() < 3 {
        return None;
    }
    while fields.len() > 3 && fields.last().map(|f| f.contains(&b'=')).unwrap_or(false) {
        fields.pop();
    }
    let attr = num_flex_hex(&String::from_utf8_lossy(fields[fields.len() - 1]))? as u32;
    let size = num_flex_hex(&String::from_utf8_lossy(fields[fields.len() - 2]))?;
    let name = crate::protocol::decode_bytes(&fields[..fields.len() - 2].join(&b':'));
    Some((crate::protocol::strip_control(&name), size, attr))
}

/// 明文取文件/取目录请求行的正文段：`<包号>:<文件ID>:<偏移>`。
///
/// 只管这三个字段**怎么写**；用哪种进制书写（方言）与加密外壳仍由调用方决定 ——
/// 所以假对端与生产侧的明文方言路径能共用它，而方言表与密封逻辑留在 net.rs。
pub fn request_line(pkt: &str, id: &str, offset: u64) -> String {
    format!("{pkt}:{id}:{offset}")
}

/// 十六进制数字（容忍 `0x` 前缀与两侧空白）
fn num_flex_hex(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    u64::from_str_radix(t.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_length_is_self_inclusive() {
        let h = header("a.txt", 12, attr::REGULAR);
        let text = String::from_utf8(h.clone()).unwrap();
        let len = usize::from_str_radix(text.split(':').next().unwrap(), 16).unwrap();
        assert_eq!(len, h.len(), "长度字段要把自己算进去");
        assert!(text.ends_with(':'));
    }

    #[test]
    fn header_escapes_separators_in_name() {
        let h = header("a:b/c\\d", 3, attr::ENTER);
        let text = String::from_utf8(h).unwrap();
        assert!(text.contains(":a_b_c_d:"), "分隔符要换成下划线：{text}");
    }

    #[test]
    fn header_and_parse_round_trip() {
        let raw = header("报告.txt", 4096, attr::ENTER);
        let (name, size, attr_v) = parse_header(&raw).expect("应能解析");
        assert_eq!((name.as_str(), size, attr_v), ("报告.txt", 4096, attr::ENTER));
    }

    #[test]
    fn parse_header_keeps_colons_inside_name_and_drops_ext_attrs() {
        // 写端会把名称里的冒号转义掉，但真实对端可能发来含冒号的形态：
        // 字段必须从右往左定位才能还原，尾部的扩展段（key=value）要剥掉
        let raw = b"0016:a:b.txt:10:1:8=0:";
        let (name, size, attr_v) = parse_header(raw).expect("应能解析");
        assert_eq!(name, "a:b.txt");
        assert_eq!(size, 0x10);
        assert_eq!(attr_v, attr::REGULAR);
    }

    #[test]
    fn parse_header_rejects_short_or_broken() {
        assert!(parse_header(b"").is_none());
        assert!(parse_header(b"0005:a.txt:x:1:").is_none(), "大小不是十六进制");
        assert!(parse_header(b"0004:a:b:").is_none(), "字段不足");
    }

    #[test]
    fn request_line_always_carries_offset() {
        assert_eq!(request_line("10", "3", 0), "10:3:0");
        assert_eq!(request_line("a", "b", 777), "a:b:777");
    }
}
