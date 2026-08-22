//! 网络层：UDP(2425) 用户发现与消息收发，TCP(2425) 文件传输服务与下载。
//!
//! 所有出站报文都从同一个绑定在协议端口上的主 socket 发出，
//! 保证对端看到的 (ip, port) 身份稳定。

use crate::protocol::{self as proto, cmd, fileattr, opt};
use crate::state::{now_secs, AppState, Config, OfferedFile, PeerInfo};
use serde_json::{json, Value};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// 共享网络上下文
pub struct NetCtx {
    pub st: Arc<AppState>,
    pub sock: Arc<UdpSocket>,
    pub port: u16,
}

static DL_SEQ: AtomicU32 = AtomicU32::new(1);

fn my_host() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "localhost".into())
        })
        .clone()
}

fn my_user(cfg: &Config) -> String {
    if cfg.nickname.is_empty() {
        my_host()
    } else {
        cfg.nickname.clone()
    }
}

/// 本机所有 IPv4 地址（用于过滤自身广播回声）
fn local_ipv4_set() -> &'static std::collections::HashSet<IpAddr> {
    static SET: std::sync::OnceLock<std::collections::HashSet<IpAddr>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        local_ip_address::list_afinet_netifas()
            .map(|v| v.into_iter().map(|(_, ip)| ip).collect())
            .unwrap_or_default()
    })
}

/* ================= 广播目标计算 ================= */

/// 全局广播 + 各网卡的定向广播(/24 推断)；Linux 上补充精确广播地址
pub fn broadcast_targets() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = vec![Ipv4Addr::BROADCAST.into()];
    let push = |ip: IpAddr, out: &mut Vec<IpAddr>| {
        if !out.contains(&ip) {
            out.push(ip);
        }
    };
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifaces {
            if let IpAddr::V4(v4) = ip {
                if v4.is_loopback() || v4.is_unspecified() {
                    continue;
                }
                let o = v4.octets();
                push(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 255)), &mut out);
            }
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(b) = local_ip_address::linux::local_broadcast_ip() {
        if let IpAddr::V4(b4) = b {
            push(IpAddr::V4(b4), &mut out);
        }
    }
    out
}

/* ================= 启动 ================= */

pub async fn start_network(st: Arc<AppState>, port: u16) -> io::Result<Arc<NetCtx>> {
    let sock = Arc::new(UdpSocket::bind(("0.0.0.0", port)).await?);
    sock.set_broadcast(true)?;

    let ctx = Arc::new(NetCtx {
        st,
        sock,
        port,
    });

    announce(&ctx).await;
    spawn_udp_loop(ctx.clone());
    spawn_tcp_server(ctx.clone());
    spawn_ticker(ctx.clone());
    Ok(ctx)
}

fn spawn_udp_loop(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            match ctx.sock.recv_from(&mut buf).await {
                Ok((n, from)) => {
                    if n == 0 {
                        continue;
                    }
                    let data = buf[..n].to_vec();
                    let ctx2 = ctx.clone();
                    tokio::spawn(async move {
                        handle_datagram(&ctx2, &data, from).await;
                    });
                }
                Err(e) => {
                    eprintln!("[udp] recv error: {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
}

fn spawn_ticker(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(45));
        tick.tick().await; // 第一次立即返回，跳过（启动时已 announce）
        loop {
            tick.tick().await;
            announce(&ctx).await;
            let stale = ctx.st.prune_stale_peers(30 * 60);
            if !stale.is_empty() {
                ctx.st.emit("users-updated", json!({}));
            }
        }
    });
}

/// 向所有广播地址发送上线/下线通告
pub async fn announce(ctx: &NetCtx) {
    let cfg = ctx.st.config();
    let pkt = proto::Packet {
        extra: proto::build_entry_extra(&cfg.nickname, &cfg.group, &cfg.encoding),
        ..proto::Packet::new(cmd::BR_ENTRY)
    };
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    let targets: Vec<SocketAddr> = broadcast_targets()
        .into_iter()
        .map(|ip| SocketAddr::from((ip, ctx.port)))
        .collect();
    for t in targets {
        let _ = ctx.sock.send_to(&bytes, t).await;
    }
}

/// 向指定地址单播上线通告（自检/定向刷新用）
pub async fn announce_unicast(ctx: &NetCtx, addrs: &[SocketAddr]) {
    let cfg = ctx.st.config();
    let pkt = proto::Packet {
        extra: proto::build_entry_extra(&cfg.nickname, &cfg.group, &cfg.encoding),
        ..proto::Packet::new(cmd::BR_ENTRY)
    };
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    for a in addrs {
        let _ = ctx.sock.send_to(&bytes, *a).await;
    }
}

