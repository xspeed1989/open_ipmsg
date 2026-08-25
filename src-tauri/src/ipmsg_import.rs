//! 从官方 IP Messenger（v4.50+）的 SQLite 日志库导入聊天记录。
//!
//! 官方客户端把历史存成 `ipmsg.db`（表结构见 shirouzu/ipmsg 的 logdb.cpp）：
//! - `msg_tbl.msg_id` 高 38 位是 Unix 秒（`msg_id >> 26`），低位是亚秒计数；
//!   `flags & DB_FLAG_FROM(0x1)` 置位表示「收到的消息」，否则是自己发出的
//! - `msghost_tbl.idx == 0` 恒为会话对方（收=发件人，发=收件人）；idx>1 是
//!   组播的 Cc 列表，本项目没有群聊概念，只按对方归属会话
//! - `host_tbl.addr` 是对端 IP，正好映射到本项目的「每 IP 一个会话」
//! - 附件只有文件名（`file_tbl`/`clip_tbl`），内容不在库里

use crate::state::AppState;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// 单个日志库的导入结果
#[derive(Default, Debug)]
pub struct ImportReport {
    /// 新写入的记录数
    pub imported: usize,
    /// 跳过数（重复导入 / 备忘录等）
    pub skipped: usize,
    /// 本次新出现的会话数
    pub sessions_new: usize,
    /// 按 (昵称, 主机) 归并掉的旧导入会话文件数
    pub merged_sessions: usize,
}

/* ================= 实现 ================= */

/// 官方 uid 形如 `name-<16位摘要>`，剥掉摘要只留账号名
fn strip_digest(uid: &str) -> String {
    match uid.rfind("-<") {
        Some(i) if uid.ends_with('>') => uid[..i].to_string(),
        _ => uid.to_string(),
    }
}

/// 会话归属：优先用对端 IP；拿不到就按主机名兜底建离线会话（`@主机名`）
fn session_key(addr: &str, host: &str) -> String {
    if addr.parse::<std::net::IpAddr>().is_ok() {
        addr.to_string()
    } else if !host.is_empty() {
        format!("@{host}")
    } else {
        "@unknown".into()
    }
}

/// 会话身份合并键：(昵称, 主机名)。两者都非空才构成身份，
/// 忽略大小写与首尾空白 —— 避免把同名不同机的人并为一人。
fn ident_key(nick: &str, host: &str) -> Option<(String, String)> {
    let n = nick.trim().to_lowercase();
    let h = host.trim().to_lowercase();
    if n.is_empty() || h.is_empty() {
        None
    } else {
        Some((n, h))
    }
}

/// 解析一条导入记录的归属会话：同一 (昵称, 主机) 的所有 IP 归并到一个会话。
///
/// 官方日志库按对端 IP 记地址，而 IP 会变（DHCP / 多网卡 / 换网络），
/// 同一台机器、同一个人的消息会被拆到多个 IP 会话里，中栏列表就出现
/// 多个同名的联系人。这里用「昵称 + 主机名」做身份识别：已登记过该身份的
/// 会话直接复用其 key；没见过的身份才按 IP 开新会话并登记。
fn resolve_key(
    ident_map: &mut HashMap<(String, String), String>,
    nick: &str,
    host: &str,
    addr: &str,
) -> String {
    if let Some(id) = ident_key(nick, host) {
        if let Some(k) = ident_map.get(&id) {
            return k.clone();
        }
        let k = session_key(addr, host);
        ident_map.insert(id, k.clone());
        return k;
    }
    session_key(addr, host)
}

/// 读 `file_tbl` / `clip_tbl` 的 msg_id → 文件名列表。
/// 老版本库可能没有这些表，任何读取失败都当作「无附件」处理。
fn fnames_by_msg(conn: &Connection, table: &str) -> HashMap<i64, Vec<String>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    let sql = format!("select msg_id, fname from {table}");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        out.entry(row.0).or_default().push(row.1);
    }
    out
}

