//! IPMsg 协议报文编解码。
//!
//! 参考 H.Shirouzu《IP Messenger 通信协议规范》(protocol.txt, v0.9.x)：
//! 报文格式：`版本:包编号:发送者名:主机名:命令字:附加数据`
//! - 命令字 = 低 16 位基本命令 + 高位选项标志，十进制表示
//! - 附加数据中消息体与其后的文件列表以 `\0` 分隔，多个文件项以 `\a`(0x07) 分隔
//! - 文件项：`ID:文件名:大小:mtime:属性[:扩展属性...]`

use serde::Serialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 协议默认端口
pub const DEFAULT_PORT: u16 = 2425;

/// 基本命令（低 16 位）
pub mod cmd {
    #![allow(dead_code)]
    pub const NOOPERATION: u32 = 0x0000_0000;
    pub const BR_ENTRY: u32 = 0x0000_0001;
    pub const BR_EXIT: u32 = 0x0000_0002;
    pub const ANSENTRY: u32 = 0x0000_0003;
    pub const BR_ABSENCE: u32 = 0x0000_0004;
    pub const BR_ISGETLIST: u32 = 0x0000_0010;
    pub const OKGETLIST: u32 = 0x0000_0011;
    pub const GETLIST: u32 = 0x0000_0012;
    pub const ANSLIST: u32 = 0x0000_0013;
    pub const SENDMSG: u32 = 0x0000_0020;
    /// 送达确认：收到带 SENDCHECKOPT 的 SENDMSG 后立即回，附加数据为原包编号。
    /// 发送方据此把消息从「待发/重投队列」里删除；不回的话它会一直重发。
    pub const RECVMSG: u32 = 0x0000_0021;
    pub const READMSG: u32 = 0x0000_0030;
    pub const DELMSG: u32 = 0x0000_0031;
    pub const ANSREADMSG: u32 = 0x0000_0032;
    pub const GETINFO: u32 = 0x0000_0040;
    pub const SENDINFO: u32 = 0x0000_0041;
    pub const GETFILEDATA: u32 = 0x0000_0060;
    pub const RELEASEFILES: u32 = 0x0000_0061;
    pub const GETDIRFILES: u32 = 0x0000_0062;
    pub const GETPUBKEY: u32 = 0x0000_0072;
    pub const ANSPUBKEY: u32 = 0x0000_0073;
}

/// 选项标志（高位）—— 数值逐项对照官方 ipmsg.h Ver 4.50
/// （github.com/shirouzu/ipmsg），勿凭记忆修改
pub mod opt {
    #![allow(dead_code)]
    /// 0x100 在上线类报文里表示「离开模式」，在 SENDMSG 里表示
    /// 「请回送达确认」(IPMSG_SENDCHECKOPT)，同值不同义，按命令区分
    pub const ABSENCEOPT: u32 = 0x0000_0100;
    pub const SENDCHECKOPT: u32 = 0x0000_0100;
    pub const SERVEROPT: u32 = 0x0000_0200;
    pub const SECRETOPT: u32 = 0x0000_0200; // 与 SERVEROPT 同值（官方历史兼容）
    pub const BROADCASTOPT: u32 = 0x0000_0400;
    pub const MULTICASTOPT: u32 = 0x0000_0800;
    pub const AUTORETOPT: u32 = 0x0000_2000;
    pub const RETRYOPT: u32 = 0x0000_4000;
    pub const PASSWORDOPT: u32 = 0x0000_8000;
    pub const NOLOGOPT: u32 = 0x0002_0000;
    pub const NOADDLISTOPT: u32 = 0x0008_0000;
    pub const DIALUPOPT: u32 = 0x0001_0000;
    pub const READCHECKOPT: u32 = 0x0010_0000;
    pub const SECRETEXOPT: u32 = 0x0030_0000; // READCHECK|SECRET
    pub const ENCRYPTOPT: u32 = 0x0040_0000;
    /// 文件流加密能力广告位（spec §2 常量表）：上线类报文置位表示支持 TCP 文件流加密
    pub const CAPFILEENCOPT: u32 = 0x0004_0000;
    /// 文件流加密标志（官方 ipmsg.h）：GETFILEDATA/GETDIRFILES 置位表示扩展部为
    /// 密封的取文件请求、正文双向过 AES-CTR 密钥流（spec §7）。
    /// 与加密能力位 CAPA_SIGN_SHA1 同值但命名空间不同（命令选项位 vs ANSPUBKEY capa）。
    pub const ENCFILEOPT: u32 = 0x2000_0000;
    pub const CAPUTF8OPT: u32 = 0x0100_0000;
    /// 官方编码协商标志（ipmsg.h）：置位表示报文文本为 UTF-8，
    /// 未置位表示本地代码页（中文系统为 GBK）
    pub const UTF8OPT: u32 = 0x0080_0000;
    pub const CLIPBOARDOPT: u32 = 0x0800_0000;
    pub const FILEATTACHOPT: u32 = 0x0020_0000;
}

