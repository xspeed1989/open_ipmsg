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

/// 基本命令（低 16 位）——逐项对照官方 ipmsg.h Ver4.50
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
    pub const ANSLIST_DICT: u32 = 0x0000_0014;
    pub const BR_ISGETLIST2: u32 = 0x0000_0018;
    pub const SENDMSG: u32 = 0x0000_0020;
    /// 送达确认：收到带 SENDCHECKOPT 的 SENDMSG 后立即回，附加数据为原包编号。
    /// 发送方据此把消息从「待发/重投队列」里删除；不回的话它会一直重发。
    pub const RECVMSG: u32 = 0x0000_0021;
    pub const READMSG: u32 = 0x0000_0030;
    /// 封书破弃通知（消息删除/撤回）：附加数据为原 SENDMSG 包编号。
    /// 官方语义：封书（SECRETOPT）接收方可「破弃」（不开封直接销毁）并回此报文；
    /// 本客户端扩展：对端用它通告「消息已撤回」，我方把对应气泡替换为撤回占位。
    pub const DELMSG: u32 = 0x0000_0031;
    /// READMSG 带 READCHECKOPT 时的确认应答（8 版协议，附加数据为原包编号）
    pub const ANSREADMSG: u32 = 0x0000_0032;
    pub const GETINFO: u32 = 0x0000_0040;
    pub const SENDINFO: u32 = 0x0000_0041;
    /// 不在模式成员的不在通知文获取（官方 §3-11）
    pub const GETABSENCEINFO: u32 = 0x0000_0050;
    pub const SENDABSENCEINFO: u32 = 0x0000_0051;
    pub const GETFILEDATA: u32 = 0x0000_0060;
    pub const RELEASEFILES: u32 = 0x0000_0061;
    pub const GETDIRFILES: u32 = 0x0000_0062;
    /// 密码保护目录传输（官方 ipmsg.h 预留命令，官方 Win 客户端未实现；
    /// 本客户端按自洽设计实现，见 docs/ipmsg-protocol-gap.md 附录）
    pub const DIRFILES_AUTH: u32 = 0x0000_0063;
    pub const DIRFILES_AUTHRET: u32 = 0x0000_0064;
    pub const GETPUBKEY: u32 = 0x0000_0072;
    pub const ANSPUBKEY: u32 = 0x0000_0073;
    /// NAT 中继代理（官方 ipmsg.h 预留命令 0xa0–0xa3，官方 Win 客户端未实现；
    /// 本客户端按自洽设计实现，见 docs/ipmsg-protocol-gap.md 附录）
    pub const AGENT_REQ: u32 = 0x0000_00a0;
    pub const AGENT_ANSREQ: u32 = 0x0000_00a1;
    pub const AGENT_PACKET: u32 = 0x0000_00a2;
    pub const AGENT_PROXYREQ: u32 = 0x0000_00a3;
    /// 成员主目录服务（官方 §3-10、14 版协议；IPDict 序列化）
    pub const DIR_POLL: u32 = 0x0000_00b0;
    pub const DIR_POLLAGENT: u32 = 0x0000_00b1;
    pub const DIR_BROADCAST: u32 = 0x0000_00b2;
    pub const DIR_ANSBROAD: u32 = 0x0000_00b3;
    pub const DIR_PACKET: u32 = 0x0000_00b4;
    pub const DIR_REQUEST: u32 = 0x0000_00b5;
    pub const DIR_AGENTPACKET: u32 = 0x0000_00b6;
    pub const DIR_EVBROAD: u32 = 0x0000_00b7;
    pub const DIR_AGENTREJECT: u32 = 0x0000_00b8;
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
    /// 封书+已读回执（官方 ipmsg.h：(READCHECKOPT|SECRETOPT) = 0x0010_0200；
    /// 曾误写 0x0030_0000（缺 SECRET 位）导致封书报文无法识别，2026-09 修正）
    pub const SECRETEXOPT: u32 = 0x0010_0200;
    pub const ENCRYPTOPT: u32 = 0x0040_0000;
    /// 加密扩展消息标志（官方 ipmsg.h = 0x04000000，spec §5 铁证）：
    /// **加密文件公告必须带此位**——官方 DecryptMsg 只在带此位时才拆分
    /// 附件段（exBuf）；不带则正文里 \0 之后的文件条目被丢弃 → 对面只见
    /// 文字、附件消失（2026-08-26 官方客户端实测）。明文公告不受影响
    /// （exStr 在 ResolveMsg 阶段即已拆分）。Entry 类报文亦带此位声明能力。
    pub const ENCEXTMSGOPT: u32 = 0x0400_0000;
    /// 文件流加密能力广告位（spec §2 常量表）：上线类报文置位表示支持 TCP 文件流加密
    pub const CAPFILEENCOPT: u32 = 0x0004_0000;
    /// 文件流加密标志（官方 ipmsg.h L119 = 0x00000800）：GETFILEDATA/GETDIRFILES
    /// 置位表示扩展部为密封的取文件请求、正文双向过 AES-CTR 密钥流（spec §7）。
    /// 官方协议按命令类别复用 0x800（入口类报文里同值是 MULTICASTOPT），
    /// 与本文件既有 MULTICASTOPT 的并存正是官方语义。
    pub const ENCFILEOPT: u32 = 0x0000_0800;
    pub const CAPUTF8OPT: u32 = 0x0100_0000;
    /// 官方编码协商标志（ipmsg.h L86 铁证）：**文本标志位 = 0x00800000**，
    /// 置位表示本报文文本为 UTF-8，未置位表示本地代码页（中文系统为 GBK）。
    /// 0x01000000（CAPUTF8OPT）只是 Entry 上的**能力声明**，不用于 SENDMSG
    /// 文本判定——两者曾因 my 误改混淆（2026-08-26 恢复），官方客户端只认
    /// 0x00800000，用错位会让其按 GBK 解我们的 UTF-8 内容 → 中文乱码。
    pub const UTF8OPT: u32 = 0x0080_0000;
    pub const CLIPBOARDOPT: u32 = 0x0800_0000;
    pub const FILEATTACHOPT: u32 = 0x0020_0000;
    /// IPDict 新格式能力（官方 ipmsg.h L92 = 0x02000000，v5 并存格式）
    pub const CAPIPDICTOPT: u32 = 0x0200_0000;
    /// 成员主角色能力位（官方 ipmsg.h L93 = 0x10000000，14 版协议）
    pub const DIR_MASTER: u32 = 0x1000_0000;
}