/// 进程退出时尽力广播 BR_EXIT（同步阻塞，短暂）
pub fn announce_exit_blocking(st: &AppState, port: u16) {
    let cfg = st.config();
    let pkt = proto::Packet::new(cmd::BR_EXIT);
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    if let Ok(sock) = std::net::UdpSocket::bind(("0.0.0.0", 0)) {
        let _ = sock.set_broadcast(true);
        for ip in broadcast_targets() {
            let _ = sock.send_to(&bytes, SocketAddr::from((ip, port)));
        }
        let peers: Vec<String> = st.peers.lock().map(|p| p.keys().cloned().collect()).unwrap_or_default();
        for key in peers {
            if let Some(addr) = parse_peer_key(&key, port) {
                let _ = sock.send_to(&bytes, addr);
            }
        }
    }
}

fn parse_peer_key(key: &str, default_port: u16) -> Option<SocketAddr> {
    let (ip, p) = key.rsplit_once(':')?;
    let ip: IpAddr = ip.parse().ok()?;
    let port: u16 = p.parse().unwrap_or(default_port);
    Some(SocketAddr::from((ip, port)))
}

/* ================= 入站处理 ================= */

async fn handle_datagram(ctx: &NetCtx, data: &[u8], from: SocketAddr) {
    let Some(pkt) = proto::parse(data) else {
        return;
    };
    #[cfg(feature = "net_debug")]
    eprintln!("[udp] <- {from} cmd={:#010x} user={:?} extra_len={}", pkt.command, pkt.user, pkt.extra.len());
    // 过滤自身广播回声（定向广播会被内核本地回投）
    if from.port() == ctx.port && local_ipv4_set().contains(&from.ip()) {
        return;
    }
    if !ctx.st.mark_seen(from.ip(), pkt.pkt_no) {
        return; // 重复包
    }
    // 基本命令取低 8 位（官方规范：所有选项标志位于 bit8 以上）
    let base = pkt.command & 0xFF;
    let key = from.to_string();

    match base {
        cmd::BR_ENTRY => {
            let (nick, group) = proto::parse_entry_extra(&pkt.extra);
            let added = ctx.st.upsert_peer(PeerInfo {
                key: key.clone(),
                ip: from.ip().to_string(),
                port: from.port(),
                nickname: nick,
                group,
                host: pkt.host.clone(),
                user: pkt.user.clone(),
                last_seen: now_secs(),
            });
            // 回应 ANSENTRY（单播），携带自己的昵称\0群组
            let cfg = ctx.st.config();
            let ans = proto::Packet {
                extra: proto::build_entry_extra(&cfg.nickname, &cfg.group, &cfg.encoding),
                ..proto::Packet::new(cmd::ANSENTRY)
            };
            let bytes = ans.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
            if added {
                ctx.st.emit("users-updated", json!({}));
            }
        }
        cmd::ANSENTRY | cmd::BR_ABSENCE => {
            let (nick, group) = proto::parse_entry_extra(&pkt.extra);
            let added = ctx.st.upsert_peer(PeerInfo {
                key: key.clone(),
                ip: from.ip().to_string(),
                port: from.port(),
                nickname: nick,
                group,
                host: pkt.host.clone(),
                user: pkt.user.clone(),
                last_seen: now_secs(),
            });
            ctx.st.emit("users-updated", json!({}));
            let _ = added;
        }
        cmd::BR_EXIT => {
            if ctx.st.remove_peer(&key).is_some() {
                ctx.st.emit("users-updated", json!({}));
            }
        }
        cmd::SENDMSG => handle_sendmsg(ctx, from, &pkt, &key).await,
        cmd::READMSG => {
            // 对端已读回执：extra = 原消息包编号，把对应出站消息标记为已读
            let no = String::from_utf8_lossy(&pkt.extra)
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u32>()
                .ok();
            #[cfg(feature = "net_debug")]
            eprintln!("[read] READMSG from {from} no={no:?}");
            if let Some(no) = no {
                if let Some(key) = resolve_session_key(ctx, from.ip()) {
                    let changed = ctx.st.mark_out_read(&key, no);
                    #[cfg(feature = "net_debug")]
                    eprintln!("[read] key={key} changed={changed}");
                    if changed {
                        ctx.st.emit("msg-read", json!({"key": key, "pkt": no}));
                    }
                }
            }
        }
        cmd::GETINFO => {
            let cfg = ctx.st.config();
            let reply = proto::Packet::new(cmd::SENDINFO | opt::AUTORETOPT);
            let mut r = reply;
            r.extra = proto::encode_out(concat!("OpenIPMsg v", env!("CARGO_PKG_VERSION")), &cfg.encoding);
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
        }
        cmd::RELEASEFILES => {
            // 对端放弃接收：释放对应的文件槽
            let first = pkt.extra.split(|&b| b == b':').next().unwrap_or(b"");
            if let Ok(no) = String::from_utf8_lossy(first).trim().parse::<u32>() {
                ctx.st.offered.lock().unwrap().retain(|(p, _), _| *p != no);
            }
        }
        // GETPUBKEY：不支持加密协商，忽略即可（对端会回退明文）
        _ => {}
    }
}