/// 文件类型属性
pub mod fileattr {
    #![allow(dead_code)]
    pub const REGULAR: u32 = 0x0000_0001;
    pub const PERM: u32 = 0x0000_0004;
    /// 官方值：目录为 2（非 REGULAR|PERM）
    pub const DIR: u32 = 0x0000_0002;
}

static PKT_SEQ: AtomicU32 = AtomicU32::new(0);

/// 生成唯一包编号（时间基址 + 序列扰动）
pub fn next_packet_no() -> u32 {
    let base = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(12345);
    let n = PKT_SEQ.fetch_add(1, Ordering::Relaxed);
    base.wrapping_add(n.wrapping_mul(7919)).max(1)
}

/// 报文头字段清洗：协议禁止 `:`、CR、LF、NUL 出现在头部字段
fn clean_field(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' | '\r' | '\n' | '\0' => '_',
            _ => c,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub pkt_no: u32,
    pub user: String,
    pub host: String,
    pub command: u32,
    pub extra: Vec<u8>,
}

impl Packet {
    pub fn new(command: u32) -> Self {
        Packet {
            pkt_no: next_packet_no(),
            user: String::new(),
            host: String::new(),
            command,
            extra: Vec::new(),
        }
    }

    pub fn with_pkt_no(mut self, no: u32) -> Self {
        self.pkt_no = no;
        self
    }

    /// 编码为线上字节流（头部 ASCII，附加数据原样）
    pub fn encode(&self, user: &str, host: &str) -> Vec<u8> {
        let mut buf =
            format!("1:{}:{}:{}:{}:", self.pkt_no, clean_field(user), clean_field(host), self.command)
                .into_bytes();
        buf.extend_from_slice(&self.extra);
        buf
    }
}

/// 从原始字节解析报文；头部按字节扫描前 5 个 `:`，避免 GBK 消息体破坏 UTF-8 解析
pub fn parse(raw: &[u8]) -> Option<Packet> {
    let mut idx = [0usize; 5];
    let mut found = 0usize;
    let mut i = 0usize;
    while i < raw.len() && found < 5 {
        if raw[i] == b':' {
            idx[found] = i;
            found += 1;
        }
        i += 1;
    }
    if found < 5 {
        return None;
    }
    let field = |k: usize| -> String {
        let start = if k == 0 { 0 } else { idx[k - 1] + 1 };
        String::from_utf8_lossy(&raw[start..idx[k]]).trim().to_string()
    };
    let _ver = field(0);
    let pkt_no: u32 = field(1).parse().ok()?;
    let user = field(2);
    let host = field(3);
    let command: u32 = field(4).parse().ok()?;
    Some(Packet {
        pkt_no,
        user,
        host,
        command,
        extra: raw[idx[4] + 1..].to_vec(),
    })
}

/* ---------------- 编码 / 解码 ---------------- */

/// 收到字节 → 文本：优先 UTF-8，失败则按 GBK 解码（兼容老版中文飞鸽传书）
pub fn decode_bytes(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::GBK.decode(b);
            decoded.into_owned()
        }
    }
}

/// 发送文本 → 字节：按配置编码（utf8 / gbk）
pub fn encode_out(s: &str, encoding: &str) -> Vec<u8> {
    if encoding.eq_ignore_ascii_case("gbk") {
        let (bytes, _, _) = encoding_rs::GBK.encode(s);
        bytes.into_owned()
    } else {
        s.as_bytes().to_vec()
    }
}