/// 文件类型属性（fileattr 低 8 位，官方 ipmsg.h L152–L160）
pub mod fileattr {
    #![allow(dead_code)]
    pub const REGULAR: u32 = 0x0000_0001;
    /// 官方值：目录为 2（非 REGULAR|PERM）
    pub const DIR: u32 = 0x0000_0002;
    /// 返回上级目录（目录流内使用）
    pub const RETPARENT: u32 = 0x0000_0003;
    pub const PERM: u32 = 0x0000_0004;
    pub const SYMLINK: u32 = 0x0000_0004;
    pub const CDEV: u32 = 0x0000_0005;
    pub const BDEV: u32 = 0x0000_0006;
    pub const FIFO: u32 = 0x0000_0007;
    pub const RESFORK: u32 = 0x0000_0010;
    /// 剪贴板图片附件（官方「粘贴图片」）
    pub const CLIPBOARD: u32 = 0x0000_0020;
    /// 属性位（高 24 位）：只读
    pub const RONLYOPT: u32 = 0x0000_0100;
    pub const HIDDENOPT: u32 = 0x0000_1000;
    pub const EXHIDDENOPT: u32 = 0x0000_2000;
    pub const ARCHIVEOPT: u32 = 0x0000_4000;
    pub const SYSTEMOPT: u32 = 0x0000_8000;
}