/// 导入一个官方 ipmsg 日志库到本应用的聊天记录。
pub fn import_ipmsg_db(st: &AppState, path: &Path) -> Result<ImportReport, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("无法打开 {}：{e}", path.display()))?;

    // 结构校验：官方日志库必有 msg_tbl，否则给出能引导用户的错误
    let has_msg_tbl: i64 = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='msg_tbl'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("读取失败：{e}"))?;
    if has_msg_tbl == 0 {
        return Err(format!(
            "{} 不是官方 IP Messenger 的日志库（找不到 msg_tbl 表）",
            path.display()
        ));
    }

    // 会话对方（msghost.idx == 0）：收=发件人，发=第一个收件人
    let mut stmt = conn
        .prepare(
            "select m.msg_id, m.flags, m.body, h.uid, h.nick, h.host, h.addr, h.gname
             from msg_tbl m
             join msghost_tbl mg on mg.msg_id = m.msg_id and mg.idx = 0
             join host_tbl h on h.host_id = mg.host_id
             order by m.msg_id asc",
        )
        .map_err(|e| format!("读取消息失败：{e}"))?;
    let peers: Vec<(i64, i64, String, String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                r.get::<_, Option<String>>(7)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| format!("读取消息失败：{e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("读取消息失败：{e}"))?;
    drop(stmt);

    // 自己发出的群发消息的其余收件人（idx > 0），逐会话落一份副本
    let mut extra_rcpt: HashMap<i64, Vec<(String, String, String, String, String)>> =
        HashMap::new(); // msg_id → [(uid,nick,host,addr,gname)]
    if let Ok(mut stmt) = conn.prepare(
        "select mg.msg_id, h.uid, h.nick, h.host, h.addr, h.gname
         from msghost_tbl mg join host_tbl h on h.host_id = mg.host_id
         where mg.idx > 0",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            ))
        }) {
            for row in rows.flatten() {
                extra_rcpt.entry(row.0).or_default().push((
                    row.1.clone(),
                    row.2.clone(),
                    row.3.clone(),
                    row.4.clone(),
                    row.5.clone(),
                ));
            }
        }
    }

    let files_by_msg = fnames_by_msg(&conn, "file_tbl");
    let clips_by_msg = fnames_by_msg(&conn, "clip_tbl");

    let db_tag = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ipmsg".into());

    // 导入前已存在的会话，用于统计「新增会话数」
    let known_before: HashSet<String> = st.list_sessions().into_iter().map(|s| s.key).collect();

    // 会话身份登记表：(昵称, 主机) → 会话 key。
    // 种子：本应用既有会话（离线历史）与在线用户 —— 导入的同名同机记录
    // 直接并入现有会话，而不是按 IP 另开新会话。
    // 同一身份出现多个会话时按优先级取：在线用户 > 有实时对话的会话 >
    // 纯导入残留（后者稍后会被 consolidate 归并掉）。
    let mut seeds: Vec<((String, String), String, bool)> = Vec::new();
    for s in st.list_sessions() {
        if let Some(id) = ident_key(&s.nickname, &s.host) {
            let live = st
                .read_history(&s.key, usize::MAX)
                .iter()
                .any(|r| r.get("imp").is_none());
            seeds.push((id, s.key, live));
        }
    }
    seeds.sort_by_key(|(_, _, live)| !*live); // live=true 优先
    let mut ident_map: HashMap<(String, String), String> = HashMap::new();
    // 在线用户优先：正在对话的人，历史应落在他的当前会话里
    for p in st.peers.lock().unwrap().values() {
        if let Some(id) = ident_key(&p.nickname, &p.host) {
            ident_map.entry(id).or_insert(p.key.clone());
        }
    }
    for (id, key, _) in seeds {
        ident_map.entry(id).or_insert(key);
    }

    // 每个会话已有的导入来源 id（重复导入去重）
    let mut seen_ids: HashMap<String, HashSet<u64>> = HashMap::new();

    let mut rep = ImportReport::default();
    let mut touched: HashSet<String> = HashSet::new();

    for (msg_id, flags, body, uid, nick, host, addr, gname) in peers {
        // 官方的「备忘录」（记事本）条目不是对话，跳过
        if uid.starts_with("ipmsg-memo-") {
            rep.skipped += 1;
            continue;
        }
        if msg_id <= 0 {
            rep.skipped += 1;
            continue;
        }
        let recv = flags & 1 != 0; // DB_FLAG_FROM

        // 目标会话列表：收到→发件人；发出→每个收件人各一份
        let mut targets: Vec<(String, String, String, String, String)> =
            vec![(uid.clone(), nick.clone(), host.clone(), addr.clone(), gname.clone())];
        if !recv {
            targets.extend(extra_rcpt.get(&msg_id).cloned().unwrap_or_default());
        }

        let ts = (msg_id as u64) >> 26;
        for (tuid, tnick, thost, taddr, tgname) in &targets {
            // 同一 (昵称, 主机) 的多个 IP 归并到同一个会话，避免列表出现同名联系人
            let key = resolve_key(&mut ident_map, tnick, thost, taddr);
            let seen = seen_ids.entry(key.clone()).or_insert_with(|| {
                st.read_history(&key, usize::MAX)
                    .iter()
                    .filter_map(|r| r.get("imp").and_then(|i| i.get("id")).and_then(|v| v.as_u64()))
                    .collect()
            });
            if !seen.insert(msg_id as u64) {
                rep.skipped += 1;
                continue;
            }

            let display_name = if tnick.is_empty() {
                strip_digest(tuid)
            } else {
                tnick.clone()
            };
            let mut rec = json!({
                "dir": if recv { "in" } else { "out" },
                "kind": "text",
                "ts": ts,
                "text": body,
                // 历史消息不再参与已读回执流程，一律视为已读
                "read": true,
                "imported": true,
                "imp": { "db": db_tag, "id": msg_id as u64 },
                "peer": {
                    "key": key,
                    "nickname": display_name,
                    "host": thost,
                    "group": tgname,
                    "user": strip_digest(tuid),
                },
            });
            let mut names = files_by_msg.get(&msg_id).cloned().unwrap_or_default();
            names.extend(clips_by_msg.get(&msg_id).cloned().unwrap_or_default());
            if !names.is_empty() {
                rec["kind"] = "file".into();
                rec["files"] = json!(names
                    .iter()
                    .map(|n| json!({"name": n, "size": 0, "state": "imported"}))
                    .collect::<Vec<_>>());
            }

            st.log_record(&key, &rec);
            touched.insert(key);
            rep.imported += 1;
        }
    }

    rep.sessions_new = touched.iter().filter(|k| !known_before.contains(*k)).count();
    // 旧版本导入按 IP 拆出的「纯导入」会话文件：按 (昵称, 主机) 归并进目标
    // 会话并删除，清理掉中栏里同一联系人的多个同名会话
    rep.merged_sessions = consolidate_imported_sessions(st, &ident_map);
    Ok(rep)
}

