//! 应用状态：配置、在线用户表、对外提供的文件槽、聊天记录(JSONL) 持久化与事件回调。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::KeyPair;
use rsa::{BigUint, RsaPublicKey};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub download_dir: String,
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// 界面主题：system（跟随系统）/ light / dark
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 界面语言：'zh-CN' / 'en'；空串表示未设置，由前端按系统语言探测
    #[serde(default)]
    pub lang: String,
    /// 端到端加密总开关：默认开启；关闭时消息按官方明文协议发送
    #[serde(default = "default_encrypt")]
    pub encrypt: bool,
    /// 不在模式：开启后 Entry 系报文带 ABSENCEOPT，收到消息自动回不在通知文
    #[serde(default)]
    pub absence_enabled: bool,
    /// 不在通知文（自动应答与 GETABSENCEINFO 的返回内容）
    #[serde(default = "default_absence_text")]
    pub absence_text: String,
    /// 密码功能总开关（官方 PasswordUse）：开启后发送可选「密码」档，
    /// 收到的密码消息必须先输入本机设置的密码才能查看
    #[serde(default)]
    pub password_use: bool,
    /// 本机密码（官方 PasswordStr 语义：双方约定同一口令）
    #[serde(default)]
    pub password: String,
    /// NAT 中继代理地址（AGENT 协议；空 = 不用代理）。格式 "ip:port" 或裸 IP（默认端口）
    #[serde(default)]
    pub agent_addr: String,
    /// 成员主（DIR_MASTER）地址；空 = 不启用目录服务。格式同 agent_addr
    #[serde(default)]
    pub master_addr: String,
    /// 是否允许对方向我们索取主机列表（官方 AllowSendList）
    #[serde(default = "default_true")]
    pub allow_send_list: bool,
    /// 是否声明 IPDict 能力位（v5 并存格式；默认开，协议层无害）
    #[serde(default = "default_true")]
    pub ipdict_enabled: bool,
    /// 成员主目录服务模式：off（关闭）/ user（作为成员，POLL master_addr）/
    /// master（作为成员主，汇总并分发全网列表）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir_mode: String,
    /// IPv6 组播成员发现（ff15::979/ff02::1）：默认开启；关闭后纯 IPv4，
    /// 供与官方 Windows 客户端混合组网时排查「v4/v6 双条目」互通问题
    #[serde(default = "default_true")]
    pub v6_mcast: bool,
}

fn default_absence_text() -> String {
    "我现在不在，有事留言。".into()
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "system".into()
}

fn default_encoding() -> String {
    // 默认 UTF-8，出站报文自动携带官方 IPMSG_UTF8OPT 编码协商标志；
    // 与 GBK 方言老客户端互通时可在设置中切换
    "utf8".into()
}

fn default_encrypt() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            nickname: String::new(),
            group: String::new(),
            download_dir: String::new(),
            encoding: default_encoding(),
            theme: default_theme(),
            lang: String::new(),
            encrypt: default_encrypt(),
            absence_enabled: false,
            absence_text: default_absence_text(),
            password_use: false,
            password: String::new(),
            agent_addr: String::new(),
            master_addr: String::new(),
            allow_send_list: default_true(),
            ipdict_enabled: default_true(),
            dir_mode: String::new(),
            v6_mcast: default_true(),
        }
    }
}

/// 待投递的离线消息（对方上线后自动发送，官方 IPMsg 语义）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PendingOut {
    /// 对方会话 key（IP）
    pub key: String,
    /// 原包号：重投用同一包号，对端按 (IP, pkt) 去重
    pub pkt: u32,
    /// 消息正文（不含延迟尾注，投递时现拼）
    pub text: String,
    /// 原始发送时间
    pub ts: u64,
    /// 附件本地路径（离线文件消息也入队；投递时重新校验并登记文件槽）。
    /// 旧版队列 JSON 没有此字段，serde default 保证兼容加载。
    #[serde(default)]
    pub paths: Vec<String>,
}

/// 历史会话摘要（中栏「离线会话」数据源）
#[derive(Serialize, Clone, Debug)]
pub struct SessionInfo {
    pub key: String,
    pub nickname: String,
    pub host: String,
    pub group: String,
    /// 该会话最近一条消息的时间戳
    pub last_ts: u64,
    /// 未读入站消息数（read=false 的 in 记录）。
    ///
    /// 前端启动时用它补回「WebView 监听就绪前被丢弃的 msg-in 事件」：
    /// 对端离线留言在我方上线瞬间重投即属此列——消息已落库，但事件没人
    /// 接，红点/托盘闪烁因此缺失；摘要的未读数就是这个持久化状态的投影。
    pub unread: u32,
    /// 最近一条未读消息的时间戳（决定托盘唤起时跳到哪个会话）
    pub unread_ts: u64,
}

/// 在历史互斥区内原子判定并落库一条入站记录后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InRecordOutcome {
    Inserted,
    Duplicate,
    ReplacedConflict,
}

/// 局域网内的对端用户
#[derive(Serialize, Clone, Debug)]
pub struct PeerInfo {
    /// 稳定标识：对端 IP。一台主机一个用户（与官方 IPMsg 语义一致）——
    /// NAT 改写或套接字重绑会让同一主机的每次广播来自不同源端口，
    /// 按 ip:port 去重会把同一个人挂成一串重复条目
    pub key: String,
    pub ip: String,
    /// 最近一次报文的源端口，用作投递地址（随报文更新）
    pub port: u16,
    pub nickname: String,
    pub group: String,
    pub host: String,
    pub user: String,
    pub last_seen: u64,
    /// 不在模式（Entry 系报文带 ABSENCEOPT）
    #[serde(default)]
    pub absence: bool,
    /// 对方不在通知文（GETABSENCEINFO 取到后缓存）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absence_text: Option<String>,
    /// 对方客户端版本串（Entry ulist 扩展 VS: 行）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vs: Option<String>,
}

/// 我们发出、等待对端来取的文件
pub struct OfferedFile {
    pub path: PathBuf,
    pub size: u64,
    /// 是否为目录（走 GETDIRFILES 流式传输）
    pub is_dir: bool,
    /// 登记时刻，用于过期清理
    pub ts: u64,
    /// 公告是否按 UTF-8 发出：对端取文件请求应带 UTF8OPT（官方 §3-9），
    /// 目录流内的文件名编码据此决定
    pub utf8: bool,
}

/// 在线消息的送达重发队列项（官方 §4-12「確認・リトライ」：
/// 带 SENDCHECKOPT 的消息在确认超时后重发同一包号，累计若干次后放弃）。
#[derive(Clone, Debug)]
pub struct RetryOut {
    pub key: String,
    pub pkt: u32,
    pub text: String,
    pub paths: Vec<String>,
    /// 首次登记的文件条目（ID 必须原样复用，重投公告与首投一致）
    pub entries: Vec<crate::protocol::FileEntry>,
    pub ts: u64,
    pub attempts: u32,
}

/// 成员主（DIR_MASTER）一侧的成员登记：POLL 成员与其代理角色
#[derive(Clone, Debug)]
pub struct DirMember {
    pub key: String,
    /// 成员最近一次 POLL 的源端口（DIR_PACKET 分发目标）
    pub port: u16,
    pub last_poll: u64,
    /// 被任命为代理广播的时长（秒，官方 AGS 键）
    pub agent_secs: u64,
    /// 该成员报告的本段网络（ADDR/MASK，如 "192.168.1.0/24"）
    pub seg: String,
    /// 是否已被任命为代理广播（DIR_BROADCAST 已发出）
    pub is_agent: bool,
}

/// 文件槽保留时长：对端可能延迟很久才来取，但也不能无限累积
pub const OFFER_TTL_SECS: u64 = 24 * 3600;

type EventFn = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// 对端密钥的内存缓存条目：最新公钥（加密/验签首选）+ 上一把（验签备选）。
/// 备选解决多客户端交替/多实例场景：同一 IP 轮流用不同密钥对发言时，
/// 缓存被覆盖导致验签随缓存浮动（2026-08-26 实测场景），保留上一把可
/// 让两个密钥的签名都验证通过。备选不持久化（重启后由自愈重握手重新学习）。
struct PeerCryptoEntry {
    capa: u32,
    pub_key: RsaPublicKey,
    prev_key: Option<RsaPublicKey>,
}

/// peer_keys.json 单条记录：{ip: {capa, n_b64, e_b64}}
#[derive(Serialize, Deserialize)]
struct PeerKeyEntry {
    capa: u32,
    n_b64: String,
    e_b64: String,
}

/// peer_keys.json 容器（rev=2：ANSPUBKEY 线格式改用官方 revendian 后，
/// rev<2 的旧缓存里公钥字节序是错的，必须整体作废重新握手）
#[derive(Serialize, Deserialize)]
struct PeerKeyFile {
    rev: u32,
    keys: HashMap<String, PeerKeyEntry>,
}

const PEER_KEY_FILE_REV: u32 = 2;

/// 读取 hidden_contacts.json（已删除会话 key 列表）；文件缺失/损坏时视为空。
fn load_hidden_contacts(data_dir: &PathBuf) -> HashSet<String> {
    if let Ok(bytes) = std::fs::read(data_dir.join("hidden_contacts.json")) {
        if let Ok(list) = serde_json::from_slice::<Vec<String>>(&bytes) {
            return list.into_iter().collect();
        }
    }
    HashSet::new()
}

fn b64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub peers: Mutex<HashMap<String, PeerInfo>>,
    pub offered: Mutex<HashMap<(u32, u32), OfferedFile>>,
    /// 对端 IP → 上次成功取文件用的请求方言下标（见 net::DIALECTS）
    dialect: Mutex<HashMap<String, usize>>,
    /// (对端IP, 包编号) → 已回送达确认的次数
    ack_count: Mutex<HashMap<(IpAddr, u32), u32>>,
    seen_queue: Mutex<VecDeque<(IpAddr, u32)>>,
    seen_set: Mutex<HashSet<(IpAddr, u32)>>,
    /// 聊天记录文件的读-改-写互斥（防止并发追加与重写互相覆盖）;
    /// ipmsg_import 的会话归并需要整段持锁做原子合并
    pub(crate) hist_lock: Mutex<()>,
    /// 待投递的离线消息：会话 key → 队列（FIFO）
    pending_out: Mutex<HashMap<String, Vec<PendingOut>>>,
    on_event: Mutex<Option<EventFn>>,
    /// 对端 IP → 密钥缓存（最新公钥 + 上一把备选）；持久化到 peer_keys.json（仅最新）
    peer_crypto: Mutex<HashMap<String, PeerCryptoEntry>>,
    /// 已确认走明文协议的对端 IP（仅内存态：重启后重新协商）
    peer_plain: Mutex<HashSet<String>>,
    /// 对端 IP → 已发出的 GETPUBKEY 探测次数（仅内存态：重启即重置）
    probe_counts: Mutex<HashMap<String, u32>>,
    /// 自愈重握手限频：IP → 最近一次触发时刻（秒）（任一端换钥后自动恢复用）
    rehandshake_at: Mutex<HashMap<String, u64>>,
    /// 本机密钥对：懒加载生成 + ipmsg_key.json 持久化（进程内只生成一次）
    own_key: OnceLock<Arc<KeyPair>>,
    /// 被用户删除（隐藏）的会话 key 集合：微信式「删除会话」语义——
    /// 删除后从列表消失（记录一并删除），对方再发消息时自动恢复。
    /// 持久化到 hidden_contacts.json，重启不丢。
    hidden_contacts: Mutex<HashSet<String>>,
    /// 在线消息送达重发队列：(会话 key, 包号) → 重发项（RECVMSG 确认后移除）
    retry_out: Mutex<HashMap<(String, u32), RetryOut>>,
    /// 成员主登记（DIR_MASTER 服务端一侧）：成员 IP → 登记
    dir_members: Mutex<HashMap<String, DirMember>>,
    /// 会话 key → 已缓存的对端不在通知文（GETABSENCEINFO 结果；BR_ABSENCE
    /// 重新通告清掉，避免展示过期内容）
    peer_absence: Mutex<HashMap<String, String>>,
    /// 主机列表获取窗口（unix 秒截止）：窗口内收到 OKGETLIST 才发起 GETLIST
    /// （官方 entryStartTime 窗口同款节流；启动与手动刷新时重开窗口）
    hostlist_window: Mutex<Option<u64>>,
    /// 成员主（DIR_MASTER）维护的全网主机列表（IPDict 主机字典条目）
    master_hosts: Mutex<Vec<crate::ipdict::Dict>>,
    /// 成员侧：作为代理（agent）的有效期截止（master IP → unix 秒；
    /// DIR_POLLAGENT 的 AGS 秒数；期间本段 entry 事件转发 master）
    agent_until: Mutex<HashMap<String, u64>>,
    /// 中继代理（AGENT 协议）两侧登记：
    /// - 客户端侧：会话 key（NAT 对端 ip）→ 我方向其发应答所用代理地址
    relay_agent: Mutex<HashMap<String, std::net::SocketAddr>>,
    /// - 代理侧：来源 ip → 真实对端地址（转发 AGENT_PACKET 的目标）
    relay_peers: Mutex<HashMap<String, std::net::SocketAddr>>,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
}