/// 取报文中的消息体（第一个 NUL 之前的部分）并解码
pub fn text_of(pkt: &Packet) -> String {
    let end = pkt.extra.iter().position(|&b| b == 0).unwrap_or(pkt.extra.len());
    decode_bytes(&pkt.extra[..end])
}

/* ---------------- 文件项 ---------------- */

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub id: u32,
    /// 对端公告中的原始 ID 字符串。各客户端进制约定不一（官方十六进制、
    /// 飞秋等十进制），回传 GETFILEDATA 请求时必须原样使用，禁止重新编码。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub raw_id: String,
    pub name: String,
    pub size: u64,
    pub mtime: u64,
    pub attr: u32,
}

impl FileEntry {
    /// 线上格式：与真实客户端抓包方言逐字段对齐（见 diag.log 样本）：
    /// - 文件 ID 用十进制（对方方言；size/mtime/attr 用十六进制）
    /// - attr 后保留 `:` 空扩展段
    /// 注意调用方需在整条公告末尾追加一个 `\a` 分隔符（样本含尾部分隔符）
    pub fn serialize(&self) -> String {
        format!(
            "{}:{}:{:x}:{:x}:{:x}:",
            self.id,
            clean_filename(&self.name),
            self.size,
            self.mtime,
            self.attr
        )
    }
}

fn clean_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' | '\u{7}' | '\0' | '\r' | '\n' => '_',
            _ => c,
        })
        .collect()
}

/// 数字字段宽容解析。官方客户端 ID 与属性用十六进制书写、大小与时间用十进制，
/// 这里对两种进制都做尝试：`hex_first` 用于 ID/属性，`dec_first` 用于大小/时间。
fn num_hex_first(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).ok();
    }
    if let Ok(v) = u64::from_str_radix(t, 16) {
        return Some(v);
    }
    t.parse::<u64>().ok()
}

fn num_dec_first(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = t.parse::<u64>() {
        return Some(v);
    }
    u64::from_str_radix(t.trim_start_matches("0x"), 16).ok()
}

/// 解析附加数据中 `\0` 之后的文件项列表（以 `\a` 分隔）
pub fn parse_file_entries(extra: &[u8]) -> Vec<FileEntry> {
    let start = match extra.iter().position(|&b| b == 0) {
        Some(p) => p + 1,
        None => return Vec::new(),
    };
    extra[start..]
        .split(|&b| b == 0x07)
        .filter(|seg| !seg.is_empty())
        .filter_map(|seg| {
            let mut it = seg.splitn(6, |&b| b == b':');
            let raw_id = String::from_utf8_lossy(it.next()?).trim().to_string();
            // 本环境对端（飞秋）ID 为十进制书写；含字母时自动按十六进制兜底
            let id = num_dec_first(&raw_id)?;
            let name = decode_bytes(it.next()?);
            if name.is_empty() {
                return None;
            }
            // 本环境真实客户端（见 diag.log 抓包）size/mtime 均为十六进制，
            // 与我方序列化保持一致：十六进制优先，纯字母串自动回退
            let size = num_hex_first(&String::from_utf8_lossy(it.next()?))?;
            let mtime = num_hex_first(&String::from_utf8_lossy(it.next()?)).unwrap_or(0);
            let attr =
                num_hex_first(&String::from_utf8_lossy(it.next()?)).map(|v| v as u32).unwrap_or(fileattr::REGULAR);
            Some(FileEntry {
                id: id as u32,
                raw_id,
                name,
                size,
                mtime,
                attr,
            })
        })
        .collect()
}

/// 构造上线类附加数据：昵称\0群组
pub fn build_entry_extra(nickname: &str, group: &str, encoding: &str) -> Vec<u8> {
    let mut extra = encode_out(nickname, encoding);
    extra.push(0);
    extra.extend_from_slice(&encode_out(group, encoding));
    extra
}

/// 解析上线类附加数据：(昵称, 群组)。
///
/// 官方格式为 `昵称\0群组`；部分客户端（飞秋等）会追加第三段及以后的能力信息，
/// 如 `Admin\0\0\nVS:00010002:5:8:6:1001` —— 只取前两段，其余忽略。
pub fn parse_entry_extra(extra: &[u8], utf8: bool) -> (String, String) {
    let mut segs = extra.split(|&b| b == 0);
    let nick = decode_for_command(segs.next().unwrap_or(&[]), if utf8 { opt::UTF8OPT } else { 0 });
    let group = decode_for_command(segs.next().unwrap_or(&[]), if utf8 { opt::UTF8OPT } else { 0 });
    (nick, group)
}

