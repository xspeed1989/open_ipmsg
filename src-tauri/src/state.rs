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
}

fn default_encoding() -> String {
    "utf8".into()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            nickname: String::new(),
            group: String::new(),
            download_dir: String::new(),
            encoding: default_encoding(),
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
}

type EventFn = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub struct AppState {
    pub config: Mutex<Config>,
    pub peers: Mutex<HashMap<String, PeerInfo>>,
    pub offered: Mutex<HashMap<(u32, u32), OfferedFile>>,
    seen_queue: Mutex<VecDeque<(IpAddr, u32)>>,
    seen_set: Mutex<HashSet<(IpAddr, u32)>>,
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
            seen_queue: Mutex::new(VecDeque::new()),
            seen_set: Mutex::new(HashSet::new()),
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
        if set.insert(item.clone()) {
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
        let path = self.log_path(key);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{}", rec);
        }
    }

    /// 读取某会话最近 limit 条记录（按时间升序返回）
    pub fn read_history(&self, key: &str, limit: usize) -> Vec<serde_json::Value> {
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

    /// 该会话是否已有同包号的入站记录（对端延迟重发去重）
    pub fn has_in_record(&self, key: &str, pkt: u32) -> bool {
        let path = self.log_path(key);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return false;
        };
        content.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l).is_ok_and(|rec| {
                rec.get("dir").and_then(|v| v.as_str()) == Some("in")
                    && rec.get("pkt").and_then(|v| v.as_u64()) == Some(pkt as u64)
            })
        })
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