const SEEN_CAP: usize = 8192;
/// 对端公钥缓存条数上限：正常局域网远用不满，防敌意洪泛导致无限膨胀
const PEER_KEY_CAP: usize = 4096;
/// GETPUBKEY 探测预算（spec §5「已标记无能力」的最小实现）：同一 IP 连续
/// 探测达到该次数仍无公钥缓存 → 认定对端不支持加密，标记明文、停止探测。
pub const PLAIN_PROBE_BUDGET: u32 = 3;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/* ---------- 全局日志开关（--log 运行时参数） ---------- */

/// 日志默认关闭；`--log` 参数启动（lib.rs run() 解析）或单实例回调转发时打开。
/// 同时控制 diag.log 诊断文件（AppState::diag）与全部 stderr 传输日志（oim_log! 宏）。
static LOG_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn log_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

pub fn set_log_enabled(on: bool) {
    LOG_ENABLED.store(on, Ordering::Relaxed);
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let logs_dir = data_dir.join("logs");
        AppState {
            config: Mutex::new(Config::default()),
            peers: Mutex::new(HashMap::new()),
            offered: Mutex::new(HashMap::new()),
            dialect: Mutex::new(HashMap::new()),
            ack_count: Mutex::new(HashMap::new()),
            seen_queue: Mutex::new(VecDeque::new()),
            seen_set: Mutex::new(HashSet::new()),
            hist_lock: Mutex::new(()),
            pending_out: Mutex::new(HashMap::new()),
            on_event: Mutex::new(None),
            peer_crypto: Mutex::new(HashMap::new()),
            peer_plain: Mutex::new(HashSet::new()),
            probe_counts: Mutex::new(HashMap::new()),
            rehandshake_at: Mutex::new(HashMap::new()),
            own_key: OnceLock::new(),
            hidden_contacts: Mutex::new(load_hidden_contacts(&data_dir)),
            retry_out: Mutex::new(HashMap::new()),
            dir_members: Mutex::new(HashMap::new()),
            peer_absence: Mutex::new(HashMap::new()),
            hostlist_window: Mutex::new(None),
            master_hosts: Mutex::new(Vec::new()),
            agent_until: Mutex::new(HashMap::new()),
            relay_agent: Mutex::new(HashMap::new()),
            relay_peers: Mutex::new(HashMap::new()),
            data_dir,
            logs_dir,
        }
    }

    /* ---------- 配置 ---------- */

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    pub fn set_config(&self, cfg: Config) {
        *self.config.lock().unwrap() = cfg;
    }

    pub fn load_config(&self) {
        let path = self.data_dir.join("config.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(cfg) = serde_json::from_slice::<Config>(&bytes) {
                *self.config.lock().unwrap() = cfg;
                return;
            }
        }
        // 首次运行：昵称留空（前端弹出设置向导），下载目录 = 数据目录/接收文件
        let mut cfg = Config::default();
        if cfg.download_dir.is_empty() {
            cfg.download_dir = self.data_dir.join("接收文件").to_string_lossy().into_owned();
        }
        *self.config.lock().unwrap() = cfg;
        let _ = self.persist_config();
    }

    pub fn persist_config(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let bytes = serde_json::to_vec_pretty(&*self.config.lock().unwrap())?;
        std::fs::write(self.data_dir.join("config.json"), bytes)
    }

    /// 线路诊断日志（数据目录/diag.log，超过 512KB 自动截断）。
    /// 记录入站报文摘要与文件传输失败原因，用于远程定位互通问题。
    /// 日志默认关闭：`--log` 参数启动才写（见 LOG_ENABLED）。
    pub fn diag(&self, line: &str) {
        if !log_enabled() {
            return;
        }
        use std::io::Write;
        static DIAG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = DIAG_LOCK.lock().unwrap();
        let path = self.data_dir.join("diag.log");
        let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(_) => return,
        };
        if let Ok(meta) = f.metadata() {
            if meta.len() > 512 * 1024 {
                // 截断重开
                drop(f);
                if std::fs::write(&path, b"").is_err() {
                    return;
                }
                f = match std::fs::OpenOptions::new().append(true).open(&path) {
                    Ok(f) => f,
                    Err(_) => return,
                };
            }
        }
        let _ = writeln!(f, "{} {}", now_secs(), line.trim_end());
    }

    /* ---------- 事件桥接 ---------- */

    pub fn set_event(&self, f: EventFn) {
        *self.on_event.lock().unwrap() = Some(f);
    }

    pub fn emit(&self, event: &str, value: serde_json::Value) {
        if let Ok(guard) = self.on_event.lock() {
            if let Some(f) = guard.as_ref() {
                f(event, value);
            }
        }
    }

    /* ---------- 用户表 ---------- */

    /// 插入或刷新用户；返回是否为新增。所有文本字段先清洗控制字符。
    ///
    /// 身份以 IP 为准：key 一律归一化为 `info.ip`，同 IP 的重复广播
    /// （哪怕源端口不同）原地合并；端口仅作为投递地址随最新报文刷新。
    pub fn upsert_peer(&self, mut info: PeerInfo) -> bool {
        use crate::protocol::strip_control;
        info.nickname = strip_control(&info.nickname);
        info.group = strip_control(&info.group);
        info.host = strip_control(&info.host);
        info.user = strip_control(&info.user);
        info.last_seen = now_secs();
        info.key = info.ip.clone();
        let mut peers = self.peers.lock().unwrap();
        match peers.get_mut(&info.key) {
            Some(existing) => {
                existing.last_seen = info.last_seen;
                existing.port = info.port;
                if !info.nickname.is_empty() {
                    existing.nickname = info.nickname;
                }
                if !info.group.is_empty() {
                    existing.group = info.group;
                }
                if !info.host.is_empty() {
                    existing.host = info.host;
                }
                if !info.user.is_empty() {
                    existing.user = info.user;
                }
                false
            }
            None => {
                peers.insert(info.key.clone(), info);
                true
            }
        }
    }

    /// 记一次送达确认，返回这是第几次（从 0 开始）。
    /// 对端如果没认我们的确认会重发同一包号，据此可以换一种写法再确认。
    pub fn bump_ack(&self, ip: IpAddr, pkt: u32) -> u32 {
        let mut m = self.ack_count.lock().unwrap();
        if m.len() > 4096 {
            m.clear();
        }
        let e = m.entry((ip, pkt)).or_insert(0);
        let n = *e;
        *e += 1;
        n
    }

    /// 取文件请求方言：把该对端上次成功的那种排到最前面
    pub fn dialect_order(&self, ip: &str, total: usize) -> Vec<usize> {
        let first = self.dialect.lock().unwrap().get(ip).copied();
        let mut out: Vec<usize> = Vec::with_capacity(total);
        if let Some(i) = first.filter(|i| *i < total) {
            out.push(i);
        }
        out.extend((0..total).filter(|i| Some(*i) != first));
        out
    }

    /// 记住该对端可用的请求方言
    pub fn remember_dialect(&self, ip: &str, idx: usize) {
        self.dialect.lock().unwrap().insert(ip.to_string(), idx);
    }

    /// 清理超过 TTL 未被领取的文件槽（每次登记新文件时顺带执行）
    pub fn prune_offered(&self) {
        let cutoff = now_secs().saturating_sub(OFFER_TTL_SECS);
        self.offered.lock().unwrap().retain(|_, o| o.ts >= cutoff);
    }

    pub fn remove_peer(&self, key: &str) -> Option<PeerInfo> {
        self.peers.lock().unwrap().remove(key)
    }

    pub fn touch_peer(&self, key: &str) {
        if let Some(p) = self.peers.lock().unwrap().get_mut(key) {
            p.last_seen = now_secs();
        }
    }

    /// 清理超时未刷新的用户（默认 30 分钟），返回被移除的 key 列表
    pub fn prune_stale_peers(&self, timeout_secs: u64) -> Vec<String> {
        let cutoff = now_secs().saturating_sub(timeout_secs);
        let mut peers = self.peers.lock().unwrap();
        let stale: Vec<String> = peers
            .iter()
            .filter(|(_, p)| p.last_seen < cutoff)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &stale {
            peers.remove(k);
        }
        stale
    }

    /* ---------- 去重（UDP 可能重复投递） ---------- */

    /// 返回 true 表示首次出现；false 表示重复包应丢弃
    pub fn mark_seen(&self, ip: IpAddr, pkt_no: u32) -> bool {
        let item = (ip, pkt_no);
        let mut set = self.seen_set.lock().unwrap();
        if !set.insert(item) {
            return false;
        }
        let mut queue = self.seen_queue.lock().unwrap();
        queue.push_back(item);
        while queue.len() > SEEN_CAP {
            if let Some(old) = queue.pop_front() {
                set.remove(&old);
            }
        }
        true
    }

    /// 持久化失败时撤销内存去重占位，让后续 UDP 重投能再次尝试落库。
    pub fn forget_seen(&self, ip: IpAddr, pkt_no: u32) {
        let item = (ip, pkt_no);
        let mut set = self.seen_set.lock().unwrap();
        set.remove(&item);
        let mut queue = self.seen_queue.lock().unwrap();
        queue.retain(|entry| *entry != item);
    }

    /* ---------- 端到端加密：本机密钥与对端公钥缓存 ---------- */

    fn own_key_path(&self) -> PathBuf {
        self.data_dir.join("ipmsg_key.json")
    }

    fn peer_keys_path(&self) -> PathBuf {
        self.data_dir.join("peer_keys.json")
    }

    /// 本机 RSA 密钥对（懒加载）：进程内只生成/读取一次。
    /// 首次调用时读 `ipmsg_key.json`，有且合法 → 加载；无 → 生成并落盘；
    /// 文件损坏 → 记 diag 后重新生成并覆盖写回，绝不因坏文件而 panic。
    pub fn own_keypair(&self) -> Arc<KeyPair> {
        self.own_key
            .get_or_init(|| {
                let path = self.own_key_path();
                if let Ok(content) = std::fs::read_to_string(&path) {
                    match KeyPair::from_json(&content) {
                        Ok(kp) => return Arc::new(kp),
                        Err(e) => self.diag(&format!("own-key 密钥文件损坏，重新生成：{e}")),
                    }
                }
                let kp = Arc::new(KeyPair::generate().expect("RSA-2048 密钥生成失败"));
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&path, kp.to_json()) {
                    self.diag(&format!("own-key 密钥落盘失败：{e}"));
                }
                // 私钥文件是未加密 PKCS#8：落盘后立即收紧为属主可读写，
                // 覆盖 umask 默认（0644 会把私钥暴露给同机其它用户）。
                // 初次生成与损坏重写共用这一处写入，两条路径都生效。
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &path,
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
                kp
            })
            .clone()
    }

    /// 本机公钥指纹（设置页核对用；get_config 透出）
    pub fn fingerprint(&self) -> String {
        self.own_keypair().fingerprint()
    }

    /// 对端最近一次公告的公钥（未缓存返回 None；发送加密用最新一把）
    pub fn peer_pubkey(&self, ip: &str) -> Option<RsaPublicKey> {
        self.peer_crypto.lock().unwrap().get(ip).map(|e| e.pub_key.clone())
    }

    /// 对端最近一次公告的能力位（未缓存返回 0）
    pub fn peer_capa(&self, ip: &str) -> u32 {
        self.peer_crypto.lock().unwrap().get(ip).map(|e| e.capa).unwrap_or(0)
    }

    /// 验签候选公钥（最新在前、上一把备选在后）：多客户端交替/多实例场景下
    /// 任一一把命中即验签通过
    pub fn peer_pubkeys(&self, ip: &str) -> Vec<RsaPublicKey> {
        let m = self.peer_crypto.lock().unwrap();
        match m.get(ip) {
            Some(e) => {
                let mut v = vec![e.pub_key.clone()];
                if let Some(p) = &e.prev_key {
                    if p != &e.pub_key {
                        v.push(p.clone());
                    }
                }
                v
            }
            None => Vec::new(),
        }
    }

    /// 缓存对端公钥并持久化到 peer_keys.json（重启不丢）
    pub fn remember_peer_key(&self, ip: &str, capa: u32, pubk: &RsaPublicKey) {
        let (same_key, prev): (bool, Option<RsaPublicKey>) = {
            let m = self.peer_crypto.lock().unwrap();
            match m.get(ip) {
                Some(e) => (&e.pub_key == pubk, e.prev_key.clone()),
                None => (false, None),
            }
        };
        if same_key {
            // 同一把钥：只更新能力位（如应答重复），备选保持不变
            let mut m = self.peer_crypto.lock().unwrap();
            m.insert(ip.to_string(), PeerCryptoEntry {
                capa,
                pub_key: pubk.clone(),
                prev_key: prev,
            });
            return;
        }
        let prev = if prev.is_none() {
            let m = self.peer_crypto.lock().unwrap();
            match m.get(ip) {
                Some(e) => {
                    use rsa::traits::PublicKeyParts;
                    let old_fp = &e.pub_key.n().to_bytes_be()[..3];
                    let new_fp = &pubk.n().to_bytes_be()[..3];
                    self.diag(&format!(
                        "peer-key-change {ip} capa={:X} 指纹 {old_fp:02x?} → {new_fp:02x?}（保留旧钥作备选）",
                        capa
                    ));
                    Some(e.pub_key.clone()) // 旧钥降为备选
                }
                None => None,
            }
        } else {
            prev
        };
        {
            let mut m = self.peer_crypto.lock().unwrap();
            m.insert(ip.to_string(), PeerCryptoEntry {
                capa,
                pub_key: pubk.clone(),
                prev_key: prev.clone(),
            });
            if m.len() > PEER_KEY_CAP {
                // 防御性上限：异常洪泛时不无限膨胀。保留当前这条，其余清空，
                // 避免把刚学到的对端也丢掉、或把空表写回磁盘
                m.clear();
                m.insert(ip.to_string(), PeerCryptoEntry {
                    capa,
                    pub_key: pubk.clone(),
                    prev_key: prev,
                });
            }
        }
        self.persist_peer_keys();
    }

    /// 撤回对端公钥缓存并持久化（能力撤回规则）。
    ///
    /// 先前广告过加密能力的对端重新上线时不再声明 ENCRYPTOPT，说明对方已
    /// 关闭加密 —— 继续持有旧公钥会让我方误发密文、对方永远解不开。
    /// 未缓存的对端是安全空操作；只影响目标 IP，其它缓存原样保留。
    pub fn forget_peer_key(&self, ip: &str) {
        let removed = self.peer_crypto.lock().unwrap().remove(ip).is_some();
        if removed {
            self.persist_peer_keys();
        }
    }

    fn persist_peer_keys(&self) {
        use rsa::traits::PublicKeyParts;
        let data = self.peer_crypto.lock().unwrap();
        let keys: HashMap<String, PeerKeyEntry> = data
            .iter()
            .map(|(ip, e)| {
                (
                    ip.clone(),
                    PeerKeyEntry {
                        capa: e.capa,
                        n_b64: b64_encode(&e.pub_key.n().to_bytes_be()),
                        e_b64: b64_encode(&e.pub_key.e().to_bytes_be()),
                    },
                )
            })
            .collect();
        drop(data);
        let bytes = serde_json::to_vec(&PeerKeyFile { rev: PEER_KEY_FILE_REV, keys }).unwrap_or_default();
        if let Some(dir) = self.peer_keys_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(self.peer_keys_path(), bytes);
    }

    /// 启动时从磁盘恢复对端密钥缓存；单条损坏跳过该条，整体损坏视为无缓存。
    /// 版本不符（rev<2，旧公钥字节序错误）时整体作废，等待重新握手。
    pub fn load_peer_keys(&self) {
        let Ok(content) = std::fs::read_to_string(self.peer_keys_path()) else {
            return;
        };
        let Ok(file) = serde_json::from_str::<PeerKeyFile>(&content) else {
            return;
        };
        if file.rev != PEER_KEY_FILE_REV {
            return;
        }
        let map = file.keys;
        let mut out: HashMap<String, (u32, RsaPublicKey)> = HashMap::new();
        for (ip, ent) in map {
            let (Some(n), Some(e)) = (b64_decode(&ent.n_b64), b64_decode(&ent.e_b64)) else {
                continue;
            };
            if let Ok(pubk) =
                RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
            {
                out.insert(ip, (ent.capa, pubk));
            }
        }
        *self.peer_crypto.lock().unwrap() = out
            .into_iter()
            .map(|(ip, (capa, k))| {
                (
                    ip,
                    PeerCryptoEntry {
                        capa,
                        pub_key: k,
                        prev_key: None,
                    },
                )
            })
            .collect();
    }

    /// 标记该对端只走明文协议（仅内存态：重启后按报文重新协商）
    pub fn mark_peer_plain(&self, ip: &str) {
        self.peer_plain.lock().unwrap().insert(ip.to_string());
    }

    pub fn peer_marked_plain(&self, ip: &str) -> bool {
        self.peer_plain.lock().unwrap().contains(ip)
    }

    /// 记一次 GETPUBKEY 探测（send_getpubkey 发出时调用），返回含本次的累计次数
    pub fn record_probe(&self, ip: &str) -> u32 {
        let mut m = self.probe_counts.lock().unwrap();
        let c = m.entry(ip.to_string()).or_insert(0);
        *c += 1;
        *c
    }

    /// 该 IP 的历史 GETPUBKEY 探测次数（无记录为 0）
    pub fn probe_count(&self, ip: &str) -> u32 {
        self.probe_counts.lock().unwrap().get(ip).copied().unwrap_or(0)
    }

    /// send_message 无缓存分支的探测决策（spec §5）：
    /// - 预算未用尽 → false，调用方继续发 GETPUBKEY；
    /// - 恰在阈值穿越点 → true 并把对端标记为明文（只发生一次），
    ///   此后消息经 peer_marked_plain 静默走明文、不再探测。
    ///
    /// 与规格的偏差：预算是纯内存计数，重启即满血重来；规格原文是
    /// 「对方重新上线广播后重置」。这里放宽为进程生命周期粒度 —— 不持久化
    /// 误标结果，对端真上线后一条 ENCRYPTOPT 报文即可重新握手。
    /// 自愈重握手限频闸门：同 IP 每 REHANDSHAKE_INTERVAL 秒至多一次，
    /// 避免对端换钥/缓存错乱时触发握手风暴
    pub fn try_rehandshake_gate(&self, ip: &str) -> bool {
        const REHANDSHAKE_INTERVAL: u64 = 10;
        let now = now_secs();
        let mut m = self.rehandshake_at.lock().unwrap();
        match m.get(ip) {
            Some(t) if now.saturating_sub(*t) < REHANDSHAKE_INTERVAL => false,
            _ => {
                m.insert(ip.to_string(), now);
                true
            }
        }
    }

    pub fn retire_probe_budget(&self, ip: &str) -> bool {
        if self.probe_count(ip) < PLAIN_PROBE_BUDGET {
            return false;
        }
        if !self.peer_marked_plain(ip) {
            self.mark_peer_plain(ip);
        }
        true
    }

    /* ---------- 离线消息待投递队列 ---------- */

    fn pending_path(&self) -> PathBuf {
        self.data_dir.join("pending_out.json")
    }

    fn persist_pending(&self) {
        let data = self.pending_out.lock().unwrap();
        let bytes = serde_json::to_vec(&*data).unwrap_or_default();
        drop(data);
        if let Some(dir) = self.pending_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(self.pending_path(), bytes);
    }

    /// 启动时从磁盘恢复待投递队列（重启不丢，官方 IPMsg 语义）
    pub fn load_pending(&self) {
        let Ok(content) = std::fs::read_to_string(self.pending_path()) else {
            return;
        };
        if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<PendingOut>>>(&content) {
            *self.pending_out.lock().unwrap() = map;
        }
    }

    /// 入队一条离线消息；同 key 同包号已存在时不重复入队，返回是否新增
    pub fn enqueue_pending(&self, item: PendingOut) -> bool {
        let mut map = self.pending_out.lock().unwrap();
        let q = map.entry(item.key.clone()).or_default();
        if q.iter().any(|p| p.pkt == item.pkt) {
            return false;
        }
        q.push(item);
        drop(map);
        self.persist_pending();
        true
    }

    /// 某会话的待投递列表（副本，FIFO 顺序；投递复查用，不取出）
    pub fn pending_for(&self, key: &str) -> Vec<PendingOut> {
        self.pending_out
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// 取出某会话全部待投递并清空（当前仅测试用；线上投递走 ack 出队）
    pub fn take_pending(&self, key: &str) -> Vec<PendingOut> {
        let mut map = self.pending_out.lock().unwrap();
        let taken = map.remove(key).unwrap_or_default();
        drop(map);
        if !taken.is_empty() {
            self.persist_pending();
        }
        taken
    }

    /// 对端回 RECVMSG（送达确认）：把该会话对应包号的待投递出队，返回是否有变更
    pub fn ack_pending(&self, key: &str, pkt: u32) -> bool {
        let mut map = self.pending_out.lock().unwrap();
        let Some(q) = map.get_mut(key) else {
            return false;
        };
        let before = q.len();
        q.retain(|p| p.pkt != pkt);
        let changed = q.len() != before;
        drop(map);
        if changed {
            self.persist_pending();
        }
        changed
    }

    /* ---------- 在线消息送达重发（官方 §4-12 確認・リトライ） ---------- */

    /// 登记一条在线发送、等待 RECVMSG 确认的消息（仅纯文本，附件重发语义
    /// 复杂且公告/槽位需一致，暂不重发附件）。
    pub fn enqueue_retry(&self, item: RetryOut) {
        let mut map = self.retry_out.lock().unwrap();
        map.insert((item.key.clone(), item.pkt), item);
    }

    /// 某会话的所有待确认消息（副本）
    pub fn retry_for(&self, key: &str) -> Vec<RetryOut> {
        self.retry_out
            .lock()
            .unwrap()
            .iter()
            .filter(|((k, _), _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// RECVMSG 确认：出队重发项，返回是否有变更
    pub fn ack_retry(&self, key: &str, pkt: u32) -> bool {
        let mut map = self.retry_out.lock().unwrap();
        map.remove(&(key.to_string(), pkt)).is_some()
    }

    /// 重发次数 +1；超过上限返回 false（调用方出队放弃）
    pub fn bump_retry(&self, key: &str, pkt: u32) -> bool {
        let mut map = self.retry_out.lock().unwrap();
        match map.get_mut(&(key.to_string(), pkt)) {
            Some(item) => {
                item.attempts += 1;
                item.attempts <= crate::net::RETRY_MAX
            }
            None => false,
        }
    }

    /// 清理某会话全部重发项（对端离线/退场时）
    pub fn clear_retry(&self, key: &str) {
        let mut map = self.retry_out.lock().unwrap();
        map.retain(|(k, _), _| k != key);
    }

    /// 全部待确认重发项的会话 key（去重）
    pub fn retry_keys(&self) -> Vec<String> {
        let map = self.retry_out.lock().unwrap();
        let mut keys: Vec<String> = map.keys().map(|(k, _)| k.clone()).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// 更新重发项的最近发送时刻（timestamp 复用 ts 字段）
    pub fn touch_retry_sent(&self, key: &str, pkt: u32, now: u64) {
        let mut map = self.retry_out.lock().unwrap();
        if let Some(item) = map.get_mut(&(key.to_string(), pkt)) {
            item.ts = now;
        }
    }

    /// 对端离线（BR_EXIT）：未确认的在线消息转入待投递队列（对方回来补投）
    pub fn demote_retry_to_pending(&self, key: &str) {
        let items: Vec<RetryOut> = self.retry_for(key);
        if items.is_empty() {
            return;
        }
        for it in items {
            self.enqueue_pending(PendingOut {
                key: key.to_string(),
                pkt: it.pkt,
                text: it.text.clone(),
                ts: it.ts,
                paths: it.paths.clone(),
            });
        }
        self.clear_retry(key);
    }

    /* ---------- 主机列表窗口（BR_ISGETLIST/OKGETLIST/GETLIST/ANSLIST） ---------- */

    pub fn open_hostlist_window(&self, secs: u64) {
        *self.hostlist_window.lock().unwrap() = Some(now_secs() + secs);
    }

    pub fn hostlist_window_open(&self) -> bool {
        match *self.hostlist_window.lock().unwrap() {
            Some(until) => now_secs() < until,
            None => false,
        }
    }

    /* ---------- 成员主（DIR_MASTER）全网主机列表 ---------- */

    pub fn master_hosts(&self) -> Vec<crate::ipdict::Dict> {
        self.master_hosts.lock().unwrap().clone()
    }

    /// 合并一批评点到的全网主机（按 IPAD 去重替换）；返回变更数
    pub fn merge_master_hosts(&self, hosts: &[crate::ipdict::Dict]) -> usize {
        let mut map = self.master_hosts.lock().unwrap();
        let mut changed = 0;
        for h in hosts {
            let ip = h
                .get_str(crate::ipdict::DICT_IPAD)
                .unwrap_or("")
                .to_string();
            if ip.is_empty() {
                continue;
            }
            let before = map.len();
            map.retain(|m| m.get_str(crate::ipdict::DICT_IPAD) != Some(ip.as_str()));
            map.push(h.clone());
            if map.len() != before || !map.iter().any(|m| m == h) {
                changed += 1;
            }
        }
        changed
    }

    /* ---------- 成员侧 agent 有效期（DIR_POLLAGENT 的 AGS） ---------- */

    pub fn set_agent_until(&self, master_ip: &str, secs_from_now: u64) {
        self.agent_until
            .lock()
            .unwrap()
            .insert(master_ip.to_string(), now_secs() + secs_from_now);
    }

    pub fn agent_active(&self, master_ip: &str) -> bool {
        match self.agent_until.lock().unwrap().get(master_ip) {
            Some(until) => now_secs() < *until,
            None => false,
        }
    }

    /* ---------- AGENT 中继登记 ---------- */

    /// 客户端侧：记下「与 key 会话的应答要走 agent 中转」
    pub fn remember_relay_agent(&self, key: &str, agent: std::net::SocketAddr) {
        self.relay_agent
            .lock()
            .unwrap()
            .insert(key.to_string(), agent);
    }

    pub fn relay_agent(&self, key: &str) -> Option<std::net::SocketAddr> {
        self.relay_agent.lock().unwrap().get(key).copied()
    }

    /// 代理侧：登记发来 AGENT_PACKET 的真实对端地址（转发目标）
    pub fn remember_relay_peer(&self, ip: &str, addr: std::net::SocketAddr) {
        self.relay_peers
            .lock()
            .unwrap()
            .insert(ip.to_string(), addr);
    }

    pub fn relay_peer(&self, ip: &str) -> Option<std::net::SocketAddr> {
        self.relay_peers.lock().unwrap().get(ip).copied()
    }

    /* ---------- 不在模式 / 密码 ---------- */

    /// 设置不在模式并返回旧值（调用方负责广播 BR_ABSENCE）
    pub fn set_absence(&self, on: bool, text: &str) -> bool {
        let mut cfg = self.config.lock().unwrap();
        let old = cfg.absence_enabled;
        cfg.absence_enabled = on;
        if !text.trim().is_empty() {
            cfg.absence_text = text.trim().to_string();
        }
        drop(cfg);
        let _ = self.persist_config();
        old
    }

    /// 缓存对端不在通知文；同 key 覆盖。返回旧值
    pub fn set_peer_absence(&self, key: &str, text: &str) -> Option<String> {
        self.peer_absence
            .lock()
            .unwrap()
            .insert(key.to_string(), text.to_string())
    }

    pub fn peer_absence_of(&self, key: &str) -> Option<String> {
        self.peer_absence.lock().unwrap().get(key).cloned()
    }

    /// 对端退出（BR_EXIT）或重新通告时（BR_ABSENCE 无不在标记）清除缓存
    pub fn clear_peer_absence(&self, key: &str) {
        self.peer_absence.lock().unwrap().remove(key);
    }

    /* ---------- 成员主登记（DIR_MASTER） ---------- */

    pub fn upsert_dir_member(&self, m: DirMember) {
        self.dir_members.lock().unwrap().insert(m.key.clone(), m);
    }

    pub fn dir_member(&self, key: &str) -> Option<DirMember> {
        self.dir_members.lock().unwrap().get(key).cloned()
    }

    pub fn dir_members_snapshot(&self) -> Vec<DirMember> {
        self.dir_members
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    pub fn remove_dir_member(&self, key: &str) {
        self.dir_members.lock().unwrap().remove(key);
    }

    /// 成员主模式下冷超时的 POLL 成员清理（返回被清理的 key）
    pub fn prune_dir_members(&self, timeout_secs: u64) -> Vec<String> {
        let now = now_secs();
        let mut map = self.dir_members.lock().unwrap();
        let stale: Vec<String> = map
            .iter()
            .filter(|(_, m)| now.saturating_sub(m.last_poll) > timeout_secs)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &stale {
            map.remove(k);
        }
        stale
    }

    /* ---------- 聊天记录 ---------- */

    /// 启动迁移：旧版会话键是 `ip:端口`，历史文件因此叫 `<ip>_<端口>.jsonl`。
    /// 现在身份键归一化为纯 IP，把这类文件改名为 `<ip>.jsonl`，并同步把记录内
    /// `peer.key` 快照（`ip:port`）改写成裸 IP —— 否则搜索跳转、已读回执等
    /// 按记录内 key 寻址的路径会找不到会话。目标文件已存在时保留旧文件不动
    /// （正常升级流程不会发生，不做有损合并）。返回迁移的文件数。
    pub fn migrate_legacy_history_keys(&self) -> usize {
        let _g = self.hist_lock.lock().unwrap();
        let Ok(rd) = std::fs::read_dir(&self.logs_dir) else {
            return 0;
        };
        let mut moved = 0usize;
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // 仅匹配 `<ipv4>_<port>` 形状；其余命名一律不动
            let Some((ip_part, port_part)) = stem.rsplit_once('_') else {
                continue;
            };
            if ip_part.parse::<std::net::Ipv4Addr>().is_err()
                || port_part.parse::<u16>().is_err()
            {
                continue;
            }
            let target = self.logs_dir.join(format!("{ip_part}.jsonl"));
            if target.exists() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let prefix = format!("{ip_part}:");
            let mut out = String::with_capacity(content.len());
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(mut rec) => {
                        if let Some(kv) = rec.get_mut("peer").and_then(|p| p.get_mut("key")) {
                            if kv.as_str().is_some_and(|s| s.starts_with(&prefix)) {
                                *kv = serde_json::Value::String(ip_part.to_string());
                            }
                        }
                        out.push_str(&rec.to_string());
                        out.push('\n');
                    }
                    // 坏行原样搬运，交给 compact_histories 统一清理
                    Err(_) => {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
            if std::fs::write(&target, &out).is_ok() {
                let _ = std::fs::remove_file(&path);
                moved += 1;
            }
        }
        moved
    }

    /// 会话 key → 聊天记录文件路径（文件名做了安全清洗）
    pub(crate) fn log_path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
            .collect();
        self.logs_dir.join(format!("{}.jsonl", safe))
    }

    fn append_record_locked(
        &self,
        path: &std::path::Path,
        rec: &serde_json::Value,
    ) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        // 整行一次 write_all：writeln! 会把 JSON 拆成多次 write 系统调用，
        // 追加模式下与其它写入者交错就会写出无法解析的坏行。
        file.write_all(format!("{rec}\n").as_bytes())
    }

    fn rewrite_records_locked(
        &self,
        path: &std::path::Path,
        lines: &[String],
    ) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        if !lines.is_empty() {
            file.write_all(lines.join("\n").as_bytes())?;
            file.write_all(b"\n")?;
        }
        Ok(())
    }

    /// 可失败的追加接口：目录创建、打开和 write_all 全部成功后才算落库。
    pub fn log_record_fallible(
        &self,
        key: &str,
        rec: &serde_json::Value,
    ) -> std::io::Result<()> {
        let _guard = self.hist_lock.lock().unwrap();
        self.append_record_locked(&self.log_path(key), rec)
    }

    /// 兼容旧调用方的便利封装；这些路径沿用忽略历史错误的旧行为。新的入站
    /// 接收代码必须调用上面的可失败接口。
    pub fn log_record(&self, key: &str, rec: &serde_json::Value) {
        let _ = self.log_record_fallible(key, rec);
    }

    /// 落库一条入站记录：同包号的旧记录存在则原地更新，否则追加。
    ///
    /// 对端的"延迟发送/离线重发"会用同一包号反复投递同一条消息（飞秋等实现
    /// 每次我方上线都会重发），逐条追加会让历史无限膨胀，更要命的是每份新副本
    /// 都是未读状态，前端一标记已读就再回一次 READMSG，对端于是反复弹
    /// "消息已被查看"。payload identity 让判断跨进程重启仍有效，并让同一
    /// 临界区里的持久化结果成为并发投递的唯一权威。
    pub fn upsert_in_record_fallible(
        &self,
        key: &str,
        rec: &serde_json::Value,
    ) -> std::io::Result<InRecordOutcome> {
        let _guard = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        let pkt = rec.get("pkt").and_then(|v| v.as_u64());
        let Some(pkt) = pkt else {
            self.append_record_locked(&path, rec)?;
            return Ok(InRecordOutcome::Inserted);
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.append_record_locked(&path, rec)?;
                return Ok(InRecordOutcome::Inserted);
            }
            Err(error) => return Err(error),
        };
        let new_identity = rec.get("payload_id").and_then(|value| value.as_str());
        let mut parsed_lines: Vec<(String, Option<serde_json::Value>)> = Vec::new();
        let mut matching_indices = Vec::new();
        for line in content.lines() {
            let parsed = serde_json::from_str::<serde_json::Value>(line).ok();
            if parsed.as_ref().is_some_and(|old| {
                old.get("dir").and_then(|value| value.as_str()) == Some("in")
                    && old.get("pkt").and_then(|value| value.as_u64()) == Some(pkt)
            }) {
                matching_indices.push(parsed_lines.len());
            }
            parsed_lines.push((line.to_string(), parsed));
        }

        if matching_indices.iter().any(|index| {
            let old_identity = parsed_lines[*index]
                .1
                .as_ref()
                .and_then(|old| old.get("payload_id"))
                .and_then(|value| value.as_str());
            new_identity.is_some() && old_identity == new_identity
        }) {
            return Ok(InRecordOutcome::Duplicate);
        }

        if let Some(first_index) = matching_indices.first().copied() {
            let legacy_without_identity = new_identity.is_none()
                && parsed_lines[first_index]
                    .1
                    .as_ref()
                    .and_then(|old| old.get("payload_id"))
                    .is_none();
            let mut replacement = rec.clone();
            if legacy_without_identity {
                // 导入记录和旧版无 identity 记录沿用旧封装行为；已认证入站
                // 报文总会提供 identity，不走此兼容分支。
                if let Some(timestamp) = parsed_lines[first_index]
                    .1
                    .as_ref()
                    .and_then(|old| old.get("ts"))
                {
                    replacement["ts"] = timestamp.clone();
                }
            }
            let matching: HashSet<usize> = matching_indices.into_iter().collect();
            let mut lines = Vec::with_capacity(parsed_lines.len());
            for (index, (original, parsed)) in parsed_lines.into_iter().enumerate() {
                if index == first_index {
                    lines.push(replacement.to_string());
                } else if !matching.contains(&index) {
                    lines.push(parsed.map_or(original, |value| value.to_string()));
                }
            }
            self.rewrite_records_locked(&path, &lines)?;
            return Ok(InRecordOutcome::ReplacedConflict);
        }

        self.append_record_locked(&path, rec)?;
        Ok(InRecordOutcome::Inserted)
    }

    /// 兼容封装：只有新追加包返回 true；需要真实 I/O 状态或冲突详情的调用方
    /// 必须使用可失败接口。
    pub fn upsert_in_record(&self, key: &str, rec: &serde_json::Value) -> bool {
        matches!(
            self.upsert_in_record_fallible(key, rec),
            Ok(InRecordOutcome::Inserted)
        )
    }

    /// 全文搜索聊天记录。
    ///
    /// `key` 为 None 时搜索全部会话。匹配正文与附件文件名（大小写不敏感），
    /// 结果按时间倒序返回，最多 `limit` 条。会话 key 取自记录里的 peer.key，
    /// 取不到时退化用文件名（旧记录兜底）。
    pub fn search_history(
        &self,
        query: &str,
        key: Option<&str>,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let _g = self.hist_lock.lock().unwrap();

        let files: Vec<PathBuf> = match key {
            Some(k) => vec![self.log_path(k)],
            None => match std::fs::read_dir(&self.logs_dir) {
                Ok(rd) => rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false))
                    .collect(),
                Err(_) => return Vec::new(),
            },
        };

        let mut hits: Vec<serde_json::Value> = Vec::new();
        for path in files {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let fallback_key = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            for line in content.lines() {
                let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let text = rec.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let file_names: Vec<String> = rec
                    .get("files")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.get("name").and_then(|v| v.as_str()))
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default();

                let in_text = text.to_lowercase().contains(&needle);
                let in_files = file_names
                    .iter()
                    .any(|n| n.to_lowercase().contains(&needle));
                if !in_text && !in_files {
                    continue;
                }

                let peer = rec.get("peer");
                let sess = peer
                    .and_then(|p| p.get("key"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| fallback_key.clone());
                let nickname = peer
                    .and_then(|p| p.get("nickname"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                hits.push(serde_json::json!({
                    "key": sess,
                    "pkt": rec.get("pkt").and_then(|v| v.as_u64()),
                    "ts": rec.get("ts").and_then(|v| v.as_u64()).unwrap_or(0),
                    "dir": rec.get("dir").and_then(|v| v.as_str()).unwrap_or(""),
                    "kind": rec.get("kind").and_then(|v| v.as_str()).unwrap_or("text"),
                    "text": text,
                    "files": file_names,
                    "nickname": nickname,
                    "hit": if in_text { "text" } else { "file" },
                }));
            }
        }
        // 时间倒序，最近的排前面
        hits.sort_by(|a, b| {
            b["ts"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["ts"].as_u64().unwrap_or(0))
        });
        hits.truncate(limit);
        hits
    }

    /// 遍历历史记录文件，返回全部会话摘要（含离线会话，中栏展示用）。
    ///
    /// key/昵称/群组以记录内 peer 快照为准（迁移后已是纯 IP 键），
    /// 无快照时退化为文件名；时间为该会话最大消息 ts，倒序返回。
    /// 防御性跳过旧版 `<ipv4>_<port>` 命名（迁移遗漏时不当成会话）。
    /// 被用户删除（隐藏）的会话直接排除——文件虽已删除，仍防御性过滤。
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let _g = self.hist_lock.lock().unwrap();
        let Ok(rd) = std::fs::read_dir(&self.logs_dir) else {
            return Vec::new();
        };
        let mut out: Vec<SessionInfo> = Vec::new();
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Some((ip_part, port_part)) = stem.rsplit_once('_') {
                if ip_part.parse::<std::net::Ipv4Addr>().is_ok()
                    && port_part.parse::<u16>().is_ok()
                {
                    continue;
                }
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut key = stem.clone();
            let (mut nickname, mut host, mut group) = (String::new(), String::new(), String::new());
            let mut last_ts = 0u64;
            let mut unread = 0u32;
            let mut unread_ts = 0u64;
            for line in content.lines() {
                let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let ts = rec.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
                if ts > last_ts {
                    last_ts = ts;
                }
                // 未读统计放在 peer 快照解析之前：旧版缺 peer 字段的记录也参与
                // 计数（会话归属由文件名确定），保证前端启动补数不遗漏。
                // out 记录的 read 是「对端已读」，只有 in 记录参与。
                if rec.get("dir").and_then(|v| v.as_str()) == Some("in")
                    && !rec.get("read").and_then(|v| v.as_bool()).unwrap_or(false)
                {
                    unread += 1;
                    if ts > unread_ts {
                        unread_ts = ts;
                    }
                }
                let Some(peer) = rec.get("peer") else {
                    continue;
                };
                if key == stem {
                    if let Some(k) = peer.get("key").and_then(|v| v.as_str()) {
                        if !k.is_empty() {
                            key = k.to_string();
                        }
                    }
                }
                // 昵称/群组/主机取最新一条快照的非空值
                if let Some(n) = peer.get("nickname").and_then(|v| v.as_str()) {
                    if !n.is_empty() {
                        nickname = n.to_string();
                    }
                }
                if let Some(h) = peer.get("host").and_then(|v| v.as_str()) {
                    if !h.is_empty() {
                        host = h.to_string();
                    }
                }
                if let Some(g) = peer.get("group").and_then(|v| v.as_str()) {
                    if !g.is_empty() {
                        group = g.to_string();
                    }
                }
            }
            if self.is_hidden(&key) {
                continue;
            }
            out.push(SessionInfo {
                key,
                nickname,
                host,
                group,
                last_ts,
                unread,
                unread_ts,
            });
        }
        out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
        out
    }

    /// 清空某会话的聊天记录（删除对应的 JSONL 文件）。
    /// 返回被删掉的记录条数；文件本就不存在时返回 0。
    pub fn clear_history(&self, key: &str) -> usize {
        let _g = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        let n = std::fs::read_to_string(&path)
            .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0);
        let _ = std::fs::remove_file(&path);
        n
    }

    /* ---------- 删除会话（微信式隐藏） ---------- */

    fn hidden_path(&self) -> PathBuf {
        self.data_dir.join("hidden_contacts.json")
    }

    /// 该会话是否已被用户删除（隐藏等待对方再来消息恢复）
    pub fn is_hidden(&self, key: &str) -> bool {
        self.hidden_contacts.lock().unwrap().contains(key)
    }

    fn persist_hidden(&self) {
        let mut keys: Vec<String> =
            self.hidden_contacts.lock().unwrap().iter().cloned().collect();
        keys.sort();
        if let Some(dir) = self.hidden_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(
            self.hidden_path(),
            serde_json::to_vec_pretty(&keys).unwrap_or_default(),
        );
    }

    /// 删除某会话（微信式）：记入隐藏集合并删除本地聊天记录，
    /// 返回删掉的记录条数；对端再发消息时由 unhide_contact 恢复。
    pub fn delete_contact(&self, key: &str) -> usize {
        self.hidden_contacts.lock().unwrap().insert(key.to_string());
        self.persist_hidden();
        self.clear_history(key)
    }

    /// 对端发来新消息：把被删会话从隐藏集合移除并落盘。
    /// 返回是否确实做过恢复（原本就在隐藏集合里）。
    pub fn unhide_contact(&self, key: &str) -> bool {
        let mut hidden = self.hidden_contacts.lock().unwrap();
        if !hidden.remove(key) {
            return false;
        }
        drop(hidden);
        self.persist_hidden();
        true
    }

    /// 启动时整理历史文件：合并同包号的入站重复记录、丢弃无法解析的坏行。
    ///
    /// 早期版本会把对端每次重投的消息逐条追加，且用 `writeln!` 分多次写入，
    /// 并发追加时可能写出交错的坏行；这里做一次性修复。
    /// 返回 (合并掉的重复记录数, 丢弃的坏行数)。
    pub fn compact_histories(&self) -> (usize, usize) {
        let _g = self.hist_lock.lock().unwrap();
        let Ok(rd) = std::fs::read_dir(&self.logs_dir) else {
            return (0, 0);
        };
        let (mut merged, mut dropped) = (0usize, 0usize);
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut out: Vec<serde_json::Value> = Vec::new();
            // 入站包号 → 在 out 中的位置
            let mut seen: HashMap<u64, usize> = HashMap::new();
            let (mut file_merged, mut file_dropped) = (0usize, 0usize);
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
                    file_dropped += 1;
                    continue;
                };
                let is_in = rec.get("dir").and_then(|v| v.as_str()) == Some("in");
                let pkt = rec.get("pkt").and_then(|v| v.as_u64());
                match (is_in, pkt) {
                    (true, Some(pkt)) => match seen.get(&pkt) {
                        Some(&idx) => {
                            // 保留首次的时间戳与已读状态，内容用最新一份
                            let mut merged_rec = rec;
                            if let Some(ts) = out[idx].get("ts") {
                                merged_rec["ts"] = ts.clone();
                            }
                            if out[idx].get("read").and_then(|v| v.as_bool()) == Some(true) {
                                merged_rec["read"] = true.into();
                            }
                            out[idx] = merged_rec;
                            file_merged += 1;
                        }
                        None => {
                            seen.insert(pkt, out.len());
                            out.push(rec);
                        }
                    },
                    _ => out.push(rec),
                }
            }
            if file_merged > 0 || file_dropped > 0 {
                let body: String = out
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                if std::fs::write(&path, body + "\n").is_ok() {
                    merged += file_merged;
                    dropped += file_dropped;
                }
            }
        }
        (merged, dropped)
    }

    /// 过滤出「要求回执且尚未标记已读」的入站包号。
    /// 前端可能因焦点变化重复请求，这里以历史为准，保证一条消息只回执一次。
    pub fn pending_receipts(&self, key: &str, pkts: &[u32]) -> Vec<u32> {
        let want: HashSet<u32> = pkts.iter().copied().collect();
        let _g = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let mut out: Vec<u32> = Vec::new();
        for line in content.lines() {
            let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if rec.get("dir").and_then(|v| v.as_str()) != Some("in") {
                continue;
            }
            let Some(pkt) = rec.get("pkt").and_then(|v| v.as_u64()).map(|p| p as u32) else {
                continue;
            };
            let need = rec.get("need_read").and_then(|v| v.as_bool()).unwrap_or(false);
            let read = rec.get("read").and_then(|v| v.as_bool()).unwrap_or(false);
            let secret = rec.get("secret").and_then(|v| v.as_bool()).unwrap_or(false);
            let locked = rec.get("locked").and_then(|v| v.as_bool()).unwrap_or(false)
                || (secret
                    && !rec
                        .get("unlocked")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false));
            // 封书/密码锁未开封的消息不发已读回执（官方 recvdlg 开封才回 READMSG）
            if want.contains(&pkt) && need && !read && !locked && !out.contains(&pkt) {
                out.push(pkt);
            }
        }
        out
    }

    /// 读取某会话最近 limit 条记录（按时间升序返回）
    pub fn read_history(&self, key: &str, limit: usize) -> Vec<serde_json::Value> {
        let _g = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let all: Vec<serde_json::Value> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let skip = all.len().saturating_sub(limit);
        all[skip..].to_vec()
    }

    /// 通用历史重写：对满足条件的记录执行 mutate，有变更才回写
    fn rewrite_history(
        &self,
        key: &str,
        mut pred: impl FnMut(&serde_json::Value) -> bool,
        mutate: impl Fn(&mut serde_json::Value),
    ) -> bool {
        let _g = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return false;
        };
        let mut changed = false;
        let mut lines: Vec<String> = Vec::new();
        for line in content.lines() {
            let mut rec: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    lines.push(line.to_string());
                    continue;
                }
            };
            if pred(&rec) {
                mutate(&mut rec);
                changed = true;
            }
            lines.push(rec.to_string());
        }
        if changed {
            let _ = std::fs::write(path, lines.join("\n") + "\n");
        }
        changed
    }

    /// 更新某条消息中指定文件的状态（下载完成等）
    pub fn update_history_file(
        &self,
        key: &str,
        pkt: u32,
        file_id: u32,
        mutate: impl Fn(&mut serde_json::Value),
    ) {
        self.rewrite_history(
            key,
            |rec| {
                rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64)
                    && rec
                        .get("files")
                        .and_then(|f| f.as_array())
                        .map(|files| {
                            files
                                .iter()
                                .any(|f| f.get("id").and_then(|v| v.as_u64()) == Some(file_id as u64))
                        })
                        .unwrap_or(false)
            },
            move |rec| {
                if let Some(files) = rec.get_mut("files").and_then(|f| f.as_array_mut()) {
                    for f in files.iter_mut() {
                        if f.get("id").and_then(|v| v.as_u64()) == Some(file_id as u64) {
                            mutate(f);
                        }
                    }
                }
            },
        );
    }

    /// 按包号定位某条记录并就地改写（撤回标记、开封/解锁标记等）
    pub fn update_history_pkt(
        &self,
        key: &str,
        pkt: u32,
        mutate: impl Fn(&mut serde_json::Value),
    ) -> bool {
        self.rewrite_history(
            key,
            |rec| rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64),
            mutate,
        )
    }

    /// 读取一条记录里需要回执的标记（撤回/封书/密码锁等展示用）
    pub fn find_history_pkt(&self, key: &str, pkt: u32) -> Option<serde_json::Value> {
        self.find_in_record(key, pkt)
    }

    /// 历史里是否存在包含指定文本的记录（自检断言用）
    pub fn history_contains_text(&self, key: &str, text: &str) -> bool {
        self.read_history(key, 500).iter().any(|r| {
            r.get("text")
                .and_then(|v| v.as_str())
                .map(|t| t.contains(text))
                .unwrap_or(false)
        })
    }

    /// 标记入站消息为已读（本地状态）
    pub fn mark_in_read(&self, key: &str, pkts: &[u32]) {
        let set: std::collections::HashSet<u32> = pkts.iter().copied().collect();
        self.rewrite_history(
            key,
            |rec| {
                rec.get("dir").and_then(|v| v.as_str()) == Some("in")
                    && rec
                        .get("pkt")
                        .and_then(|v| v.as_u64())
                        .map(|p| set.contains(&(p as u32)))
                        .unwrap_or(false)
            },
            |rec| {
                rec["read"] = true.into();
            },
        );
    }

    /// 查找该会话中指定包号的最近一条入站记录（用于状态继承）
    pub fn find_in_record(&self, key: &str, pkt: u32) -> Option<serde_json::Value> {
        use std::io::BufRead;
        let path = self.log_path(key);
        let f = std::fs::File::open(&path).ok()?;
        let last = std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
            .filter(|rec| {
                rec.get("dir").and_then(|v| v.as_str()) == Some("in")
                    && rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64)
            })
            .last()?;
        Some(last)
    }

    /// 按包号查任意方向记录（撤回/开封等校验用；find_in_record 只查入站）
    pub fn find_history_any(&self, key: &str, pkt: u32) -> Option<serde_json::Value> {
        use std::io::BufRead;
        let path = self.log_path(key);
        let f = std::fs::File::open(&path).ok()?;
        std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
            .filter(|rec| rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64))
            .last()
    }

    /// 标记出站消息已被对端已读（收到 READMSG 回执），返回是否有变更
    pub fn mark_out_read(&self, key: &str, pkt: u32) -> bool {
        self.rewrite_history(
            key,
            |rec| {
                rec.get("dir").and_then(|v| v.as_str()) == Some("out")
                    && rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64)
            },
            |rec| {
                rec["read"] = true.into();
            },
        )
    }
}