/// 附件扩展属性类型（文件项尾段 `extend-attr=val` 的 attr 编号，官方 ipmsg.h
/// L170–L187）。本客户端主要使用 CLIPBOARDPOS（贴图插入位置）与解析兜底。
pub mod extattr {
    #![allow(dead_code)]
    pub const UID: u32 = 0x01;
    pub const USERNAME: u32 = 0x02;
    pub const GID: u32 = 0x03;
    pub const GROUPNAME: u32 = 0x04;
    pub const CLIPBOARDPOS: u32 = 0x08;
    pub const PERM: u32 = 0x10;
    pub const MAJORNO: u32 = 0x11;
    pub const MINORNO: u32 = 0x12;
    pub const CTIME: u32 = 0x13;
    pub const MTIME: u32 = 0x14;
    pub const ATIME: u32 = 0x15;
    pub const CREATETIME: u32 = 0x16;
    pub const CREATOR: u32 = 0x20;
    pub const FILETYPE: u32 = 0x21;
    pub const FINDERINFO: u32 = 0x22;
    pub const ACL: u32 = 0x30;
    pub const ALIASFNAME: u32 = 0x40;
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
    /// 扩展属性段（官方 spec §3-5 extend-attr）：`attr 编号 → 值字符串`。
    /// 常见：CLIPBOARDPOS(8)=插入位置（贴图）、PERM/UID/MTIME 等。
    /// 解析端透传；发送端仅贴图带 CLIPBOARDPOS。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ext_attrs: Vec<(u32, String)>,
}

impl FileEntry {
    /// 取指定编号的扩展属性值（首个匹配）
    pub fn ext(&self, id: u32) -> Option<&str> {
        self.ext_attrs
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, v)| v.as_str())
    }

    /// 线上格式：与真实客户端抓包方言逐字段对齐（见 diag.log 样本）：
    /// - 文件 ID 用十进制（对方方言；size/mtime/attr 用十六进制）
    /// - 文件名含 `:` 时按官方规范用 `::` 转义（官方 ipmsg.h 明文规定；
    ///   旧版本地实现曾用 `_` 替换，解析端对两种都宽容）
    /// - attr 后依次拼扩展属性段 `key=value:`，无扩展时保留 `:` 空扩展段
    /// 注意调用方需在整条公告末尾追加一个 `\a` 分隔符（样本含尾部分隔符）
    /// encoding：文件名按发送编码写入（utf8→UTF-8；gbk→GBK）。UTF-8 配置下
    /// 报文带 UTF8OPT 标志、官方按 UTF-8 解；GBK 配置下不带标志、官方按本地
    /// 码页解——两者都必须与正文编码一致，否则中文文件名乱码（2026-08 现场）。
    pub fn serialize(&self, encoding: &str) -> String {
        let mut out = format!(
            "{}:{}:{:x}:{:x}:{:x}",
            self.id,
            clean_filename_enc(&self.name, encoding),
            self.size,
            self.mtime,
            self.attr
        );
        for (k, v) in &self.ext_attrs {
            out.push(':');
            out.push_str(&format!("{k}={v}"));
        }
        out.push(':');
        out
    }
}

/// 官方规范：文件名中的 `:` 以 `::` 转义（其余分隔符类字符规范未定义，替换为 `_`）
fn clean_filename_enc(s: &str, encoding: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| match c {
            ':' => ':'.to_string() + ":", // "::" 转义
            '\u{7}' | '\0' | '\r' | '\n' => "_".to_string(),
            _ => c.to_string(),
        })
        .collect();
    // 非法字符清洗后再按发送编码转换（GBK 下不转会导致本地码页对端乱码）
    String::from_utf8_lossy(&encode_out(&s, encoding)).into_owned()
}