async fn handle_sendmsg(ctx: &NetCtx, from: SocketAddr, pkt: &proto::Packet, key: &str) {
    let attach = pkt.command & opt::FILEATTACHOPT != 0;
    let files = if attach {
        proto::parse_file_entries(&pkt.extra)
    } else {
        Vec::new()
    };

    let no_add_list = pkt.command & opt::NOADDLISTOPT != 0;
    if !no_add_list && !ctx.st.peers.lock().unwrap().contains_key(key) {
        // 陌生来源直接发消息：按包头注册用户
        let added = ctx.st.upsert_peer(PeerInfo {
            key: key.to_string(),
            ip: from.ip().to_string(),
            port: from.port(),
            nickname: pkt.user.clone(),
            group: String::new(),
            host: pkt.host.clone(),
            user: pkt.user.clone(),
            last_seen: now_secs(),
        });
        if added {
            ctx.st.emit("users-updated", json!({}));
        }
    } else {
        ctx.st.touch_peer(key);
    }

    let display = {
        let peers = ctx.st.peers.lock().unwrap();
        peers
            .get(key)
            .map(|p| p.nickname.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| pkt.user.clone())
    };

    let kind = if files.is_empty() { "text" } else { "file" };
    let rec = json!({
        "dir": "in",
        "kind": kind,
        "text": proto::text_of(pkt),
        "files": files.iter().map(|f| json!({
            "id": f.id, "name": f.name, "size": f.size, "state": "pending",
        })).collect::<Vec<_>>(),
        "ts": now_secs(),
        "pkt": pkt.pkt_no,
        "peer": {"key": key, "nickname": display, "host": pkt.host},
        // 对端要求已读回执：待用户查看后由 mark_read 发送 READMSG
        "need_read": pkt.command & opt::READCHECKOPT != 0,
        "read": false,
    });
    ctx.st.log_record(key, &rec);
    ctx.st.emit("msg-in", json!({"key": key, "msg": rec}));
}

/* ================= 出站消息 ================= */

/// 发送文本/文件消息。paths 为空则纯文本。
pub async fn send_message(
    ctx: &NetCtx,
    key: &str,
    text: &str,
    paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let peer = ctx
        .st
        .peers
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or_else(|| "对方不在线或尚未发现".to_string())?;
    let target = parse_peer_key(&peer.key, ctx.port).ok_or("无效的对方地址")?;

    let cfg = ctx.st.config();
    let pkt_no = proto::next_packet_no();

    // 注册文件槽必须在发包前完成（对端可能立刻来取）
    let mut entries: Vec<proto::FileEntry> = Vec::new();
    let mut inserted: Vec<(u32, u32)> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let path = PathBuf::from(p);
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("无法读取文件 {}: {e}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("暂不支持发送目录：{}", path.display()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let id = (i + 1) as u32;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(proto::FileEntry {
            id,
            name: name.clone(),
            size: meta.len(),
            mtime,
            attr: fileattr::REGULAR,
        });
        inserted.push((pkt_no, id));
        ctx.st.offered.lock().unwrap().insert(
            (pkt_no, id),
            OfferedFile {
                path,
                size: entries.last().unwrap().size,
            },
        );
    }

    let mut extra = proto::encode_out(text, &cfg.encoding);
    if !entries.is_empty() {
        extra.push(0);
        let joined: Vec<String> = entries.iter().map(|e| e.serialize()).collect();
        extra.extend_from_slice(joined.join("\u{7}").as_bytes());
    }

    // 请求已读回执：对端查看后应回复 READMSG
    let command = cmd::SENDMSG
        | opt::READCHECKOPT
        | if entries.is_empty() { 0 } else { opt::FILEATTACHOPT };
    let mut pkt = proto::Packet::new(command).with_pkt_no(pkt_no);
    pkt.extra = extra;
    let bytes = pkt.encode(&my_user(&cfg), &my_host());

    if let Err(e) = ctx.sock.send_to(&bytes, target).await {
        // 发送失败：回滚文件槽
        let mut offered = ctx.st.offered.lock().unwrap();
        for k in &inserted {
            offered.remove(k);
        }
        return Err(format!("UDP 发送失败: {e}"));
    }

    let kind = if entries.is_empty() { "text" } else { "file" };
    let rec = json!({
        "dir": "out",
        "kind": kind,
        "text": text,
        "files": entries.iter().zip(paths.iter()).map(|(e, p)| json!({
            "id": e.id, "name": e.name, "size": e.size, "path": p, "state": "sent",
        })).collect::<Vec<_>>(),
        "ts": now_secs(),
        "pkt": pkt_no,
        "peer": {"key": peer.key, "nickname": peer.nickname, "host": peer.host, "group": peer.group},
        "rcpt": true,
        "read": false,
    });
    ctx.st.log_record(key, &rec);
    Ok(rec)
}