/// 去除解码文本中的控制字符，避免污染界面与日志
pub fn strip_control(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// 按报文的 UTF8OPT 标志解码文本：
/// - 置位：强制 UTF-8（官方协商语义）
/// - 未置位：先严格校验 UTF-8（部分实现置位习惯漏标时仍可正确解码），
///   校验失败再按本地代码页 GBK 解码
pub fn decode_for_command(b: &[u8], command: u32) -> String {
    if command & opt::UTF8OPT != 0 || std::str::from_utf8(b).is_ok() {
        String::from_utf8_lossy(b).into_owned()
    } else {
        let (decoded, _, _) = encoding_rs::GBK.decode(b);
        decoded.into_owned()
    }
}

/// 是否按 UTF-8 编码出站文本
pub fn is_utf8_mode(encoding: &str) -> bool {
    !encoding.eq_ignore_ascii_case("gbk")
}

/// 官方延迟投递尾注里的时间：`MM/DD HH:MM`，**本地时区**。
/// Unix 平台用 libc::localtime_r 取本地时间；其它平台回退 UTC 分量
/// （civil_from_days 换算，仅作保底）。
pub fn fmt_delayed(ts: u64) -> String {
    #[cfg(unix)]
    {
        let t = ts as i64;
        let mut tmv: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&t, &mut tmv);
        }
        format!(
            "{:02}/{:02} {:02}:{:02}",
            tmv.tm_mon + 1,
            tmv.tm_mday,
            tmv.tm_hour,
            tmv.tm_min
        )
    }
    #[cfg(not(unix))]
    {
        fn civil_from_days(z: i64) -> (i64, u32, u32) {
            let z = z + 719_468;
            let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
            let doe = (z - era * 146_097) as u64;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
            let y = yoe as i64 + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
            let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
            (if m <= 2 { y + 1 } else { y }, m, d)
        }
        let (_, mo, d) = civil_from_days((ts / 86_400) as i64);
        let secs = ts % 86_400;
        format!("{:02}/{:02} {:02}:{:02}", mo, d, secs / 3600, (secs % 3600) / 60)
    }
}