/* ---------------- 单元测试 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!(
            "oim-state-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::new(dir)
    }

    /// 日志开关（--log 运行时参数）：
    /// - 默认关闭：diag() 不创建、不写入诊断文件
    /// - 打开后：diag() 写入 diag.log
    /// - 再关闭：不再追加
    #[test]
    fn diag_respects_log_switch() {
        let st = temp_state("diag");
        std::fs::create_dir_all(&st.data_dir).unwrap();

        // 默认关闭（与 --log 缺省一致）：不落盘
        set_log_enabled(false);
        st.diag("should-not-appear");
        assert!(
            !st.data_dir.join("diag.log").exists(),
            "默认关闭时 diag.log 不应被创建"
        );

        // --log 打开：写入
        set_log_enabled(true);
        st.diag("hello-diag");
        let content = std::fs::read_to_string(st.data_dir.join("diag.log")).unwrap();
        assert!(content.contains("hello-diag"), "打开开关后记录应落盘");

        // 关闭后不再追加
        set_log_enabled(false);
        st.diag("should-not-appear-2");
        let content2 = std::fs::read_to_string(st.data_dir.join("diag.log")).unwrap();
        assert!(!content2.contains("should-not-appear-2"), "关闭后不应再写");
    }

    #[test]
    fn config_persist_roundtrip() {
        let st = temp_state("config");
        std::fs::create_dir_all(&st.data_dir).unwrap();
        st.load_config();
        assert!(st.config().nickname.is_empty(), "首次运行昵称留空，由设置向导填写");
        assert!(!st.config().download_dir.is_empty(), "下载目录有默认值");
        let mut cfg = st.config();
        cfg.nickname = "测试昵称".into();
        cfg.group = "G1".into();
        cfg.lang = "en".into();
        st.set_config(cfg);
        st.persist_config().unwrap();

        let st2 = AppState::new(st.data_dir.clone());
        st2.load_config();
        assert_eq!(st2.config().nickname, "测试昵称");
        assert_eq!(st2.config().group, "G1");
        assert_eq!(st2.config().lang, "en", "界面语言持久化");

        // 旧版配置文件没有 lang 字段：回落空串（前端按系统语言探测）
        let path = st.data_dir.join("config.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("lang");
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let st3 = AppState::new(st.data_dir.clone());
        st3.load_config();
        assert!(st3.config().lang.is_empty(), "缺失字段回落空串（跟随系统）");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn seen_dedup() {
        let st = temp_state("seen");
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(st.mark_seen(ip, 1));
        assert!(!st.mark_seen(ip, 1));
        assert!(st.mark_seen(ip, 2));
        assert!(st.mark_seen("127.0.0.2".parse().unwrap(), 1));
    }

    #[test]
    fn seen_dedup_is_atomic_under_concurrent_insert() {
        let st = Arc::new(temp_state("seen-race"));
        let ip: IpAddr = "127.0.0.9".parse().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let handles: Vec<_> = (0..32)
            .map(|_| {
                let st = st.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    st.mark_seen(ip, 77)
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|winner| *winner)
            .count();

        assert_eq!(winners, 1, "同一个 (IP, PKT) 只能有一个首次插入者");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn history_append_read_update() {
        let st = temp_state("hist");
        let rec = serde_json::json!({
            "dir": "in", "kind": "file", "text": "hi", "pkt": 100, "ts": 1,
            "files": [{"id": 3, "name": "a.zip", "size": 5, "state": "pending"}]
        });
        st.log_record("192.168.1.9:2425", &rec);
        st.log_record("192.168.1.9:2425", &serde_json::json!({"dir":"out","text":"x","pkt":101,"ts":2}));
        let hist = st.read_history("192.168.1.9:2425", 10);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0]["pkt"], 100);

        st.update_history_file("192.168.1.9:2425", 100, 3, |f| {
            f["state"] = "done".into();
            f["path"] = "/tmp/a.zip".into();
        });
        let hist = st.read_history("192.168.1.9:2425", 10);
        assert_eq!(hist[0]["files"][0]["state"], "done");
        assert_eq!(hist[0]["files"][0]["path"], "/tmp/a.zip");

        // limit 生效：只取最后 1 条
        let hist = st.read_history("192.168.1.9:2425", 1);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0]["pkt"], 101);

        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn search_history_matches_text_and_filenames() {
        let st = temp_state("search");
        let a = "10.0.0.1:2425";
        let b = "10.0.0.2:2425";
        st.log_record(a, &serde_json::json!({
            "dir":"in","kind":"text","text":"明天下午开会","pkt":1,"ts":100,
            "peer":{"key":a,"nickname":"老王"}
        }));
        st.log_record(a, &serde_json::json!({
            "dir":"out","kind":"file","text":"","pkt":2,"ts":200,
            "files":[{"id":1,"name":"会议纪要.docx","size":10}],
            "peer":{"key":a,"nickname":"老王"}
        }));
        st.log_record(b, &serde_json::json!({
            "dir":"in","kind":"text","text":"Hello World","pkt":3,"ts":300,
            "peer":{"key":b,"nickname":"Tom"}
        }));

        // 全局搜索：命中正文
        let r = st.search_history("开会", None, 50);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["key"], a);
        assert_eq!(r[0]["hit"], "text");
        assert_eq!(r[0]["nickname"], "老王");

        // 命中附件名
        let r = st.search_history("纪要", None, 50);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["hit"], "file");
        assert_eq!(r[0]["pkt"], 2);

        // 大小写不敏感
        assert_eq!(st.search_history("hello", None, 50).len(), 1);
        assert_eq!(st.search_history("HELLO", None, 50).len(), 1);

        // 限定会话
        assert!(st.search_history("hello", Some(a), 50).is_empty());
        assert_eq!(st.search_history("hello", Some(b), 50).len(), 1);

        // 多条命中按时间倒序 + limit 生效
        let r = st.search_history("会", None, 50);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["ts"], 200, "最近的排前面");
        assert_eq!(st.search_history("会", None, 1).len(), 1);

        // 空查询不返回结果，避免把整个历史倒出来
        assert!(st.search_history("   ", None, 50).is_empty());
        assert!(st.search_history("找不到的词", None, 50).is_empty());
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn clear_history_removes_records() {
        let st = temp_state("clear");
        st.log_record("k:9", &serde_json::json!({"dir":"in","pkt":1,"ts":1,"text":"a"}));
        st.log_record("k:9", &serde_json::json!({"dir":"out","pkt":2,"ts":2,"text":"b"}));
        assert_eq!(st.read_history("k:9", 10).len(), 2);
        assert_eq!(st.clear_history("k:9"), 2);
        assert!(st.read_history("k:9", 10).is_empty());
        // 清空后仍可继续记录新消息
        st.log_record("k:9", &serde_json::json!({"dir":"in","pkt":3,"ts":3,"text":"c"}));
        assert_eq!(st.read_history("k:9", 10).len(), 1);
        // 没有历史的会话：清空是幂等的空操作
        assert_eq!(st.clear_history("k:none"), 0);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn delete_contact_hides_and_removes_history() {
        let st = temp_state("del-contact");
        st.log_record("10.0.0.9", &serde_json::json!({"dir":"in","pkt":1,"ts":1,"text":"a"}));
        st.log_record("10.0.0.9", &serde_json::json!({"dir":"out","pkt":2,"ts":2,"text":"b"}));
        assert_eq!(st.list_sessions().len(), 1);
        assert!(!st.is_hidden("10.0.0.9"));

        assert_eq!(st.delete_contact("10.0.0.9"), 2, "返回被删掉的记录条数");
        assert!(st.is_hidden("10.0.0.9"));
        assert!(st.list_sessions().is_empty(), "删除后会话不再出现在列表");
        assert!(st.read_history("10.0.0.9", 10).is_empty(), "记录文件已删除");

        // 对方重新发消息：历史重建，但列表仍隐藏（等待 unhide 恢复）
        st.log_record("10.0.0.9", &serde_json::json!({"dir":"in","pkt":3,"ts":3,"text":"c"}));
        assert!(st.list_sessions().is_empty(), "恢复前仍隐藏");

        // 收到对方消息（unhide_contact）后会话重新出现
        assert!(st.unhide_contact("10.0.0.9"));
        assert!(!st.unhide_contact("10.0.0.9"), "未隐藏的会话恢复是空操作");
        assert!(!st.is_hidden("10.0.0.9"));
        assert_eq!(st.list_sessions().len(), 1, "恢复后重新出现在列表");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn hidden_contacts_persist_across_reload() {
        let dir = std::env::temp_dir().join(format!("oim-hidden-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = AppState::new(dir.clone());
        st.log_record("192.168.1.5", &serde_json::json!({"dir":"in","pkt":1,"ts":1,"text":"a"}));
        st.delete_contact("192.168.1.5");
        assert!(st.is_hidden("192.168.1.5"));
        drop(st);

        // 重启：hidden_contacts.json 落盘，隐藏集合原样恢复
        let st2 = AppState::new(dir.clone());
        assert!(st2.is_hidden("192.168.1.5"));
        assert!(st2.list_sessions().is_empty());
        assert!(!st2.is_hidden("192.168.1.6"), "未删除的 key 不受影响");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_record_upsert_dedups_resends() {
        let st = temp_state("resend");
        let rec = serde_json::json!({
            "dir": "in", "kind": "text", "text": "原文", "pkt": 500, "ts": 100,
            "need_read": true, "read": false
        });
        assert!(st.upsert_in_record("k:1", &rec), "首次落库");
        assert_eq!(st.pending_receipts("k:1", &[500]), vec![500]);
        st.mark_in_read("k:1", &[500]);
        assert!(st.pending_receipts("k:1", &[500]).is_empty(), "已读后不再回执");

        // 对端重投：正文带尾注、状态继承已读
        let resend = serde_json::json!({
            "dir": "in", "kind": "text", "text": "原文\n(IPMsg Delayed Send)", "pkt": 500,
            "ts": 999, "need_read": true, "read": true
        });
        assert!(!st.upsert_in_record("k:1", &resend), "重投不算新消息");
        let hist = st.read_history("k:1", 50);
        assert_eq!(hist.len(), 1, "重投不追加新记录");
        assert_eq!(hist[0]["ts"], 100, "保留首次收到时间");
        assert!(hist[0]["text"].as_str().unwrap().contains("Delayed Send"));
        assert!(st.pending_receipts("k:1", &[500]).is_empty(), "重投不再回执");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn exact_payload_identity_duplicate_does_not_rewrite_persisted_state() {
        let st = temp_state("payload-id-duplicate");
        let first = serde_json::json!({
            "dir": "in", "kind": "file", "text": "same", "pkt": 501, "ts": 100,
            "payload_id": "sha256:abc", "read": true, "unlocked": true,
            "files": [{"id": 7, "state": "done", "path": "/kept/file.png"}]
        });
        assert!(st.upsert_in_record("k:payload", &first));
        let before = std::fs::read(st.log_path("k:payload")).unwrap();

        let retry = serde_json::json!({
            "dir": "in", "kind": "file", "text": "same", "pkt": 501, "ts": 999,
            "payload_id": "sha256:abc", "read": false, "unlocked": false,
            "files": [{"id": 7, "state": "downloading"}]
        });
        assert!(!st.upsert_in_record("k:payload", &retry));

        assert_eq!(std::fs::read(st.log_path("k:payload")).unwrap(), before);
        assert_eq!(st.find_in_record("k:payload", 501).unwrap()["read"], true);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn inbound_history_outcomes_survive_restart_and_distinguish_conflicts() {
        let dir = std::env::temp_dir().join(format!(
            "oim-state-outcome-restart-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let first = AppState::new(dir.clone());
        let original = serde_json::json!({
            "dir": "in", "kind": "text", "text": "one", "pkt": 700, "ts": 1,
            "payload_id": "sha256:one", "read": true, "locked": false, "unlocked": true
        });
        assert_eq!(
            first
                .upsert_in_record_fallible("10.0.0.7", &original)
                .unwrap(),
            InRecordOutcome::Inserted
        );
        drop(first);

        let restarted = AppState::new(dir.clone());
        let duplicate = serde_json::json!({
            "dir": "in", "kind": "text", "text": "one", "pkt": 700, "ts": 999,
            "payload_id": "sha256:one", "read": false, "locked": true, "unlocked": false
        });
        assert_eq!(
            restarted
                .upsert_in_record_fallible("10.0.0.7", &duplicate)
                .unwrap(),
            InRecordOutcome::Duplicate
        );
        assert_eq!(restarted.find_in_record("10.0.0.7", 700).unwrap()["read"], true);

        let conflict = serde_json::json!({
            "dir": "in", "kind": "text", "text": "two", "pkt": 700, "ts": 2,
            "payload_id": "sha256:two", "read": false, "locked": true, "unlocked": false
        });
        assert_eq!(
            restarted
                .upsert_in_record_fallible("10.0.0.7", &conflict)
                .unwrap(),
            InRecordOutcome::ReplacedConflict
        );
        let stored = restarted.find_in_record("10.0.0.7", 700).unwrap();
        assert_eq!(stored["text"], "two");
        assert_eq!(stored["ts"], 2);
        assert_eq!(stored["read"], false);
        assert_eq!(stored["locked"], true);
        assert_eq!(stored["unlocked"], false);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fallible_history_apis_report_invalid_directory_path() {
        let dir = std::env::temp_dir().join(format!(
            "oim-state-invalid-history-dir-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, b"not a directory").unwrap();
        let st = AppState::new(dir.clone());
        let record = serde_json::json!({
            "dir": "in", "pkt": 1, "payload_id": "sha256:x"
        });

        assert!(st.log_record_fallible("10.0.0.1", &record).is_err());
        assert!(st
            .upsert_in_record_fallible("10.0.0.1", &record)
            .is_err());
        std::fs::remove_file(dir).unwrap();
    }

    /// 会话摘要未读统计：in 且 read=false 计数；out 记录、已读记录不计；
    /// 无 read 字段的旧记录按未读处理（与前端 !m.read 语义一致）。
    #[test]
    fn session_summary_counts_unread_in_records() {
        let st = temp_state("sesssum");
        let rec = |pkt: u32, ts: u64, read: bool| {
            serde_json::json!({
                "dir": "in", "kind": "text", "pkt": pkt, "ts": ts, "read": read,
                "peer": {"key": "10.0.0.9", "nickname": "阿九", "host": "h9"}
            })
        };
        st.log_record("10.0.0.9", &rec(1, 100, false));
        st.log_record("10.0.0.9", &rec(2, 200, false));
        st.log_record("10.0.0.9", &rec(3, 300, true)); // 已读：不计
        st.log_record("10.0.0.9", &serde_json::json!({
            "dir": "out", "kind": "text", "pkt": 9, "ts": 400, "read": false,
            "peer": {"key": "10.0.0.9", "nickname": "阿九", "host": "h9"}
        }));
        // 旧版记录没有 read 字段：视为未读
        st.log_record("10.0.0.9", &serde_json::json!({
            "dir": "in", "kind": "text", "pkt": 4, "ts": 250,
            "peer": {"key": "10.0.0.9", "nickname": "阿九", "host": "h9"}
        }));
        let sess = st
            .list_sessions()
            .into_iter()
            .find(|s| s.key == "10.0.0.9")
            .expect("会话摘要存在");
        assert_eq!(sess.unread, 3, "入站未读 3 条（1、2、4），已读与出站不计");
        assert_eq!(sess.unread_ts, 250, "未读时间戳取最新未读记录的 ts");
        assert_eq!(sess.last_ts, 400, "last_ts 仍取全部记录最大 ts");

        // 标记已读后摘要回落
        st.mark_in_read("10.0.0.9", &[2, 4]);
        let sess = st
            .list_sessions()
            .into_iter()
            .find(|s| s.key == "10.0.0.9")
            .expect("会话摘要存在");
        assert_eq!(sess.unread, 1, "标记已读后仅剩包号 1 未读");
        assert_eq!(sess.unread_ts, 100);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn compact_histories_repairs_old_logs() {
        let st = temp_state("compact");
        std::fs::create_dir_all(&st.logs_dir).unwrap();
        let path = st.logs_dir.join("k_1.jsonl");
        // 旧版本遗留：同包号 3 份副本 + 一行交错坏行 + 一条正常出站记录
        let body = concat!(
            r#"{"dir":"in","pkt":7,"ts":1,"text":"a","read":true,"need_read":true}"#, "\n",
            r#"{"dir":"in","pkt":7,"ts":2,"text":"a+","read":false,"need_read":true}"#, "\n",
            r#"{"dir":"in","pkt":"#, "\n",
            r#"{"dir":"in","pkt":7,"ts":3,"text":"a++","read":false,"need_read":true}"#, "\n",
            r#"{"dir":"out","pkt":8,"ts":4,"text":"b"}"#, "\n",
        );
        std::fs::write(&path, body).unwrap();
        let (merged, dropped) = st.compact_histories();
        assert_eq!((merged, dropped), (2, 1));
        let hist = st.read_history("k:1", 50);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0]["ts"], 1, "保留首次时间戳");
        assert_eq!(hist[0]["text"], "a++", "内容取最新一份");
        assert_eq!(hist[0]["read"], true, "已读状态不被重投覆盖");
        assert_eq!(hist[1]["pkt"], 8);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn pending_out_queue_roundtrip_and_ack() {
        let st = temp_state("pending");
        // 离线人发消息：入队并持久化
        assert!(st.enqueue_pending(PendingOut {
            key: "10.0.0.9".into(),
            pkt: 777,
            text: "等你上线".into(),
            ts: 100,
            paths: vec!["/tmp/a.zip".into(), "/tmp/docs".into()],
        }));
        let list = st.pending_for("10.0.0.9");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pkt, 777);
        assert_eq!(list[0].paths, vec!["/tmp/a.zip", "/tmp/docs"], "附件路径随队列持久化");
        assert!(st.pending_for("10.0.0.8").is_empty());

        // 重启（新实例读同一数据目录）后队列仍在
        let st2 = AppState::new(st.data_dir.clone());
        st2.load_pending();
        let list2 = st2.pending_for("10.0.0.9");
        assert_eq!(list2.len(), 1, "重启不丢待投递");
        assert_eq!(list2[0].text, "等你上线");
        assert_eq!(list2[0].paths.len(), 2, "重启后附件路径仍在");

        // 收到 RECVMSG 确认后出队
        assert!(st2.ack_pending("10.0.0.9", 777));
        assert!(st2.pending_for("10.0.0.9").is_empty());
        let st3 = AppState::new(st.data_dir.clone());
        st3.load_pending();
        assert!(st3.pending_for("10.0.0.9").is_empty(), "确认后持久化移除");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn pending_out_legacy_format_without_paths_loads() {
        let st = temp_state("pending-legacy");
        // 旧版队列 JSON 没有 paths 字段：必须能兼容加载（serde default 补空）
        std::fs::create_dir_all(&st.data_dir).unwrap();
        std::fs::write(
            st.pending_path(),
            r#"{"10.0.0.9":[{"key":"10.0.0.9","pkt":1,"text":"旧版","ts":1}]}"#,
        )
        .unwrap();
        st.load_pending();
        let list = st.pending_for("10.0.0.9");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pkt, 1);
        assert!(list[0].paths.is_empty(), "旧记录无附件字段，补空");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn pending_out_can_drop_all_for_key() {
        let st = temp_state("pending-drop");
        for pkt in [1u32, 2, 3] {
            st.enqueue_pending(PendingOut {
                key: "10.0.0.9".into(),
                pkt,
                text: "x".into(),
                ts: 1,
                paths: vec![],
            });
        }
        let taken = st.take_pending("10.0.0.9");
        assert_eq!(taken.len(), 3);
        assert!(st.pending_for("10.0.0.9").is_empty(), "取出后即清空");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn list_sessions_returns_history_chats() {
        let st = temp_state("sessions");
        std::fs::create_dir_all(&st.logs_dir).unwrap();
        // 两个会话：一个在线时会话（peer 快照完整），一个只靠文件名兜底
        let a = "10.0.0.5";
        let b = "192.168.1.8";
        st.log_record(a, &serde_json::json!({
            "dir": "in", "pkt": 1, "ts": 100, "text": "hi",
            "peer": {"key": a, "nickname": "小王", "host": "pc-wang", "group": "财务"}
        }));
        st.log_record(b, &serde_json::json!({
            "dir": "out", "pkt": 2, "ts": 300, "text": "yo"
        }));
        let list = st.list_sessions();
        assert_eq!(list.len(), 2, "两个有历史的会话都列出");
        let by_key: std::collections::HashMap<_, _> =
            list.iter().map(|s| (s.key.as_str(), s)).collect();
        assert_eq!(by_key[a].nickname, "小王");
        assert_eq!(by_key[a].group, "财务");
        assert_eq!(by_key[a].last_ts, 100);
        assert_eq!(by_key[b].last_ts, 300, "无 peer 快照时按文件名校出 key，时间取最大");
        assert_eq!(by_key[b].key, b);
        // 按最近时间倒序
        assert_eq!(list[0].key, b);
        // 无历史时返回空
        let st2 = temp_state("sessions2");
        assert!(st2.list_sessions().is_empty());
        let _ = std::fs::remove_dir_all(&st.data_dir);
        let _ = std::fs::remove_dir_all(&st2.data_dir);
    }

    #[test]
    fn list_sessions_skips_legacy_ip_port_files() {
        let st = temp_state("sessions-legacy");
        std::fs::create_dir_all(&st.logs_dir).unwrap();
        // 迁移遗漏的旧命名文件（防御性跳过，不当作两个会话）
        std::fs::write(st.logs_dir.join("10.0.0.6_2425.jsonl"), "{}\n").unwrap();
        assert!(st.list_sessions().is_empty());
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn peer_upsert_merges_same_ip_across_ports() {
        let st = temp_state("peers");
        // 同一主机先从固定端口 :2425 上线；NAT 改写/套接字重绑后，
        // 后续广播可能来自任意临时端口 —— 必须识别为同一个用户
        assert!(st.upsert_peer(PeerInfo {
            key: "10.0.0.3:2425".into(),
            ip: "10.0.0.3".into(),
            port: 2425,
            nickname: "老王".into(),
            group: "财务".into(),
            host: "pc-wang".into(),
            user: "wang".into(),
            last_seen: 0,
            absence: false,
            absence_text: None,
            vs: None,
        }));
        let t0 = now_secs();
        assert!(!st.upsert_peer(PeerInfo {
            key: "10.0.0.3:50000".into(),
            ip: "10.0.0.3".into(),
            port: 50000,
            nickname: String::new(),
            group: String::new(),
            host: String::new(),
            user: String::new(),
            // last_seen 由 upsert 统一盖为当前时间，注入值会被覆盖
            last_seen: 5,
            absence: false,
            absence_text: None,
            vs: None,
        }));
        let peers = st.peers.lock().unwrap();
        assert_eq!(peers.len(), 1, "同 IP 不同源端口只允许一条记录");
        let p = &peers["10.0.0.3"];
        assert_eq!(p.nickname, "老王", "新报文字段为空时保留既有昵称");
        assert_eq!(p.group, "财务");
        assert_eq!(p.port, 50000, "端口随最新报文更新，否则回包发往失效地址");
        assert!(p.last_seen >= t0, "活跃时间随最新报文刷新");
        drop(peers);
        assert!(st.prune_stale_peers(u64::MAX).is_empty());
        // 手动回拨时间戳，模拟长时间未刷新
        st.peers
            .lock()
            .unwrap()
            .get_mut("10.0.0.3")
            .unwrap()
            .last_seen = 1;
        assert_eq!(st.prune_stale_peers(60).len(), 1);
        assert!(st.peers.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn legacy_history_files_migrate_to_ip_keys() {
        let st = temp_state("migrate");
        std::fs::create_dir_all(&st.logs_dir).unwrap();
        // 旧版会话文件以 `ip_端口.jsonl` 命名，记录内 peer.key 也是 ip:port
        let legacy = st.logs_dir.join("10.0.0.3_2425.jsonl");
        let body = concat!(
            r#"{"dir":"in","pkt":1,"ts":1,"text":"hi","peer":{"key":"10.0.0.3:2425","nickname":"老王"}}"#,
            "\n",
            r#"{"dir":"out","pkt":2,"ts":2,"text":"yo","peer":{"key":"10.0.0.3:2425","nickname":"老王"}}"#,
            "\n",
        );
        std::fs::write(&legacy, body).unwrap();
        // 无关文件不受影响；目标已存在时不吞掉旧文件
        std::fs::write(st.logs_dir.join("notes.jsonl"), "{}\n").unwrap();
        std::fs::write(st.logs_dir.join("10.0.0.9.jsonl"), "{}\n").unwrap();
        std::fs::write(st.logs_dir.join("10.0.0.9_2425.jsonl"), "{}\n").unwrap();

        let n = st.migrate_legacy_history_keys();
        assert_eq!(n, 1);
        assert!(!legacy.exists(), "旧命名文件应已迁移");
        let hist = st.read_history("10.0.0.3", 50);
        assert_eq!(hist.len(), 2, "迁移后按 IP 键可读到全部历史");
        assert_eq!(
            hist[0]["peer"]["key"], "10.0.0.3",
            "记录内快照的会话键同步归一化，搜索跳转才找得到会话"
        );
        assert_eq!(hist[0]["peer"]["nickname"], "老王", "其余字段原样保留");
        assert!(st.logs_dir.join("notes.jsonl").exists(), "非会话命名不受影响");
        assert!(st.logs_dir.join("10.0.0.9.jsonl").exists());
        assert!(
            st.logs_dir.join("10.0.0.9_2425.jsonl").exists(),
            "目标已存在时保留旧文件，不做有损合并"
        );
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn peer_key_cache_persists_across_reload() {
        use crate::crypto::KeyPair;
        use rsa::traits::PublicKeyParts;
        let st = temp_state("pcrypt");
        let kp = KeyPair::generate().unwrap();
        st.remember_peer_key("10.0.0.9", 0x40100004, &kp.public_key());
        assert!(st.peer_pubkey("10.0.0.9").is_some());
        assert_eq!(st.peer_capa("10.0.0.9") & 0x40100004, 0x40100004);
        // 未缓存的对端：公钥 None、能力位 0
        assert!(st.peer_pubkey("10.0.0.99").is_none());
        assert_eq!(st.peer_capa("10.0.0.99"), 0);

        // 新建同目录实例模拟重启
        let st2 = AppState::new(st.data_dir.clone());
        st2.load_peer_keys();
        assert!(st2.peer_pubkey("10.0.0.9").is_some(), "重启后密钥仍在");
        assert_eq!(
            st2.peer_pubkey("10.0.0.9").unwrap().n().to_bytes_be(),
            kp.public_key().n().to_bytes_be()
        );
        assert_eq!(st2.peer_capa("10.0.0.9"), 0x40100004, "能力位随密钥一起恢复");

        // 明文标记是内存态，不跨实例
        st.mark_peer_plain("10.0.0.8");
        assert!(st.peer_marked_plain("10.0.0.8"));
        assert!(!st.peer_marked_plain("10.0.0.9"));
        assert!(!st2.peer_marked_plain("10.0.0.8"), "明文标记不持久化");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    /// GETPUBKEY 探测预算（spec §5「已标记无能力」的最小实现）：
    /// 同一 IP 累计探测达阈值时，无缓存分支必须把对端标记为明文 ——
    /// 否则对每个不支持加密的客户端永远反复探测。
    #[test]
    fn probe_budget_crossing_marks_peer_plain() {
        let st = temp_state("probe");
        let ip = "10.9.9.9";
        // 每发一次 GETPUBKEY 计一次数；未达阈值时分支继续探测、不标记
        for sent in 1..PLAIN_PROBE_BUDGET {
            assert_eq!(st.record_probe(ip), sent, "计数按发送次数累加");
            assert!(
                !st.retire_probe_budget(ip),
                "预算未用尽前应返回「继续探测」"
            );
            assert!(!st.peer_marked_plain(ip), "穿越前不得标记明文");
        }
        // 第 3 次：恰在此刻穿越阈值 → 分支把对端标记为明文
        st.record_probe(ip);
        assert!(
            st.retire_probe_budget(ip),
            "第 {PLAIN_PROBE_BUDGET} 次探测后预算用尽，应触发明文标记"
        );
        assert!(
            st.peer_marked_plain(ip),
            "阈值穿越后 peer_marked_plain 必须翻转，后续消息静默走明文"
        );
        // 幂等：已标记后再评估不产生副作用，计数不受影响
        assert!(st.retire_probe_budget(ip));
        assert_eq!(st.probe_count(ip), PLAIN_PROBE_BUDGET);
        // 计数按 IP 相互独立
        st.record_probe("10.9.9.10");
        assert_eq!(st.probe_count("10.9.9.10"), 1);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn forget_peer_key_drops_cache_and_persists_withdrawal() {
        let st = temp_state("forget");
        let kp = KeyPair::generate().unwrap();
        st.remember_peer_key("10.0.0.9", 0x40100004, &kp.public_key());
        assert!(st.peer_pubkey("10.0.0.9").is_some());

        // 能力撤回：对端重新上线却不再声明 ENCRYPTOPT 时必须清掉其公钥缓存，
        // 否则我方会继续向已关闭加密的对端发送密文（对方永远解不开）
        st.forget_peer_key("10.0.0.9");
        assert!(
            st.peer_pubkey("10.0.0.9").is_none(),
            "撤回后内存缓存立即失效"
        );
        // 撤回必须持久化：重启（新实例读盘）后旧公钥不得复活
        let st2 = AppState::new(st.data_dir.clone());
        st2.load_peer_keys();
        assert!(st2.peer_pubkey("10.0.0.9").is_none(), "撤回写盘，重启不复活");

        // 未缓存的对端撤回是安全空操作；其它对端的缓存不受影响
        st.forget_peer_key("10.0.0.99");
        st.remember_peer_key("10.0.0.8", 1, &kp.public_key());
        st.forget_peer_key("10.0.0.9");
        assert!(st.peer_pubkey("10.0.0.8").is_some(), "只撤回目标对端");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn own_keypair_lazy_generates_and_persists() {
        let st = temp_state("ownkey");
        let fp1 = st.own_keypair().fingerprint();
        assert!(st.data_dir.join("ipmsg_key.json").exists(), "生成即落盘");
        // 幂等：同一实例反复取是同一把钥匙
        assert_eq!(st.own_keypair().fingerprint(), fp1);

        // 重启（新实例读同一数据目录）后仍是同一把钥匙
        let st2 = AppState::new(st.data_dir.clone());
        assert_eq!(st2.own_keypair().fingerprint(), fp1);
        assert_eq!(st.fingerprint(), fp1);
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn own_keypair_regenerates_on_corrupt_file() {
        let st = temp_state("ownkey-corrupt");
        std::fs::create_dir_all(&st.data_dir).unwrap();
        std::fs::write(st.data_dir.join("ipmsg_key.json"), "{{{不是JSON").unwrap();
        // 损坏文件不能让进程崩：重新生成一把并覆盖写回
        let kp = st.own_keypair();
        assert_eq!(kp.modulus_be().len(), crate::crypto::RSA_BITS / 8);
        let fp = kp.fingerprint();
        let st2 = AppState::new(st.data_dir.clone());
        assert_eq!(st2.own_keypair().fingerprint(), fp, "重生的钥匙已落盘");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn encrypt_flag_defaults_true_and_persists() {
        let st = temp_state("encflag");
        std::fs::create_dir_all(&st.data_dir).unwrap();
        st.load_config();
        assert!(st.config().encrypt, "默认开启加密");

        let mut cfg = st.config();
        cfg.encrypt = false;
        st.set_config(cfg);
        st.persist_config().unwrap();

        let st2 = AppState::new(st.data_dir.clone());
        st2.load_config();
        assert!(!st2.config().encrypt, "关闭状态重启后保留");

        // 旧版本配置文件没有 encrypt 字段时按默认值补齐（serde default）
        let path = st.data_dir.join("config.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("encrypt");
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let st3 = AppState::new(st.data_dir.clone());
        st3.load_config();
        assert!(st3.config().encrypt, "缺失字段回落 serde 默认值 true");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    /// 私钥文件存的是未加密 PKCS#8（含全部私钥成分），落盘必须收紧为
    /// 属主可读写（0o600），不能带着 umask 默认的组/其他人可读权限躺在磁盘上。
    #[cfg(unix)]
    #[test]
    fn own_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |dir: &std::path::Path| -> u32 {
            std::fs::metadata(dir.join("ipmsg_key.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        // 1) 首次生成路径
        let st = temp_state("keymode");
        std::fs::create_dir_all(&st.data_dir).unwrap();
        let _ = st.own_keypair();
        assert_eq!(mode_of(&st.data_dir), 0o600, "初次生成的私钥文件应为 0o600");

        // 2) 损坏后重写路径（重新生成并覆盖写回，同样要收紧）
        std::fs::write(st.data_dir.join("ipmsg_key.json"), "{{{不是JSON").unwrap();
        let st2 = AppState::new(st.data_dir.clone());
        let _ = st2.own_keypair();
        assert_eq!(
            mode_of(&st.data_dir),
            0o600,
            "损坏重写后的私钥文件也应为 0o600"
        );
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }
}