/* ================= 已读回执 ================= */

/// 解析 READMSG 来源对应的会话 key：优先精确匹配，其次按 IP 匹配
fn resolve_session_key(ctx: &NetCtx, ip: IpAddr) -> Option<String> {
    let peers = ctx.st.peers.lock().unwrap();
    let exact = format!("{}:{}", ip, ctx.port);
    if peers.contains_key(&exact) {
        return Some(exact);
    }
    peers
        .values()
        .find(|p| p.ip == ip.to_string())
        .map(|p| p.key.clone())
}

/// 标记入站消息为已读，并对要求回执的消息向对端发送 READMSG。
/// 返回成功发出的回执数（对方离线时本地仍然标记为已读）。
pub async fn mark_read_and_receipt(
    ctx: &NetCtx,
    key: &str,
    pkts: &[u32],
) -> Result<usize, String> {
    if pkts.is_empty() {
        return Ok(0);
    }
    ctx.st.mark_in_read(key, pkts);

    let peer = ctx.st.peers.lock().unwrap().get(key).cloned();
    let Some(peer) = peer else {
        return Ok(0); // 对方不在线：仅本地标记
    };
    let Some(target) = parse_peer_key(&peer.key, ctx.port) else {
        return Ok(0);
    };

    let cfg = ctx.st.config();
    let mut sent = 0;
    for p in pkts {
        let mut r = proto::Packet::new(cmd::READMSG | opt::AUTORETOPT);
        r.extra = p.to_string().into_bytes();
        let bytes = r.encode(&my_user(&cfg), &my_host());
        if ctx.sock.send_to(&bytes, target).await.is_ok() {
            sent += 1;
        }
    }
    Ok(sent)
}

/* ================= TCP 文件传输 ================= */

fn spawn_tcp_server(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let listener = match TcpListener::bind(("0.0.0.0", ctx.port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[tcp] bind {} failed: {e}", ctx.port);
                return;
            }
        };
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let ctx2 = ctx.clone();
                    tokio::spawn(async move {
                        serve_getfile(&ctx2, stream).await;
                    });
                }
                Err(e) => {
                    eprintln!("[tcp] accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
    });
}

/// 服务端：解析 GETFILEDATA 请求并回传文件字节流
async fn serve_getfile(ctx: &NetCtx, mut stream: TcpStream) {
    // 读请求行：容忍有无结尾换行
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut req_pkt = None;
    while buf.len() < 1024 && Instant::now() < deadline {
        let n = match tokio::time::timeout(Duration::from_millis(300), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => n,
            Ok(Ok(_)) => break, // EOF
            Ok(Err(_)) => break,
            Err(_) => break, // 超时就用已有数据尝试解析
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = proto::parse(&buf) {
            req_pkt = Some(p);
            break;
        }
    }
    let Some(req) = req_pkt else { return };
    if req.command & 0xFF != cmd::GETFILEDATA {
        return;
    }
    // extra: pkt_id:file_id:offset
    let parts: Vec<String> = String::from_utf8_lossy(&req.extra)
        .split(':')
        .map(|s| s.trim().to_string())
        .collect();
    if parts.len() < 3 {
        return;
    }
    let offer_pkt = num_flex_dec_first(&parts[0]).unwrap_or(0) as u32;
    let file_id = num_flex_dec_first(&parts[1]).unwrap_or(0) as u32;
    let offset = num_flex_dec_first(&parts[2]).unwrap_or(0);

    let slot = ctx.st.offered.lock().unwrap().get(&(offer_pkt, file_id)).map(|o| (o.path.clone(), o.size));
    let Some((path, _size)) = slot else { return };

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return,
    };
    if offset > 0 {
        if file.seek(io::SeekFrom::Start(offset)).await.is_err() {
            return;
        }
    }
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&chunk[..n]).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = stream.flush().await;
}