/* ---------------- 单元测试 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_roundtrip() {
        let mut p = Packet::new(cmd::SENDMSG | opt::FILEATTACHOPT).with_pkt_no(42);
        p.extra = b"hello world".to_vec();
        let user = "张三:san";
        let host = "pc\n01";
        let bytes = p.encode(user, host);
        let q = parse(&bytes).expect("parse ok");
        assert_eq!(q.pkt_no, 42);
        assert_eq!(q.user, "张三_san");
        assert_eq!(q.host, "pc_01");
        assert_eq!(q.command, cmd::SENDMSG | opt::FILEATTACHOPT);
        assert_eq!(q.extra, b"hello world");
        assert_eq!(text_of(&q), "hello world");
    }

    #[test]
    fn gbk_decode() {
        // “你好” 的 GBK 编码字节
        let gbk = [0xC4u8, 0xE3, 0xBA, 0xC3];
        assert_eq!(decode_bytes(&gbk), "你好");
    }

    #[test]
    fn gbk_encode_roundtrip() {
        let bytes = encode_out("你好世界", "gbk");
        assert_eq!(decode_bytes(&bytes), "你好世界");
        let utf8 = encode_out("你好", "utf8");
        assert_eq!(utf8, "你好".as_bytes());
    }

    #[test]
    fn file_entries_roundtrip() {
        let e1 = FileEntry {
            id: 1,
            raw_id: String::new(),
            name: "报告 最终版.pdf".into(),
            size: 20480,
            mtime: 1700000000,
            attr: fileattr::REGULAR,
        };
        let e2 = FileEntry {
            id: 2,
            raw_id: String::new(),
            name: "photo.jpg".into(),
            size: 999999,
            mtime: 1700000001,
            attr: fileattr::REGULAR,
        };
        let mut extra = "看看这两个文件".as_bytes().to_vec();
        extra.push(0);
        extra.extend_from_slice(e1.serialize().as_bytes());
        extra.push(0x07);
        extra.extend_from_slice(e2.serialize().as_bytes());

        let pkt = Packet {
            pkt_no: 7,
            user: "a".into(),
            host: "h".into(),
            command: cmd::SENDMSG | opt::FILEATTACHOPT,
            extra,
        };
        assert_eq!(text_of(&pkt), "看看这两个文件");
        let fs = parse_file_entries(&pkt.extra);
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].name, "报告 最终版.pdf");
        assert_eq!(fs[0].size, 20480);
        assert_eq!(fs[1].id, 2);
        assert_eq!(fs[1].size, 999999);

        // 线上格式与真实客户端方言一致：ID 十进制，size/mtime/attr 十六进制，
        // attr 后保留空扩展段（尾部冒号）
        assert_eq!(
            e1.serialize(),
            format!("1:报告 最终版.pdf:{:x}:{:x}:1:", 20480, 1700000000)
        );
    }

    #[test]
    fn file_entries_compat_styles() {
        // 官方/本环境风格：全字段十六进制（含纯数字串也按十六进制解释）
        let raw = b"text\x00a:name_a:100:111:1:xx\x07b:name_b:200:222:1";
        let fs = parse_file_entries(raw);
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].id, 0xa);
        assert_eq!(fs[0].size, 0x100);
        assert_eq!(fs[0].mtime, 0x111);
        assert_eq!(fs[1].id, 0xb);
        assert_eq!(fs[1].size, 0x200);
        assert_eq!(fs[1].mtime, 0x222);
    }

    #[test]
    fn file_entries_feiq_real_capture() {
        // 真实抓包（飞秋类客户端）：ID 十进制、size/mtime 十六进制、attr 后带空扩展段
        let raw = b"\x00 89000344:Microsoft Edge.lnk:8d4:6a894407:1:\x07";
        let fs = parse_file_entries(raw);
        assert_eq!(fs.len(), 1);
        // raw_id 必须原样保留，回传 GETFILEDATA 时按原字符串回显
        assert_eq!(fs[0].raw_id, "89000344");
        assert_eq!(fs[0].name, "Microsoft Edge.lnk");
        assert_eq!(fs[0].size, 0x8d4); // 2260 字节
        assert_eq!(fs[0].mtime, 0x6a894407);
    }

    #[test]
    fn entry_extra_roundtrip() {
        let extra = build_entry_extra("小明", "研发部", "utf8");
        let (nick, group) = parse_entry_extra(&extra, true);
        assert_eq!(nick, "小明");
        assert_eq!(group, "研发部");
    }

    #[test]
    fn entry_extra_feiq_capability_suffix() {
        // 飞秋等客户端：昵称\0群组\0能力串（VS=版本信息），能力串必须被忽略
        let extra = b"Admin\x00\x00\nVS:00010002:5:8:6:1001";
        let (nick, group) = parse_entry_extra(extra, false);
        assert_eq!(nick, "Admin");
        assert_eq!(group, "");
    }

    #[test]
    fn entry_extra_no_group() {
        let (nick, group) = parse_entry_extra("李四".as_bytes(), true);
        assert_eq!(nick, "李四");
        assert_eq!(group, "");
    }

    #[test]
    fn strip_control_removes_nul_and_newlines() {
        assert_eq!(strip_control("A\u{0}\nVS:1"), "AVS:1");
        assert_eq!(strip_control("正常名字"), "正常名字");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse(b"not a packet").is_none());
        assert!(parse(b"1:x:y:z:abc:def").is_none());
        assert!(parse(b"").is_none());
    }

    #[test]
    fn fmt_delayed_matches_official_shape() {
        // 官方延迟投递尾注形如 "(IPMsg Delayed Send: 08/22 15:02)"
        // 具体值依赖机器时区，这里只断言形状与跨天变化（与时区无关）
        assert_eq!(fmt_delayed(0).len(), 11, "MM/DD HH:MM = 11 字符");
        assert_ne!(fmt_delayed(0), fmt_delayed(86_400), "跨天必须变化");
        assert_ne!(
            fmt_delayed(0),
            fmt_delayed(31 * 86_400),
            "跨月必须变化（至少日期不同）"
        );
        // 本机（UTC+8，对照 diag.log 的本地时间戳）校验精确值：
        // fmt_delayed(1787537826) == "08/24 10:17"
    }
}