/// 把「纯导入」的旧会话文件按 (昵称, 主机) 归并到目标会话，返回归并的文件数。
///
/// 旧版本导入按 IP 归档：同一个人换过 IP（DHCP / 多网卡）会在中栏留下多个
/// 同名会话。这里把「全部记录都带 imp 标记」（纯导入产物，没有本应用实时
/// 对话）且身份可识别的文件整体并入目标会话：按 imp.id 去重、记录里的
/// peer.key 快照同步改写（否则搜索跳转找不到会话），然后删除源文件。
/// 只要混有实时记录（无 imp 标记）就视为真实会话，绝不动它。
fn consolidate_imported_sessions(
    st: &AppState,
    ident_map: &HashMap<(String, String), String>,
) -> usize {
    // 第一遍：快照出可归并的源文件与目标 key（list_sessions 内部要拿
    // hist_lock，这里不能先持锁，否则自锁死）
    let mut moves: Vec<(String, String)> = Vec::new();
    for s in st.list_sessions() {
        let Some(id) = ident_key(&s.nickname, &s.host) else {
            continue;
        };
        let Some(target) = ident_map.get(&id) else {
            continue;
        };
        if *target == s.key {
            continue;
        }
        let path = st.log_path(&s.key);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let all_imported = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()
                    .and_then(|r| r.get("imp").cloned())
                    .is_some()
            });
        if !all_imported {
            continue;
        }
        moves.push((s.key.clone(), target.clone()));
    }
    if moves.is_empty() {
        return 0;
    }
    // 目标自身也可能在移动列表里（A→B 且 B→C）：先追到最终目标再写
    let target_of: HashMap<String, String> = moves.iter().cloned().collect();
    // 第二遍：持锁执行归并——读源、写目标、删源一气呵成，避免实时消息在
    // 间隙落进即将被删除的源文件而被一起丢掉。此段只做直接文件 I/O，
    // 不再调用任何会取 hist_lock 的 AppState 方法。
    let _g = st.hist_lock.lock().unwrap();
    let mut merged = 0usize;
    // 每个目标会话已见到的 imp.id（跨多个源文件累计，避免重复搬入）
    let mut seen_targets: HashMap<String, HashSet<u64>> = HashMap::new();
    for (src, tgt) in &moves {
        let mut target = tgt.clone();
        while let Some(next) = target_of.get(&target) {
            if *next == target {
                break;
            }
            target = next.clone();
        }
        if target == *src {
            continue;
        }
        let spath = st.log_path(src);
        let Ok(content) = std::fs::read_to_string(&spath) else {
            continue;
        };
        // 源文件逐条解析；混入任何非导入记录（无 imp / 坏行）就放弃这次归并
        // —— 那说明它已经是真实会话，不能删
        let mut recs: Vec<serde_json::Value> = Vec::new();
        let mut imp_only = true;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(r) => {
                    if r.get("imp").is_none() {
                        imp_only = false;
                    }
                    recs.push(r);
                }
                Err(_) => imp_only = false,
            }
        }
        if !imp_only {
            continue;
        }
        let seen = seen_targets
            .entry(target.clone())
            .or_insert_with(|| {
                let tpath = st.log_path(&target);
                std::fs::read_to_string(&tpath)
                    .ok()
                    .map(|c| {
                        c.lines()
                            .filter_map(|l| {
                                serde_json::from_str::<serde_json::Value>(l)
                                    .ok()
                                    .and_then(|r| {
                                        r.get("imp")
                                            .and_then(|i| i.get("id"))
                                            .and_then(|v| v.as_u64())
                                    })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
        // 逐条去重并改写 peer.key 快照，攒成一次追加
        let mut append = String::new();
        for mut rec in recs {
            let Some(id) = rec
                .get("imp")
                .and_then(|i| i.get("id"))
                .and_then(|v| v.as_u64())
            else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            // 记录内的 peer.key 快照改写为目标会话，搜索跳转才不会落空
            if let Some(p) = rec.get_mut("peer") {
                if let Some(kv) = p.get_mut("key") {
                    *kv = serde_json::Value::String(target.clone());
                }
            }
            append.push_str(&rec.to_string());
            append.push('\n');
        }
        if append.is_empty() {
            continue;
        }
        let tpath = st.log_path(&target);
        if let Some(dir) = tpath.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        let ok = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&tpath)
            .ok()
            .map(|mut f| f.write_all(append.as_bytes()).is_ok())
            .unwrap_or(false);
        if ok && std::fs::remove_file(&spath).is_ok() {
            merged += 1;
        }
    }
    merged
}

/* ---------------- 单元测试 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn temp_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!(
            "oim-import-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        AppState::new(dir)
    }

    /// 按官方 ipmsg.db 的真实表结构生成 fixture 库
    fn make_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE msg_tbl(
                msg_id integer primary key, importid integer, cmd integer,
                flags integer, body text, lines integer,
                alter_date integer, comment text, packet_no integer);
            CREATE TABLE host_tbl(
                host_id integer primary key, uid text, nick text,
                host text, addr text, gname text);
            CREATE TABLE msghost_tbl(
                msg_id integer, host_id integer, flags integer, idx integer);
            CREATE TABLE file_tbl(msg_id integer, fname text);
            CREATE TABLE clip_tbl(msg_id integer, fname text, cx integer, cy integer);
            "#,
        )
        .unwrap();

        // 官方样本里的真实形态：uid 带 "-<摘要>" 尾巴；备忘录条目 uid 以 ipmsg-memo- 开头
        conn.execute(
            "insert into host_tbl values(1,'alice-<64b938a16883bf8f>','Alice','PC-ALICE','192.168.1.11','研发')",
            [],
        ).unwrap();
        conn.execute(
            "insert into host_tbl values(2,'bob-<88f54dca963f106a>','Bob','PC-BOB','10.0.0.5','')",
            [],
        ).unwrap();
        conn.execute(
            "insert into host_tbl values(3,'ipmsg-memo-<0000000000000000>',' [ Memo ] ','localhost','','')",
            [],
        ).unwrap();
        conn.execute(
            "insert into host_tbl values(4,'carol','Carol','CAROL-PC','','')",
            [],
        ).unwrap();

        let t: i64 = 1_700_000_000;
        let mid = |sec: i64, n: i64| (sec << 26) | n;
        let ins_msg = |conn: &Connection, id: i64, flags: i64, body: &str| {
            conn.execute(
                "insert into msg_tbl(msg_id,cmd,flags,body,lines) values(?1,0,?2,?3,1)",
                params![id, flags, body],
            )
            .unwrap();
        };
        let ins_ghost = |conn: &Connection, id: i64, hid: i64, idx: i64| {
            conn.execute(
                "insert into msghost_tbl values(?1,?2,0,?3)",
                params![id, hid, idx],
            )
            .unwrap();
        };

        // 收到：来自 Alice
        ins_msg(&conn, mid(t, 1), 1, "你好");
        ins_ghost(&conn, mid(t, 1), 1, 0);
        // 发出：给 Alice
        ins_msg(&conn, mid(t + 60, 2), 0, "收到");
        ins_ghost(&conn, mid(t + 60, 2), 1, 0);
        // 收到的组播：发件人 Alice，Cc Bob —— 只应归入 Alice 会话
        ins_msg(&conn, mid(t + 120, 4), 1 | 0x10_0000, "全员注意");
        ins_ghost(&conn, mid(t + 120, 4), 1, 0);
        ins_ghost(&conn, mid(t + 120, 4), 2, 1);
        // 群发：同时发给 Alice 和 Bob —— 两个会话都应有副本
        ins_msg(&conn, mid(t + 180, 8), 0, "周会改到三点");
        ins_ghost(&conn, mid(t + 180, 8), 1, 0);
        ins_ghost(&conn, mid(t + 180, 8), 2, 1);
        // 备忘录：跳过
        ins_msg(&conn, mid(t + 240, 16), 0x20, "待办事项");
        ins_ghost(&conn, mid(t + 240, 16), 3, 0);
        // 无 IP 的对端：兜底按主机名建会话
        ins_msg(&conn, mid(t + 300, 32), 1, "我在内网深处");
        ins_ghost(&conn, mid(t + 300, 32), 4, 0);
        // 带附件 + 剪贴板截图的消息
        ins_msg(&conn, mid(t + 360, 64), 1 | 0x2 | 0x4, "");
        ins_ghost(&conn, mid(t + 360, 64), 1, 0);
        conn.execute(
            "insert into file_tbl values(?1,'报告.zip')",
            params![mid(t + 360, 64)],
        )
        .unwrap();
        conn.execute(
            "insert into clip_tbl values(?1,'ipmsgclip_r_1782986553_0.png',100,50)",
            params![mid(t + 360, 64)],
        )
        .unwrap();
    }

    #[test]
    fn parses_direction_time_text_and_peer_snapshot() {
        let st = temp_state("parse");
        let db = st.data_dir.join("sample.db");
        make_db(&db);

        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(rep.imported, 7, "6 条正常消息 + 1 条带附件");

        let hist = st.read_history("192.168.1.11", 100);
        // 收到 Alice 的第一条
        let first = hist
            .iter()
            .find(|r| r["text"] == "你好")
            .expect("应包含收到的消息");
        assert_eq!(first["dir"], "in");
        assert_eq!(first["ts"], 1_700_000_000u64, "时间取自 msg_id 高位");
        assert_eq!(first["kind"], "text");
        assert_eq!(first["peer"]["key"], "192.168.1.11");
        assert_eq!(first["peer"]["nickname"], "Alice");
        assert_eq!(first["peer"]["host"], "PC-ALICE");
        assert_eq!(first["peer"]["group"], "研发");
        assert_eq!(
            first["peer"]["user"], "alice",
            "uid 的 -<摘要> 尾巴应剥掉"
        );
        assert_eq!(first["imp"]["db"], "sample");
        assert_eq!(
            first["imp"]["id"],
            ((1_700_000_000i64 << 26) | 1) as u64
        );

        // 自己发出的那条
        let out = hist.iter().find(|r| r["text"] == "收到").unwrap();
        assert_eq!(out["dir"], "out");

        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn multi_recipient_sent_lands_in_every_session() {
        let st = temp_state("fanout");
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        import_ipmsg_db(&st, &db).unwrap();

        for key in ["192.168.1.11", "10.0.0.5"] {
            let hit = st
                .read_history(key, 100)
                .iter()
                .any(|r| r["text"] == "周会改到三点" && r["dir"] == "out");
            assert!(hit, "群发消息应出现在 {key} 的会话里");
        }
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn received_multicast_belongs_to_sender_only() {
        let st = temp_state("multicast");
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        import_ipmsg_db(&st, &db).unwrap();

        let in_alice = st
            .read_history("192.168.1.11", 100)
            .iter()
            .any(|r| r["text"] == "全员注意");
        let in_bob = st
            .read_history("10.0.0.5", 100)
            .iter()
            .any(|r| r["text"] == "全员注意");
        assert!(in_alice, "组播归发件人会话");
        assert!(!in_bob, "Cc 列表不另立会话（无群聊概念）");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn memo_entries_are_skipped() {
        let st = temp_state("memo");
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(rep.skipped, 1, "备忘录计入 skipped");
        let leaked = st
            .read_history("@localhost", 100)
            .iter()
            .chain(st.read_history("localhost", 100).iter())
            .any(|r| r["text"] == "待办事项");
        assert!(!leaked, "备忘录不应落入任何会话");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn peer_without_ip_falls_back_to_hostname_session() {
        let st = temp_state("noip");
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        import_ipmsg_db(&st, &db).unwrap();
        let hist = st.read_history("@CAROL-PC", 100);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0]["text"], "我在内网深处");
        assert_eq!(hist[0]["dir"], "in");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn attachments_become_imported_file_cards() {
        let st = temp_state("attach");
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        import_ipmsg_db(&st, &db).unwrap();

        let rec = st
            .read_history("192.168.1.11", 100)
            .into_iter()
            .find(|r| r["kind"] == "file")
            .expect("附件消息应为 file 类型");
        let files = rec["files"].as_array().unwrap();
        assert_eq!(files.len(), 2, "普通附件与剪贴板截图都记录");
        let names: Vec<&str> = files
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"报告.zip"));
        assert!(names.contains(&"ipmsgclip_r_1782986553_0.png"));
        for f in files {
            assert_eq!(f["state"], "imported", "标记为历史导入，前端不给下载按钮");
        }
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn reimport_is_a_noop() {
        let st = temp_state("dedup");
        let db = st.data_dir.join("sample.db");
        make_db(&db);

        let first = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(first.imported, 7);

        let second = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(second.imported, 0, "重复导入不追加");
        // 每条消息按目标会话各跳过一次：6 条单会话 + 群发的 2 个会话 = 8
        assert_eq!(second.skipped, 8, "全部按已存在跳过");

        let total = st.read_history("192.168.1.11", 100).len();
        assert_eq!(total, 5, "会话记录数不变（收3 + 群发1 + 附件1）");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn same_peer_different_ips_merge_by_name_and_host() {
        let st = temp_state("mergeip");
        let db = st.data_dir.join("multi.db");
        // Alice 先后用两个 IP（DHCP 换网段），昵称与主机名不变
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE msg_tbl(msg_id integer primary key, cmd integer, flags integer, body text, lines integer);
            CREATE TABLE host_tbl(host_id integer primary key, uid text, nick text, host text, addr text, gname text);
            CREATE TABLE msghost_tbl(msg_id integer, host_id integer, flags integer, idx integer);
            insert into host_tbl values(1,'alice','Alice','PC-ALICE','192.168.1.11','研发');
            insert into host_tbl values(2,'alice','Alice','PC-ALICE','192.168.1.99','研发');
            insert into msg_tbl(msg_id,flags,body) values(((1700000000)<<26)|1, 1, '第一条（老 IP）');
            insert into msghost_tbl values(((1700000000)<<26)|1, 1, 0, 0);
            insert into msg_tbl(msg_id,flags,body) values(((1700000100)<<26)|2, 1, '第二条（新 IP）');
            insert into msghost_tbl values(((1700000100)<<26)|2, 2, 0, 0);
            "#,
        )
        .unwrap();

        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(rep.imported, 2);
        assert_eq!(rep.sessions_new, 1, "两个 IP 只算一个新会话");

        let hist = st.read_history("192.168.1.11", 10);
        assert_eq!(hist.len(), 2, "两个 IP 的消息并入同一个会话");
        assert_eq!(hist[0]["text"], "第一条（老 IP）");
        assert_eq!(hist[1]["text"], "第二条（新 IP）");
        assert!(
            !st.list_sessions().iter().any(|s| s.key == "192.168.1.99"),
            "不应按新 IP 另开会话"
        );
        assert!(
            hist.iter().all(|r| r["peer"]["key"] == "192.168.1.11"),
            "记录内的 peer.key 快照都指向合并后的会话"
        );
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn imports_merge_into_existing_session_and_consolidate_old_files() {
        let st = temp_state("mergeexisting");
        // 现有会话：本应用的实时记录（无 imp），IP 192.168.1.50
        st.log_record("192.168.1.50", &serde_json::json!({
            "dir": "in", "kind": "text", "text": "现在用的地址", "pkt": 1, "ts": 1_700_000_001,
            "peer": {"key": "192.168.1.50", "nickname": "Alice", "host": "PC-ALICE"},
        }));
        // 旧版本导入产生的「纯导入」残留：Alice 换过 IP 的另一个文件，只含 imp 记录
        st.log_record("192.168.1.77", &serde_json::json!({
            "dir": "in", "kind": "text", "text": "旧地址的历史", "ts": 1_700_000_002,
            "imp": {"db": "sample", "id": 9001},
            "peer": {"key": "192.168.1.77", "nickname": "Alice", "host": "PC-ALICE"},
        }));

        let db = st.data_dir.join("sample.db");
        make_db(&db);
        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert!(rep.merged_sessions >= 1, "旧导入文件应按身份归并");

        // Alice 的导入消息并入同名同机的现有会话
        let hist = st.read_history("192.168.1.50", 100);
        assert!(
            hist.iter().any(|r| r["text"] == "你好"),
            "导入消息并入现有会话而非按新 IP 另开"
        );
        // 旧导入文件被并入目标会话并删除
        assert!(
            !st.list_sessions().iter().any(|s| s.key == "192.168.1.77"),
            "旧导入文件已归并删除"
        );
        assert!(
            hist.iter().any(|r| r["text"] == "旧地址的历史"),
            "旧导入记录搬到目标会话"
        );
        let old_rec = hist.iter().find(|r| r["text"] == "旧地址的历史").unwrap();
        assert_eq!(
            old_rec["peer"]["key"], "192.168.1.50",
            "归并后的旧记录 peer.key 快照指向目标会话"
        );
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn live_sessions_are_never_consolidated() {
        let st = temp_state("nomerge-live");
        // 192.168.1.60：真实会话（记录无 imp），归并目标只能往这里追加
        st.log_record("192.168.1.60", &serde_json::json!({
            "dir": "out", "kind": "text", "text": "发给 Bob 的话", "pkt": 1, "ts": 1_700_000_001,
            "peer": {"key": "192.168.1.60", "nickname": "Bob", "host": "PC-BOB"},
        }));
        let db = st.data_dir.join("sample.db");
        make_db(&db);
        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(rep.merged_sessions, 0, "真实会话文件不许被归并删除");

        let keys: Vec<String> = st.list_sessions().iter().map(|s| s.key.clone()).collect();
        assert!(keys.contains(&"192.168.1.60".to_string()), "真实会话保留");
        // Bob 的导入消息并入现有同名同机会话
        let hist = st.read_history("192.168.1.60", 100);
        assert!(
            hist.iter().any(|r| r["text"] == "周会改到三点"),
            "导入的 Bob 消息并入他的现有会话"
        );
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn non_ipmsg_sqlite_is_rejected_with_clear_error() {
        let st = temp_state("reject");
        let db = st.data_dir.join("other.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute("create table t(x)", []).unwrap();
        let err = import_ipmsg_db(&st, &db).unwrap_err();
        assert!(err.contains("官方"), "错误信息要能引导用户：{err}");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    /// 真实样本回归：设 OIM_IPMSG_SAMPLE=ipmsg.db 路径时启用（CI 无此环境变量自动跳过）
    #[test]
    fn real_sample_db_if_present() {
        let Ok(sample) = std::env::var("OIM_IPMSG_SAMPLE") else {
            return;
        };
        let path = Path::new(&sample);
        if !path.exists() {
            eprintln!("样本不存在，跳过：{sample}");
            return;
        }
        let st = temp_state("realsample");
        let rep = import_ipmsg_db(&st, path).expect("真实样本应能导入");
        eprintln!("imported={} skipped={} sessions_new={}", rep.imported, rep.skipped, rep.sessions_new);
        assert!(rep.imported > 0, "真实样本应有消息可导");

        // 全库记录健全性：时间合理、方向合法、无备忘录泄漏
        for sess in st.list_sessions() {
            for rec in st.read_history(&sess.key, usize::MAX) {
                assert!(rec["ts"].as_u64().unwrap_or(0) > 100_000_000, "ts 应为合理 unix 秒");
                let dir = rec["dir"].as_str().unwrap();
                assert!(dir == "in" || dir == "out");
                assert!(
                    rec["peer"]["nickname"].as_str().unwrap_or("") != " [ Memo ] ",
                    "备忘录不应被导入"
                );
            }
        }
        // 再导一次：全部去重
        let again = import_ipmsg_db(&st, path).unwrap();
        assert_eq!(again.imported, 0, "重复导入应为空操作");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }

    #[test]
    fn missing_attachment_tables_are_tolerated() {
        let st = temp_state("oldschema");
        let db = st.data_dir.join("old.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE msg_tbl(msg_id integer primary key, cmd integer, flags integer, body text, lines integer);
            CREATE TABLE host_tbl(host_id integer primary key, uid text, nick text, host text, addr text, gname text);
            CREATE TABLE msghost_tbl(msg_id integer, host_id integer, flags integer, idx integer);
            insert into host_tbl values(1,'dan','Dan','DAN-PC','172.16.0.9','');
            insert into msg_tbl(msg_id,flags,body) values(((1700000100)<<26)|7, 1, '老版本');
            insert into msghost_tbl values(((1700000100)<<26)|7, 1, 0, 0);
            "#,
        )
        .unwrap();
        let rep = import_ipmsg_db(&st, &db).unwrap();
        assert_eq!(rep.imported, 1);
        assert_eq!(st.read_history("172.16.0.9", 10)[0]["text"], "老版本");
        let _ = std::fs::remove_dir_all(&st.data_dir);
    }
}