fn num_flex_dec_first(t: &str) -> Option<u64> {
    let t = t.trim();
    if let Ok(v) = t.parse::<u64>() {
        return Some(v);
    }
    u64::from_str_radix(t.trim_start_matches("0x"), 16).ok()
}

/// 客户端：从对端下载一个附件到配置的接收目录（后台任务，进度走事件）
pub async fn download_file_task(
    ctx: &NetCtx,
    key: &str,
    pkt_no: u32,
    file_id: u32,
    name: &str,
) -> Result<PathBuf, String> {
    let peer = ctx
        .st
        .peers
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or_else(|| "对方不在线".to_string())?;
    let target = parse_peer_key(&peer.key, ctx.port).ok_or("无效的对方地址")?;

    let cfg = ctx.st.config();
    let dir = PathBuf::from(if cfg.download_dir.is_empty() {
        ctx.st.data_dir.join("接收文件").to_string_lossy().into_owned()
    } else {
        cfg.download_dir.clone()
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建接收目录失败: {e}"))?;

    let safe_name: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '_' } else { c })
        .collect();
    let final_path = unique_path(&dir.join(&safe_name));
    let tmp_path = final_path.with_extension(format!(
        "part{}",
        DL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let result = fetch_to_file(ctx, key, target, pkt_no, file_id, &tmp_path).await;
    match result {
        Ok(total) => {
            std::fs::rename(&tmp_path, &final_path)
                .map_err(|e| format!("保存文件失败: {e}"))?;
            ctx.st.emit(
                "file-progress",
                json!({"key": key, "pkt": pkt_no, "file_id": file_id,
                       "transferred": total, "total": total, "done": true,
                       "path": final_path.to_string_lossy()}),
            );
            ctx.st.update_history_file(key, pkt_no, file_id, |f| {
                f["state"] = "done".into();
                f["path"] = Value::String(final_path.to_string_lossy().into_owned());
            });
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            ctx.st.emit(
                "file-progress",
                json!({"key": key, "pkt": pkt_no, "file_id": file_id,
                       "transferred": 0, "total": 0, "done": false, "error": e}),
            );
            ctx.st.update_history_file(key, pkt_no, file_id, |f| {
                f["state"] = "failed".into();
            });
            Err(e)
        }
    }
}

async fn fetch_to_file(
    ctx: &NetCtx,
    key: &str,
    target: SocketAddr,
    pkt_no: u32,
    file_id: u32,
    tmp: &std::path::Path,
) -> Result<u64, String> {
    let cfg = ctx.st.config();
    let mut stream = tokio::time::timeout(Duration::from_secs(6), TcpStream::connect(target))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|e| format!("连接失败: {e}"))?;

    let req = format!(
        "1:{}:{}:{}:{}:{}:{}:0\n",
        proto::next_packet_no(),
        my_user(&cfg),
        my_host(),
        cmd::GETFILEDATA,
        pkt_no,
        file_id
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("发送请求失败: {e}"))?;

    let mut file = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    let mut total: u64 = 0;
    let mut last_emit = Instant::now();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("接收数据失败: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&chunk[..n])
            .await
            .map_err(|e| format!("写入文件失败: {e}"))?;
        total += n as u64;
        if last_emit.elapsed() >= Duration::from_millis(150) {
            last_emit = Instant::now();
            ctx.st.emit(
                "file-progress",
                json!({"key": key, "pkt": pkt_no, "file_id": file_id,
                       "transferred": total, "total": 0, "done": false}),
            );
        }
    }
    file.flush().await.map_err(|e| format!("写入失败: {e}"))?;
    drop(file);
    Ok(total)
}

/// 目标路径已存在时追加 (1)/(2)…
fn unique_path(path: &std::path::Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for i in 1..10000u32 {
        let candidate = if ext.is_empty() {
            path.with_file_name(format!("{stem}({i})"))
        } else {
            path.with_file_name(format!("{stem}({i}).{ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!("{}-{}.bin", stem, proto::next_packet_no()))
}