/// 按 `:` 切分一段字节，但跳过 `::` 转义对（官方文件名转义约定）。
/// 返回的段内 `::` 已还原为单个 `:`。
fn split_colon_escaped(seg: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < seg.len() {
        if seg[i] == b':' {
            if i + 1 < seg.len() && seg[i + 1] == b':' {
                cur.push(b':'); // 转义对还原
                i += 2;
                continue;
            }
            out.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(seg[i]);
        i += 1;
    }
    out.push(cur);
    out
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

/// 解析附加数据中 `\0` 之后的文件项列表（以 `\a` 分隔）。
/// 字段：`ID:文件名:大小:mtime:属性[:扩展属性=值...]`；
/// 文件名中的 `::` 转义对先还原；扩展属性解析为 (编号, 值) 对透传。
pub fn parse_file_entries(extra: &[u8]) -> Vec<FileEntry> {
    let start = match extra.iter().position(|&b| b == 0) {
        Some(p) => p + 1,
        None => return Vec::new(),
    };
    extra[start..]
        .split(|&b| b == 0x07)
        .filter(|seg| !seg.is_empty())
        .filter_map(|seg| {
            let fields = split_colon_escaped(seg);
            if fields.len() < 5 {
                return None;
            }
            let raw_id = String::from_utf8_lossy(&fields[0]).into_owned();
            // 本环境对端（飞秋）ID 为十进制书写；含字母时自动按十六进制兜底
            let id = num_dec_first(&raw_id)?;
            let name = decode_bytes(&fields[1]);
            if name.is_empty() {
                return None;
            }
            // 本环境真实客户端（见 diag.log 抓包）size/mtime 均为十六进制，
            // 与我方序列化保持一致：十六进制优先，纯字母串自动回退
            let size = num_hex_first(&String::from_utf8_lossy(&fields[2]))?;
            let mtime = num_hex_first(&String::from_utf8_lossy(&fields[3])).unwrap_or(0);
            let attr =
                num_hex_first(&String::from_utf8_lossy(&fields[4])).map(|v| v as u32).unwrap_or(fileattr::REGULAR);
            // 扩展属性段：`key=value`（值可含逗号；末尾空段忽略）
            let mut ext_attrs: Vec<(u32, String)> = Vec::new();
            for f in &fields[5..] {
                let f = String::from_utf8_lossy(f);
                let f = f.trim();
                if f.is_empty() {
                    continue;
                }
                let (k, v) = match f.split_once('=') {
                    Some((k, v)) => (k, v),
                    None => (f, ""), // 无 '=' 的孤立段宽容为无值属性
                };
                if let Some(kv) = num_hex_first(k) {
                    ext_attrs.push((kv as u32, v.to_string()));
                }
            }
            Some(FileEntry {
                id: id as u32,
                raw_id,
                name,
                size,
                mtime,
                attr,
                ext_attrs,
            })
        })
        .collect()
}

/// 上线类附加数据的解析结果。
///
/// 传统字段：`昵称\0群组`；UTF-8 扩展（官方 §3-9，BR 系报文的标准 UTF-8
/// 表达方式）：`\0` 之后接 `\n` + 若干 `UN:/HN:/NN:/GN:/VS:` 行，NN:/GN:
/// 出现时优先覆盖昵称/群组。部分客户端（飞秋等）只发 `\nVS:...` 版本行。
#[derive(Debug, Clone, Default, Serialize)]
pub struct EntryInfo {
    pub nick: String,
    pub group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vs: Option<String>,
}

/// 构造上线类附加数据：昵称\0群组（无 UTF-8 扩展行）
pub fn build_entry_extra(nickname: &str, group: &str, encoding: &str) -> Vec<u8> {
    build_entry_extra_ex(nickname, group, "", "", encoding)
}

/// 构造上线类附加数据：UTF-8 编码模式下按官方 §3-9 追加 `\0\nNN:/GN:/UN:/HN:`
/// 扩展行（BR 系报文禁用 UTF8OPT 位，UTF-8 名字走本扩展；官方 msgmng.cpp
/// 在报文带 CAPUTF8OPT 时解析这些行并覆盖昵称/群组）。GBK 模式下不加扩展，
/// 保持纯本地码页字节。
pub fn build_entry_extra_ex(
    nickname: &str,
    group: &str,
    user: &str,
    host: &str,
    encoding: &str,
) -> Vec<u8> {
    let mut extra = encode_out(nickname, encoding);
    extra.push(0);
    extra.extend_from_slice(&encode_out(group, encoding));
    if is_utf8_mode(encoding) {
        extra.push(0);
        extra.push(b'\n');
        if !user.is_empty() {
            extra.extend_from_slice(format!("UN:{user}\n").as_bytes());
        }
        if !host.is_empty() {
            extra.extend_from_slice(format!("HN:{host}\n").as_bytes());
        }
        if !nickname.is_empty() {
            extra.extend_from_slice(format!("NN:{nickname}\n").as_bytes());
        }
        if !group.is_empty() {
            extra.extend_from_slice(format!("GN:{group}\n").as_bytes());
        }
        // VS: 版本行（官方 MakeMsg 恒带；hex 元组布局）
        extra.extend_from_slice(format!("VS:{}\n", ver_hex_info()).as_bytes());
    }
    extra
}

/// 线上版本串（官方 VS:/CVER 布局）：`00010002:主:次:补丁:0`
pub fn ver_hex_info() -> String {
    let v = env!("CARGO_PKG_VERSION");
    let mut parts = v.split('.');
    let maj = parts.next().unwrap_or("0");
    let min = parts.next().unwrap_or("0");
    let pat = parts.next().unwrap_or("0");
    format!("{:08x}:{maj}:{min}:{pat}:0", 0x0001_0002)
}

/// 解析上线类附加数据（含 `\n` 扩展行）。
///
/// 官方格式为 `昵称\0群组[\0\nUN:...\nHN:...\nNN:...\nGN:...\nVS:...]`；
/// 部分客户端（飞秋等）只追加 `\0\0\nVS:00010002:5:8:6:1001` 版本行。
/// `\n` 行仅在报文带 CAPUTF8OPT 时参与解析（官方 msgmng.cpp 同款门控），
/// 且行内文本恒按 UTF-8 解码（官方 §3-9：扩展本身即 UTF-8）。
pub fn parse_entry_extra(extra: &[u8], command: u32) -> EntryInfo {
    let mut segs = extra.split(|&b| b == 0);
    let nick = decode_for_command(segs.next().unwrap_or(&[]), command);
    let group = decode_for_command(segs.next().unwrap_or(&[]), command);
    let mut info = EntryInfo {
        nick,
        group,
        ..Default::default()
    };
    // 第三段起为 ulist（\n 扩展行）；仅当报文声明 CAPUTF8OPT 时采用
    if command & opt::CAPUTF8OPT != 0 {
        if let Some(rest) = segs.next() {
            let rest = rest.strip_prefix(&[b'\n'][..]).unwrap_or(rest);
            for line in rest.split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let line = String::from_utf8_lossy(line).into_owned();
                if let Some(v) = line.strip_prefix("UN:") {
                    info.uname = Some(v.to_string());
                } else if let Some(v) = line.strip_prefix("HN:") {
                    info.hname = Some(v.to_string());
                } else if let Some(v) = line.strip_prefix("NN:") {
                    if !v.is_empty() {
                        info.nick = v.to_string();
                    }
                } else if let Some(v) = line.strip_prefix("GN:") {
                    if !v.is_empty() {
                        info.group = v.to_string();
                    }
                } else if let Some(v) = line.strip_prefix("VS:") {
                    info.vs = Some(v.to_string());
                }
            }
        }
    }
    info
}

/* ---------------- 主机列表（BR_ISGETLIST/OKGETLIST/GETLIST/ANSLIST） ---------------- */

/// 主机列表条目的空字段占位（官方 HOSTLIST_DUMMY = `\b`）
pub const HOSTLIST_DUMMY: &str = "\u{8}";
/// ANSLIST 主字段分隔符（官方 HOSTLIST_SEPARATOR = `\a`）
pub const HOSTLIST_SEP: u8 = 0x07;

/// ANSLIST 中的一台主机（字段顺序与官方 MakeHostListStr 一致）
#[derive(Debug, Clone)]
pub struct HostListEntry {
    pub user: String,
    pub host: String,
    pub status: u32,
    pub ip: String,
    pub port: u16,
    pub nick: String,
    pub group: String,
}

/// 线上端口字段：官方把 `htons(port)` 按十进制 %d 打印（小端 Windows 上即
/// 字节序翻转值，2425 → 30985）。输出端保持一致（u16::from_be）。
/// 解析端两种解释都试（见 parse_host_port）。
pub fn host_port_wire(port: u16) -> u32 {
    u32::from(u16::from_be(port))
}

/// 宽容解析 ANSLIST 里的小端翻转端口：优先取「翻转换算后正好是 2425」的
/// 那个候选（官方小端机器线值），否则取原样（BE 实现直写端口号）。
pub fn parse_host_port(v: u32) -> u16 {
    let swapped = (v as u16).swap_bytes();
    if swapped == crate::protocol::DEFAULT_PORT {
        swapped
    } else {
        v as u16
    }
}

/// 序列化 ANSLIST 附加数据（官方 SendHostList 结构）：
/// `<续传索引>\a<本包条数>\a` + 每条
/// `<user>\a<host>\a<status>\a<ip>\a<port>\a<nick|\b>\a<group|\b>\a`
/// 返回 (线上字节, 实际编入条数)；超过预算即截断（调用方续传）。
pub fn build_anslist(
    hosts: &[HostListEntry],
    start: usize,
    budget: usize,
    encoding: &str,
) -> (Vec<u8>, usize) {
    let mut out: Vec<u8> = format!("0\u{7}0\u{7}").into_bytes();
    let mut n = 0usize;
    for h in hosts.iter().skip(start) {
        let wire = format!(
            "{}\u{7}{}\u{7}{}\u{7}{}\u{7}{}\u{7}{}\u{7}{}\u{7}",
            clean_field(&h.user),
            clean_field(&h.host),
            h.status,
            h.ip,
            host_port_wire(h.port),
            if h.nick.is_empty() { HOSTLIST_DUMMY.into() } else { h.nick.clone() },
            if h.group.is_empty() { HOSTLIST_DUMMY.into() } else { h.group.clone() },
        );
        let bytes = encode_out(&wire, encoding);
        if out.len() + bytes.len() > budget {
            break;
        }
        out.extend_from_slice(&bytes);
        n += 1;
    }
    // 回填续传索引与本包条数（条数可能被预算截断）
    let head = format!(
        "{}\u{7}{}\u{7}",
        if start + n == hosts.len() { 0 } else { start + n },
        n
    );
    let head = encode_out(&head, encoding);
    out[..head.len()].copy_from_slice(&head);
    (out, n)
}

/// 解析 ANSLIST 附加数据：返回 (续传索引, 主机列表)。
/// 每台主机 7 个字段（user\ahost\astatus\aip\aport\anick\agroup），
/// 空占位 `\b` 还原为空串；字段不足即截断。
pub fn parse_anslist(extra: &[u8], command: u32) -> (u32, Vec<HostListEntry>) {
    let fields: Vec<&[u8]> = extra.split(|&b| b == HOSTLIST_SEP).collect();
    let atoi = |f: &[u8]| -> u32 {
        String::from_utf8_lossy(f).trim().parse::<u32>().unwrap_or(0)
    };
    if fields.len() < 2 {
        return (0, Vec::new());
    }
    let cont = atoi(fields[0]);
    let mut out = Vec::new();
    let mut i = 2;
    while i + 7 <= fields.len() {
        let (user, host, status, ip, port, nick, group) = (
            decode_for_command(fields[i], command),
            decode_for_command(fields[i + 1], command),
            atoi(fields[i + 2]),
            String::from_utf8_lossy(fields[i + 3]).trim().to_string(),
            parse_host_port(atoi(fields[i + 4])),
            decode_for_command(fields[i + 5], command),
            decode_for_command(fields[i + 6], command),
        );
        i += 7;
        // 整条都是空 → 跳过（官方序列化的尾部空字段）
        if user.is_empty() && host.is_empty() && ip.is_empty() && nick.is_empty() {
            continue;
        }
        out.push(HostListEntry {
            user,
            host,
            status,
            ip,
            port,
            nick: if nick == HOSTLIST_DUMMY { String::new() } else { nick },
            group: if group == HOSTLIST_DUMMY { String::new() } else { group },
        });
    }
    (cont, out)
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
            ext_attrs: vec![],
        };
        let e2 = FileEntry {
            id: 2,
            raw_id: String::new(),
            name: "photo.jpg".into(),
            size: 999999,
            mtime: 1700000001,
            attr: fileattr::REGULAR,
            ext_attrs: vec![(extattr::CLIPBOARDPOS, "0".into())],
        };
        let mut extra = "看看这两个文件".as_bytes().to_vec();
        extra.push(0);
        extra.extend_from_slice(e1.serialize("utf8").as_bytes());
        extra.push(0x07);
        extra.extend_from_slice(e2.serialize("utf8").as_bytes());

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
        // CLIPBOARDPOS 扩展属性透传
        assert_eq!(fs[1].ext(extattr::CLIPBOARDPOS), Some("0"));

        // 线上格式与真实客户端方言一致：ID 十进制，size/mtime/attr 十六进制，
        // attr 后保留空扩展段（尾部冒号）
        assert_eq!(
            e1.serialize("utf8"),
            format!("1:报告 最终版.pdf:{:x}:{:x}:1:", 20480, 1700000000)
        );
        // 带扩展属性：attr 后拼 `key=value:` 段
        assert_eq!(
            e2.serialize("utf8"),
            format!("2:photo.jpg:{:x}:{:x}:1:8=0:", 999999, 1700000001)
        );
    }

    #[test]
    fn file_name_colon_escaped_roundtrip() {
        // 官方规范：文件名中的 ':' 以 "::" 转义；序列化与解析必须还原
        let e = FileEntry {
            id: 3,
            raw_id: String::new(),
            name: "A:B 报告:c.txt".into(),
            size: 10,
            mtime: 1,
            attr: fileattr::REGULAR,
            ext_attrs: vec![],
        };
        let wire = e.serialize("utf8");
        assert!(wire.contains("A::B 报告::c.txt"), "必须按 :: 转义线上格式: {wire}");
        let mut extra = b"\0".to_vec();
        extra.extend_from_slice(wire.as_bytes());
        extra.push(0x07);
        let fs = parse_file_entries(&extra);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].name, "A:B 报告:c.txt");
        assert_eq!(fs[0].id, 3);
    }

    #[test]
    fn file_names_with_raw_colons_still_parse() {
        // 旧式/非官方实现不转义直接带 ':' 的方言：id 与名字按段位宽容解析
        let raw = b"x\x00a:b:100:200:1:".to_vec();
        let fs = parse_file_entries(&raw);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].name, "b", "旧方言无转义，': 前截断为名字（宽容不报错）");
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
        assert_eq!(fs[0].raw_id, " 89000344");
        assert_eq!(fs[0].name, "Microsoft Edge.lnk");
        assert_eq!(fs[0].size, 0x8d4); // 2260 字节
        assert_eq!(fs[0].mtime, 0x6a894407);
    }

    #[test]
    fn file_entry_preserves_raw_id_whitespace_while_parsing_numeric_id() {
        let raw = b"body\x00 10 :report.txt:20:1234:1:\x07";
        let files = parse_file_entries(raw);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, 10);
        assert_eq!(files[0].raw_id, " 10 ");
    }

    #[test]
    fn file_entries_official_clipboard_image() {
        // 官方 5.8.6「粘贴图片」公告（share.cpp EncodeMsg 实测格式）：
        //   id:ipmsgclip_s_<id>_<pos>.png:size:mtime:attr(0x20):8=<pos>:\a
        // attr=IPMSG_FILE_CLIPBOARD(0x20)，扩展段 IPMSG_FILE_CLIPBOARDPOS=8
        // 必须正确解析出名字/扩展名与属性，接收侧才能按图片自动接收内联预览
        let raw = b"\x000e:ipmsgclip_s_14_0.png:55:7b:20:8=0:\x07".to_vec();
        let fs = parse_file_entries(&raw);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].id, 0x0e);
        assert_eq!(fs[0].name, "ipmsgclip_s_14_0.png");
        assert_eq!(fs[0].size, 0x55);
        assert_eq!(fs[0].attr & 0xFF, 0x20, "IPMSG_FILE_CLIPBOARD");
        assert_eq!(fs[0].ext(extattr::CLIPBOARDPOS), Some("0"), "CLIPBOARDPOS 扩展段要落到 ext_attrs");
        assert!(
            fs[0].name.to_lowercase().ends_with(".png"),
            "粘贴图片必须保留 .png 扩展名（内联预览前提）"
        );
    }

    #[test]
    fn entry_extra_roundtrip() {
        // UTF-8 模式：传统字段 + \0\nNN:/GN: 扩展行，NN:/GN: 覆盖解析结果
        let extra = build_entry_extra_ex("小明", "研发部", "xiaoming", "pc-01", "utf8");
        let info = parse_entry_extra(&extra, cmd::BR_ENTRY | opt::CAPUTF8OPT);
        assert_eq!(info.nick, "小明");
        assert_eq!(info.group, "研发部");
        assert_eq!(info.uname.as_deref(), Some("xiaoming"));
        assert_eq!(info.hname.as_deref(), Some("pc-01"));
        // GBK 模式：不加扩展行
        let extra = build_entry_extra("小明", "研发部", "gbk");
        assert!(!extra.contains(&b'\n'), "GBK 模式不得带 \\n 扩展");
        let info = parse_entry_extra(&extra, cmd::BR_ENTRY);
        assert_eq!(info.nick, "小明");
        assert_eq!(info.group, "研发部");

        // 没有 CAPUTF8OPT 时扩展行不参与解析（官方门控）
        let extra = build_entry_extra_ex("网名", "组", "u", "h", "utf8");
        let info = parse_entry_extra(&extra, cmd::BR_ENTRY | opt::UTF8OPT);
        assert_eq!(info.nick, "网名");
        assert_eq!(info.uname, None);
    }

    #[test]
    fn entry_extra_feiq_capability_suffix() {
        // 飞秋等客户端：昵称\0群组\0\nVS=版本信息，能力串必须被忽略
        let extra = b"Admin\x00\x00\nVS:00010002:5:8:6:1001";
        let info = parse_entry_extra(extra, cmd::BR_ENTRY | opt::CAPUTF8OPT);
        assert_eq!(info.nick, "Admin");
        assert_eq!(info.group, "");
        assert_eq!(info.vs.as_deref(), Some("00010002:5:8:6:1001"));
    }

    #[test]
    fn entry_extra_no_group() {
        let info = parse_entry_extra("李四".as_bytes(), cmd::BR_ENTRY | opt::CAPUTF8OPT);
        assert_eq!(info.nick, "李四");
        assert_eq!(info.group, "");
    }

    #[test]
    fn anslist_roundtrip() {
        let hosts = vec![
            HostListEntry {
                user: "alice".into(),
                host: "pc-a".into(),
                status: cmd::BR_ENTRY | opt::CAPUTF8OPT,
                ip: "192.168.1.10".into(),
                port: 2425,
                nick: "爱丽丝".into(),
                group: "研发部".into(),
            },
            HostListEntry {
                user: "bob".into(),
                host: "pc-b".into(),
                status: cmd::BR_ENTRY,
                ip: "192.168.1.11".into(),
                port: 2425,
                nick: String::new(),
                group: String::new(),
            },
        ];
        let (wire, n) = build_anslist(&hosts, 0, 4096, "utf8");
        assert_eq!(n, 2);
        let (cont, parsed) = parse_anslist(&wire, cmd::ANSLIST | opt::CAPUTF8OPT);
        assert_eq!(cont, 0, "全部编入 → 续传索引为 0");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].user, "alice");
        assert_eq!(parsed[0].nick, "爱丽丝");
        assert_eq!(parsed[0].group, "研发部");
        assert_eq!(parsed[0].port, 2425);
        assert_eq!(parsed[1].nick, "");
        assert_eq!(parsed[1].group, "");
        assert_eq!(parsed[1].port, 2425);
        // 端口线值 = htons(port) 十进制（官方小端怪癖：2425 → 30985）
        assert!(wire.windows(5).any(|w| w == b"30985"), "端口必须按网络序线值");
        // 预算截断 → 续传索引指向下一台（首条目+头部约 62B）
        let (wire2, n2) = build_anslist(&hosts, 0, 40, "utf8");
        assert_eq!(n2, 0);
        let (cont2, _) = parse_anslist(&wire2, cmd::ANSLIST);
        assert_eq!(cont2, 0, "一条都装不下时续传仍为 0（调用方按条数判断结束）");
        let (wire3, n3) = build_anslist(&hosts, 0, 90, "utf8");
        let (cont3, _) = parse_anslist(&wire3, cmd::ANSLIST);
        assert_eq!(n3, 1);
        assert_eq!(cont3, 1, "装下 1 条 → 续传索引 1");
    }

    #[test]
    fn anslist_parses_official_wire_shape() {
        // 官方 MakeHostListStr 形态（小端 htons 端口 30985、\b 空占位、尾部空段）
        let raw = b"1\x077\x07alice\x07pc-a\x07400003\x07192.168.1.10\x0730985\x07\x08\x07grp\x07\x07";
        let (cont, list) = parse_anslist(raw, cmd::ANSLIST);
        assert_eq!(cont, 1);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].ip, "192.168.1.10");
        assert_eq!(list[0].port, 2425, "30985 字节序翻转回 2425");
        assert_eq!(list[0].nick, "");
        assert_eq!(list[0].group, "grp");
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
