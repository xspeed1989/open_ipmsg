//! 应用状态：配置、在线用户表、对外提供的文件槽、聊天记录(JSONL) 持久化与事件回调。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
}

fn default_theme() -> String {
    "system".into()
}

fn default_encoding() -> String {
    // 默认 UTF-8，出站报文自动携带官方 IPMSG_UTF8OPT 编码协商标志；
    // 与 GBK 方言老客户端互通时可在设置中切换
    "utf8".into()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            nickname: String::new(),
            group: String::new(),
            download_dir: String::new(),
            encoding: default_encoding(),
            theme: default_theme(),
        }
    }
}

/// 局域网内的对端用户
#[derive(Serialize, Clone, Debug)]
pub struct PeerInfo {
    /// 稳定标识："ip:port"
    pub key: String,
    pub ip: String,
    pub port: u16,
    pub nickname: String,
    pub group: String,
    pub host: String,
    pub user: String,
    pub last_seen: u64,
}

/// 我们发出、等待对端来取的文件
pub struct OfferedFile {
    pub path: PathBuf,
    pub size: u64,
    /// 是否为目录（走 GETDIRFILES 流式传输）
    pub is_dir: bool,
    /// 登记时刻，用于过期清理
    pub ts: u64,
}

/// 文件槽保留时长：对端可能延迟很久才来取，但也不能无限累积
pub const OFFER_TTL_SECS: u64 = 24 * 3600;

type EventFn = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

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
    /// 聊天记录文件的读-改-写互斥（防止并发追加与重写互相覆盖）
    hist_lock: Mutex<()>,
    on_event: Mutex<Option<EventFn>>,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
}

const SEEN_CAP: usize = 8192;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
            on_event: Mutex::new(None),
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
    pub fn diag(&self, line: &str) {
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
    pub fn upsert_peer(&self, mut info: PeerInfo) -> bool {
        use crate::protocol::strip_control;
        info.nickname = strip_control(&info.nickname);
        info.group = strip_control(&info.group);
        info.host = strip_control(&info.host);
        info.user = strip_control(&info.user);
        info.last_seen = now_secs();
        let mut peers = self.peers.lock().unwrap();
        match peers.get_mut(&info.key) {
            Some(existing) => {
                existing.last_seen = info.last_seen;
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
        {
            let set = self.seen_set.lock().unwrap();
            if set.contains(&item) {
                return false;
            }
        }
        let mut set = self.seen_set.lock().unwrap();
        let mut queue = self.seen_queue.lock().unwrap();
        if set.insert(item) {
            queue.push_back(item);
            while queue.len() > SEEN_CAP {
                if let Some(old) = queue.pop_front() {
                    set.remove(&old);
                }
            }
        }
        true
    }

    /* ---------- 聊天记录 ---------- */

    fn log_path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
            .collect();
        self.logs_dir.join(format!("{}.jsonl", safe))
    }

    /// 追加一条消息记录
    pub fn log_record(&self, key: &str, rec: &serde_json::Value) {
        let _g = self.hist_lock.lock().unwrap();
        let path = self.log_path(key);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            // 整行一次写入：writeln! 会把 JSON 拆成多次 write 系统调用，
            // 追加模式下与其它写入者交错就会写出无法解析的坏行
            let line = format!("{rec}\n");
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// 落库一条入站记录：同包号的旧记录存在则原地更新，否则追加。
    ///
    /// 对端的"延迟发送/离线重发"会用同一包号反复投递同一条消息（飞秋等实现
    /// 每次我方上线都会重发），逐条追加会让历史无限膨胀，更要命的是每份新副本
    /// 都是未读状态，前端一标记已读就再回一次 READMSG，对端于是反复弹
    /// "消息已被查看"。返回 true 表示是本会话第一次见到该包号。
    pub fn upsert_in_record(&self, key: &str, rec: &serde_json::Value) -> bool {
        let pkt = rec.get("pkt").and_then(|v| v.as_u64());
        let Some(pkt) = pkt else {
            self.log_record(key, rec);
            return true;
        };
        {
            let _g = self.hist_lock.lock().unwrap();
            let path = self.log_path(key);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let mut found = false;
                let mut lines: Vec<String> = Vec::new();
                for line in content.lines() {
                    let old: serde_json::Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(_) => {
                            lines.push(line.to_string());
                            continue;
                        }
                    };
                    let hit = old.get("dir").and_then(|v| v.as_str()) == Some("in")
                        && old.get("pkt").and_then(|v| v.as_u64()) == Some(pkt);
                    if hit && !found {
                        found = true;
                        let mut merged = rec.clone();
                        // 保留首次收到的时间，重发不该把消息顶到列表末尾
                        if let Some(ts) = old.get("ts") {
                            merged["ts"] = ts.clone();
                        }
                        lines.push(merged.to_string());
                    } else if hit {
                        // 历史上已经堆积的重复副本：顺手清理掉
                        continue;
                    } else {
                        lines.push(line.to_string());
                    }
                }
                if found {
                    let _ = std::fs::write(&path, lines.join("\n") + "\n");
                    return false;
                }
            }
        }
        self.log_record(key, rec);
        true
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
            if want.contains(&pkt) && need && !read && !out.contains(&pkt) {
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
        st.set_config(cfg);
        st.persist_config().unwrap();

        let st2 = AppState::new(st.data_dir.clone());
        st2.load_config();
        assert_eq!(st2.config().nickname, "测试昵称");
        assert_eq!(st2.config().group, "G1");
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
    fn peer_upsert_prune() {
        let st = temp_state("peers");
        let info = PeerInfo {
            key: "10.0.0.3:2425".into(),
            ip: "10.0.0.3".into(),
            port: 2425,
            nickname: "老王".into(),
            group: "财务".into(),
            host: "pc-wang".into(),
            user: "wang".into(),
            last_seen: 0,
        };
        assert!(st.upsert_peer(info));
        assert!(!st.upsert_peer(PeerInfo {
            key: "10.0.0.3:2425".into(),
            ip: "10.0.0.3".into(),
            port: 2425,
            nickname: String::new(),
            group: String::new(),
            host: String::new(),
            user: String::new(),
            last_seen: 0,
        }));
        assert_eq!(st.peers.lock().unwrap()["10.0.0.3:2425"].nickname, "老王");
        assert_eq!(st.peers.lock().unwrap()["10.0.0.3:2425"].group, "财务");
        assert!(st.prune_stale_peers(u64::MAX).is_empty());
        // 手动回拨时间戳，模拟长时间未刷新
        st.peers
            .lock()
            .unwrap()
            .get_mut("10.0.0.3:2425")
            .unwrap()
            .last_seen = 1;
        assert_eq!(st.prune_stale_peers(60).len(), 1);
        assert!(st.peers.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }
}
