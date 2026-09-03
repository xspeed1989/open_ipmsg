//! 网络层：UDP(2425) 用户发现与消息收发，TCP(2425) 文件传输服务与下载。
//!
//! 所有出站报文都从同一个绑定在协议端口上的主 socket 发出，
//! 保证对端看到的 (ip, port) 身份稳定。

use crate::crypto;
use crate::ipdict::{self as ipd};
use crate::protocol::{self as proto, cmd, fileattr, opt};
use crate::state::{
    now_secs, AppState, Config, DirMember, OfferedFile, PeerInfo, PendingOut, RetryOut,
};
use serde_json::{json, Value};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// 共享网络上下文
pub struct NetCtx {
    pub st: Arc<AppState>,
    pub sock: Arc<UdpSocket>,
    /// IPv6 组播套接字（ff15::979 / ff02::1 成员发现；本机无 IPv6 时为 None）
    pub v6_sock: tokio::sync::Mutex<Option<Arc<UdpSocket>>>,
    pub port: u16,
}

/// IPv6 站点组播地址（官方 §3-1：ff15::979）
const IPV6_MCAST_SITE: Ipv6Addr = Ipv6Addr::new(0xff15, 0, 0, 0, 0, 0, 0, 0x0979);
/// IPv6 链路组播地址（官方 §3-1：ff02::1，localhost）
const IPV6_MCAST_LINK: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);

/// 枚举本机 IPv6 接口（scope_id ≥ 1 才可用）：返回 (scope_id, 全局地址集合)。
/// 用于组播加入与按接口发送（链路组播需要接口作用域）。
fn v6_ifaces() -> Vec<(u32, Ipv6Addr)> {
    let mut out = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ia = &*cur;
            if !ia.ifa_addr.is_null() && (*ia.ifa_addr).sa_family as i32 == libc::AF_INET6 {
                let sin6 = &*(ia.ifa_addr as *const libc::sockaddr_in6);
                let addr = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                if sin6.sin6_scope_id != 0
                    && !addr.is_unspecified()
                    && !addr.is_loopback()
                    && !out.iter().any(|(s, a)| *s == sin6.sin6_scope_id && a == &addr)
                {
                    out.push((sin6.sin6_scope_id, addr));
                }
            }
            cur = ia.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

/// 创建 IPv6 组播套接字：绑定 [::]:端口，逐接口加入 ff15::979 与 ff02::1。
/// 失败（本机无 IPv6/被禁用）返回 None，调用方记 diag 后静默降级。
/// `hermetic`（测试模式的 quiet/loopback 启动）：只加入回环接口的组，
/// 避免真实局域网的 ff02::1 全节点组播（其它机器上的 IPMsg 实例）污染自检。
fn create_v6_multicast_sock(port: u16, hermetic: bool) -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_only_v6(true)?;
    sock.set_reuse_address(true)?;
    // 组播回环：同一主机多实例互见（自检双实例依赖）
    sock.set_multicast_loop_v6(true)?;
    sock.bind(&SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0).into())?;
    if !hermetic {
        // 生产模式：逐接口加入站点/链路组播组
        for (scope, _addr) in v6_ifaces() {
            let _ = sock.join_multicast_v6(&IPV6_MCAST_SITE, scope);
            let _ = sock.join_multicast_v6(&IPV6_MCAST_LINK, scope);
        }
    }
    // 测试模式（hermetic）：不加入任何组 —— 自检全程单播互达，
    // 避免真实局域网的 ff02::1 全节点组播（其它 IPMsg 实例）污染断言
    let std_sock: std::net::UdpSocket = sock.into();
    std_sock.set_nonblocking(true)?;
    UdpSocket::from_std(std_sock)
}

static DL_SEQ: AtomicU32 = AtomicU32::new(1);

/// 全局递增的公告文件 ID（模拟真实客户端的全局大数风格，
/// 避免每条消息从 1 重计被对端按 ID 追踪时忽略）
static FILE_ID_SEQ: AtomicU32 = AtomicU32::new(0x1000_0000);

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

/// 本机所有地址（IPv4 + IPv6 + 回环，用于过滤自身广播/组播回声）
fn local_ip_set() -> &'static std::collections::HashSet<IpAddr> {
    static SET: std::sync::OnceLock<std::collections::HashSet<IpAddr>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        let mut set: std::collections::HashSet<IpAddr> = local_ip_address::list_afinet_netifas()
            .map(|v| v.into_iter().map(|(_, ip)| ip).collect())
            .unwrap_or_default();
        set.insert(IpAddr::V4(Ipv4Addr::LOCALHOST));
        set.insert(IpAddr::V6(Ipv6Addr::LOCALHOST));
        set
    })
}

/* ================= 能力广告与上线通告 ================= */

/// 上线类报文的能力位。
/// 广播/单播通告、ANSENTRY 应答、预握手 GETPUBKEY 扩展部共用这一口径（spec §5/§7）。
///
/// - FILEATTACHOPT / CLIPBOARDOPT：与加密无关，只要 TCP 服务可用就声明
///   （与官方 HostStatus() 一致）。官方 senddlg.cpp SendMsgSetUsers 只有在对端
///   声明 CLIPBOARDOPT 时才把「粘贴图片」作为附件发出；不声明则撤销
///   FILEATTACHOPT，图片丢失只剩空格占位——粘图互发必须声明。
/// - ENCRYPTOPT / CAPFILEENCOPT / ENCEXTMSGOPT：受加密总开关控制（开 → 声明）。
fn entry_caps(cfg: &Config) -> u32 {
    let mut caps = opt::FILEATTACHOPT | opt::CLIPBOARDOPT;
    if cfg.encrypt {
        // ENCEXTMSGOPT：官方 Entry 恒带（0x0fe40003 实测），声明支持加密扩展消息
        caps |= opt::ENCRYPTOPT | opt::CAPFILEENCOPT | opt::ENCEXTMSGOPT;
    }
    if cfg.ipdict_enabled {
        // v5 并存格式能力位（官方 HostStatus 同款）
        caps |= opt::CAPIPDICTOPT;
    }
    if cfg.dir_mode == "master" {
        // 成员主角色：官方 HostStatus 在 DIRMODE_MASTER 时带 DIR_MASTER|DIALUPOPT
        caps |= opt::DIR_MASTER | opt::DIALUPOPT;
    }
    caps
}

/// 我方线上版本串（官方 `VS:` 行/`CVER` 键的 hex 元组布局）
pub fn my_ver_hex_info() -> String {
    proto::ver_hex_info()
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
    start_network_impl(st, None, port, true).await
}

/// 测试用：与 `start_network` 相同，但跳过启动时对 2425 端口的上线广播。
///
/// 无人值守自检在同一台机器上并跑多个实例；若各实例启动即广播，本机/局域网
/// 真实运行的 open-ipmsg（2425 端口）会应答这些广播，用相同的会话键（对端 IP）
/// 抢先注册、甚至先于被测对端完成公钥握手 —— 公钥缓存一旦被真实客户端占据，
/// 握手守护（有缓存不握手）就会跳过与被测对端的握手，造成环境性抖动。
pub(crate) async fn start_network_quiet(st: Arc<AppState>, port: u16) -> io::Result<Arc<NetCtx>> {
    start_network_impl(st, None, port, false).await
}

/// 测试用：绑定到指定 IP 的静默启动（多实例自检的隔离手段）。
///
/// 自检的 A/B/C 三个实例若都绑 0.0.0.0，会话键全是 127.0.0.1，任何一台
/// 本机/局域网真实 open-ipmsg 的应答都能混进它们各自的会话（见 start_network_quiet
/// 注释）；把 B 绑到独占的 127.0.0.2 后，A↔B 会话的键（127.0.0.2）不受
/// 127.0.0.1 上任何外来流量影响，预握手断言才确定。C 保留 127.0.0.1，因为
/// 撤回场景的语义就是「同一 IP 换新实例」。
pub(crate) async fn start_network_loopback(
    st: Arc<AppState>,
    bind: Ipv4Addr,
    port: u16,
) -> io::Result<Arc<NetCtx>> {
    start_network_impl(st, Some(bind), port, false).await
}

async fn start_network_impl(
    st: Arc<AppState>,
    bind: Option<Ipv4Addr>,
    port: u16,
    announce_start: bool,
) -> io::Result<Arc<NetCtx>> {
    let sock = Arc::new(match bind {
        Some(ip) => UdpSocket::bind(SocketAddr::from((ip, port))).await?,
        None => UdpSocket::bind(("0.0.0.0", port)).await?,
    });
    sock.set_broadcast(true)?;

    // IPv6 组播通道：尽力创建（无 IPv6 环境静默降级；v6_mcast 关闭时跳过）；
    // quiet/loopback 启动（自检）走回环节点，隔离真实局域网
    let cfg_start = st.config();
    let v6_sock = if !cfg_start.v6_mcast {
        oim_log!("[udp6] IPv6 组播已按配置关闭（纯 IPv4）");
        None
    } else {
        let hermetic = !announce_start;
        match create_v6_multicast_sock(port, hermetic) {
        Ok(s) => {
            oim_log!("[udp6] IPv6 组播已启用（ff15::979 / ff02::1）");
            Some(s)
        }
        Err(e) => {
            oim_log!("[udp6] IPv6 组播不可用（降级纯 IPv4）: {e}");
            None
        }
        }
    };

    let ctx = Arc::new(NetCtx {
        st,
        sock,
        v6_sock: tokio::sync::Mutex::new(v6_sock.map(Arc::new)),
        port,
    });

    if announce_start {
        announce(&ctx).await;
        // 启动 2 分钟的主机列表获取窗口（官方 entryStartTime 语义）
        ctx.st.open_hostlist_window(120);
    }
    spawn_udp_loop(ctx.clone());
    spawn_v6_udp_loop(ctx.clone());
    spawn_tcp_server(ctx.clone());
    spawn_ticker(ctx.clone());
    spawn_retry_loop(ctx.clone());
    spawn_dir_loop(ctx.clone());
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
                    oim_log!("[udp] recv error: {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
}

/// IPv6 组播接收循环：与 v4 主循环走同一 handle_datagram（身份键仍为源 IP 字符串）
fn spawn_v6_udp_loop(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let sock = {
            let guard = ctx.v6_sock.lock().await;
            match guard.as_ref() {
                Some(s) => Some(s.clone()),
                None => None,
            }
        };
        let Some(sock) = sock else { return };
        let mut buf = vec![0u8; 65535];
        loop {
            match sock.recv_from(&mut buf).await {
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
                    oim_log!("[udp6] recv error: {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
}

/// 在线消息送达重发循环（官方 §4-12 確認・リトライ）：每秒检查重发队列，
/// 过期未确认（RETRY_INTERVAL_SECS）的重发同一包号，超过 RETRY_MAX 次放弃。
fn spawn_retry_loop(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.tick().await;
        loop {
            tick.tick().await;
            let keys = ctx.st.retry_keys();
            for k in keys {
                retry_pending_for(ctx.clone(), &k).await;
            }
        }
    });
}

/// 成员主目录服务循环（DIR_MASTER）：成员侧定期 POLL，主侧定期发 DIR_PACKET。
fn spawn_dir_loop(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(45));
        tick.tick().await;
        loop {
            tick.tick().await;
            dir_tick(&ctx).await;
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
            // 离线消息复查：对方在线就重投（送达以 RECVMSG 确认为准，
            // 这里只负责「还在线就再发一遍」，失败消息由对端按包号去重）
            let keys: Vec<String> = {
                let peers = ctx.st.peers.lock().unwrap();
                peers.keys().cloned().collect()
            };
            for k in keys {
                flush_pending_for(&ctx, &k).await;
            }
            let stale = ctx.st.prune_stale_peers(30 * 60);
            if !stale.is_empty() {
                ctx.st.emit("users-updated", json!({}));
            }
            // 成员主侧：清理超时未 POLL 的成员
            let cfg = ctx.st.config();
            if cfg.dir_mode == "master" {
                let gone = ctx.st.prune_dir_members(3 * 60);
                for g in &gone {
                    ctx.st.diag(&format!("dir-master: 成员 {g} 停止 POLL，移除"));
                }
                if !gone.is_empty() {
                    push_dir_packet(&ctx, "成员离线").await;
                }
            }
        }
    });
}

/* ================= 在线消息送达重发（官方 §4-12 確認・リトライ） ================= */

/// 送达重发间隔（秒）：首次发送后每 4s 一重发
pub const RETRY_INTERVAL_SECS: u64 = 4;
/// 最长重发次数（含首次共发送 1+RETRY_MAX 次）
pub const RETRY_MAX: u32 = 4;

/// 重发某个会话到期的未确认消息（同一包号、同一文件 ID）。
async fn retry_pending_for(ctx: Arc<NetCtx>, key: &str) {
    let now = now_secs();
    for item in ctx.st.retry_for(key) {
        if item.ts + RETRY_INTERVAL_SECS > now {
            continue; // 未到重发点
        }
        // 独占计数：超限即放弃（该条消息保持「已发送」状态）
        if !ctx.st.bump_retry(key, item.pkt) {
            ctx.st.diag(&format!("-> {key} pkt={} 重发 {} 次未确认，放弃", item.pkt, RETRY_MAX));
            continue;
        }
        // 对方离线：整体转入待投递队列（对方回来时补投）
        let peer = ctx.st.peers.lock().unwrap().get(key).cloned();
        let Some(peer) = peer else {
            ctx.st.demote_retry_to_pending(key);
            return;
        };
        let Some(target) = peer_addr(&peer) else {
            continue;
        };
        // 附件路径复查：文件已丢失 → 放弃重发（气泡保持原状）
        let mut ok = true;
        for (p, e) in item.paths.iter().zip(item.entries.iter()) {
            let meta = match std::fs::metadata(p) {
                Ok(m) => m,
                Err(_) => {
                    ok = false;
                    ctx.st.diag(&format!("-> {key} pkt={} 附件丢失，放弃重发：{p}", item.pkt));
                    break;
                }
            };
            let want_dir = e.attr & 0xFF == fileattr::DIR;
            if meta.is_dir() != want_dir {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        // 重登记文件槽（ID 必须与公告一致，直接使用条目里的 ID）
        let cfg = ctx.st.config();
        let utf8 = proto::is_utf8_mode(&cfg.encoding);
        for (p, e) in item.paths.iter().zip(item.entries.iter()) {
            ctx.st.prune_offered();
            ctx.st.offered.lock().unwrap().insert(
                (item.pkt, e.id),
                OfferedFile {
                    path: PathBuf::from(p),
                    size: e.size,
                    is_dir: e.attr & 0xFF == fileattr::DIR,
                    ts: now_secs(),
                    utf8,
                },
            );
        }
        // 重组公告（与 send_message 相同的拼接）
        let mut extra = proto::encode_out(&item.text, &cfg.encoding);
        if !item.entries.is_empty() {
            extra.push(0);
            let joined: Vec<String> = item
                .entries
                .iter()
                .map(|e| e.serialize(&cfg.encoding))
                .collect();
            extra.extend_from_slice(joined.join("\u{7}").as_bytes());
            extra.push(0x07);
        }
        let want_rcpt = item.entries.is_empty();
        // 出站加密决策与 send_message 一致
        let mut enc = false;
        let mut wire_extra = extra.clone();
        if cfg.encrypt {
            if let Some(pubk) = ctx.st.peer_pubkey(key) {
                match crypto::seal_message(&pubk, &ctx.st.own_keypair(), &plain_payload(&extra)) {
                    Ok(sealed) => {
                        wire_extra = sealed.into_bytes();
                        enc = true;
                    }
                    Err(e) => {
                        ctx.st.diag(&format!("-> {key} 重发加密失败（pkt={}），放弃：{e}", item.pkt));
                        continue;
                    }
                }
            }
        }
        let command = cmd::SENDMSG
            | opt::SENDCHECKOPT
            | if want_rcpt { opt::READCHECKOPT } else { 0 }
            | if item.entries.is_empty() { 0 } else { opt::FILEATTACHOPT }
            | if utf8 { opt::UTF8OPT } else { 0 }
            | if enc && !item.entries.is_empty() { opt::ENCEXTMSGOPT } else { 0 }
            | if enc { opt::ENCRYPTOPT } else { 0 };
        let mut pkt = proto::Packet::new(command).with_pkt_no(item.pkt);
        pkt.extra = wire_extra;
        let bytes = pkt.encode(&my_user(&cfg), &my_host());
        // 中继会话/配置代理时重发也走 AGENT 包裹（与 send_message 同策）
        let origin_ip: IpAddr = ctx
            .sock
            .local_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let inner_ip: IpAddr = peer.ip.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let (wire, send_to) = match ctx.st.relay_agent(key) {
            Some(agent) => (
                wrap_agent_packet(&cfg, &inner_ip, &origin_ip, &bytes),
                agent,
            ),
            None => match parse_agent_addr(&cfg.agent_addr) {
                Some(agent) => (
                    wrap_agent_packet(&cfg, &inner_ip, &origin_ip, &bytes),
                    agent,
                ),
                None => (bytes, target),
            },
        };
        if ctx.sock.send_to(&wire, send_to).await.is_ok() {
            ctx.st.touch_retry_sent(key, item.pkt, now);
            ctx.st
                .diag(&format!("-> {key} 在线消息重发 pkt={}（第 {} 次）", item.pkt, item.attempts));
        }
    }
}

/// 出站加密的明文输入：完整扩展部 + 恰好一个尾部 \0。
/// 官方加密报文的密文对象是含 \0 的完整明文（对端解密后由 crypto::OpenMsg.plain
/// 剥掉这个尾部 \0），而本端明文扩展部本身不带尾部 \0。
fn plain_payload(extra: &[u8]) -> Vec<u8> {
    let mut p = extra.to_vec();
    p.push(0);
    p
}

/// 把某会话的待投递离线消息重发给对方（对方已在线时）。
/// 不删除队列：以对端 RECVMSG 送达确认为准（见 ack_pending），
/// 否则对方离线期间的重发会静默丢包。
async fn flush_pending_for(ctx: &NetCtx, key: &str) {
    // 守卫只在块内存活，绝不跨 await
    let peer = {
        let peers = ctx.st.peers.lock().unwrap();
        match peers.get(key) {
            Some(p) => p.clone(),
            None => return,
        }
    };
    let Some(target) = peer_addr(&peer) else {
        return;
    };
    let cfg = ctx.st.config();
    let utf8 = proto::is_utf8_mode(&cfg.encoding);
    for item in ctx.st.pending_for(key) {
        // 附件消息：重投前重新校验文件并登记文件槽（复用直发路径）。
        // 文件已丢失/不可读 → 该条无法投递，出队并记诊断（否则无限重试）。
        let mut entries: Vec<proto::FileEntry> = Vec::new();
        if !item.paths.is_empty() {
            match register_offer_files(ctx, item.pkt, &item.paths, utf8) {
                Ok((es, _)) => entries = es,
                Err(e) => {
                    ctx.st.diag(&format!(
                        "-> {key} 离线附件消息 pkt={} 文件不可用，丢弃：{e}",
                        item.pkt
                    ));
                    ctx.st.ack_pending(key, item.pkt);
                    continue;
                }
            }
        }
        let note = proto::fmt_delayed(item.ts);
        // 官方尾注：对端（含本客户端）据此显示「离线留言 · 原发送时间」
        let body = format!("{}\n----\n(IPMsg Delayed Send: {note} )", item.text);
        let mut plain = proto::encode_out(&body, &cfg.encoding);
        if !entries.is_empty() {
            // 附件段与直发同构：\0 分隔 + \a 连接 + 尾部 \a
            plain.push(0);
            let joined: Vec<String> = entries.iter().map(|e| e.serialize(&cfg.encoding)).collect();
            plain.extend_from_slice(joined.join("\u{7}").as_bytes());
            plain.push(0x07);
        }
        // 重投时套用与直发一致的加密决策：此刻已缓存对方公钥就密封，
        // 否则保守明文。这里不触发握手、绝不阻塞重投——对方上线广播后
        // 预握手通常已完成，后续消息自然恢复加密。密封意外失败（如加注
        // 尾注后超限）同样降级明文并记诊断，保证离线留言最终可达。
        let mut enc = false;
        let wire_extra = match if cfg.encrypt { ctx.st.peer_pubkey(key) } else { None } {
            Some(pubk) => {
                match crypto::seal_message(&pubk, &ctx.st.own_keypair(), &plain_payload(&plain)) {
                    Ok(sealed) => {
                        enc = true;
                        sealed.into_bytes()
                    }
                    Err(e) => {
                        ctx.st.diag(&format!(
                            "-> {key} 离线重投加密失败（pkt={}），降级明文：{e}",
                            item.pkt
                        ));
                        plain
                    }
                }
            }
            None => plain,
        };
        let command = cmd::SENDMSG
            | opt::SENDCHECKOPT
            | if entries.is_empty() { opt::READCHECKOPT } else { 0 }
            | if entries.is_empty() { 0 } else { opt::FILEATTACHOPT }
            | if utf8 { opt::UTF8OPT } else { 0 }
            // 加密附件公告需带 ENCEXTMSGOPT，与直发保持一致
            | if enc && !entries.is_empty() { opt::ENCEXTMSGOPT } else { 0 }
            | if enc { opt::ENCRYPTOPT } else { 0 };
        let pkt = proto::Packet::new(command).with_pkt_no(item.pkt);
        let pkt = proto::Packet {
            extra: wire_extra,
            ..pkt
        };
        let bytes = pkt.encode(&my_user(&cfg), &my_host());
        if ctx.sock.send_to(&bytes, target).await.is_ok() {
            ctx.st.diag(&format!(
                "-> {key} 离线消息重投 pkt={}（{} 附件项，待 RECVMSG 确认）",
                item.pkt,
                entries.len()
            ));
        }
    }
}

/// 构造上线类报文（BR_ENTRY/BR_ABSENCE 共用）：
/// - 附加数据走官方 §3-9：UTF-8 模式下昵称/群组放传统字段 + `\0\nUN:/HN:/NN:/GN:/VS:`
///   扩展行；**不置 UTF8OPT**（官方规范：BR 系报文禁用该位，官方客户端按扩展行取
///   UTF-8 名字；GBK 模式下保持纯本地码页字节、不加扩展行）
/// - ABSENCEOPT 随不在模式开关
fn build_entry_packet(cfg: &Config, cmd: u32) -> proto::Packet {
    proto::Packet {
        extra: proto::build_entry_extra_ex(
            &cfg.nickname,
            &cfg.group,
            &my_user(cfg),
            &my_host(),
            &cfg.encoding,
        ),
        command: cmd
            | opt::CAPUTF8OPT
            | if cfg.absence_enabled { opt::ABSENCEOPT } else { 0 }
            | entry_caps(cfg),
        ..proto::Packet::new(0)
    }
}

/// 向所有广播地址发送上线/下线通告
pub async fn announce(ctx: &NetCtx) {
    let cfg = ctx.st.config();
    let pkt = build_entry_packet(&cfg, cmd::BR_ENTRY);
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    let targets: Vec<SocketAddr> = broadcast_targets()
        .into_iter()
        .map(|ip| SocketAddr::from((ip, ctx.port)))
        .collect();
    for t in targets {
        let _ = ctx.sock.send_to(&bytes, t).await;
    }
    // IPv6 组播通道（ff15::979 站点组播 + ff02::1 链路组播，逐接口带 scope）
    let cfg_v6ok = ctx.st.config().v6_mcast;
    let v6 = ctx.v6_sock.lock().await;
    if cfg_v6ok {
    if let Some(s) = v6.as_ref() {
        for (scope, _) in v6_ifaces() {
            let _ = s
                .send_to(&bytes, SocketAddr::V6(SocketAddrV6::new(IPV6_MCAST_SITE, ctx.port, 0, scope)))
                .await;
            let _ = s
                .send_to(&bytes, SocketAddr::V6(SocketAddrV6::new(IPV6_MCAST_LINK, ctx.port, 0, scope)))
                .await;
        }
    }
    }
}

/// 向指定地址单播上线通告（自检/定向刷新用）
pub async fn announce_unicast(ctx: &NetCtx, addrs: &[SocketAddr]) {
    let cfg = ctx.st.config();
    let pkt = build_entry_packet(&cfg, cmd::BR_ENTRY);
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    for a in addrs {
        let _ = ctx.sock.send_to(&bytes, *a).await;
    }
}

/// 不在模式开关广播（官方 MENU_ABSENCE 语义：BR_ABSENCE + ABSENCEOPT，
/// 接收方不应答 ANSENTRY；能力位/扩展行与 BR_ENTRY 同构）
pub async fn announce_absence(ctx: &NetCtx) {
    let cfg = ctx.st.config();
    let pkt = build_entry_packet(&cfg, cmd::BR_ABSENCE);
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    let mut targets: Vec<SocketAddr> = broadcast_targets()
        .into_iter()
        .map(|ip| SocketAddr::from((ip, ctx.port)))
        .collect();
    {
        let peers = ctx.st.peers.lock().unwrap();
        targets.extend(peers.values().filter_map(peer_addr));
    } // 守卫在此释放，之后才 await
    for t in targets {
        let _ = ctx.sock.send_to(&bytes, t).await;
    }
    ctx.st
        .diag(&format!("-> 广播 BR_ABSENCE（不在模式 {}）", cfg.absence_enabled));
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
        let targets: Vec<SocketAddr> = st
            .peers
            .lock()
            .map(|p| p.values().filter_map(peer_addr).collect())
            .unwrap_or_default();
        for addr in targets {
            let _ = sock.send_to(&bytes, addr);
        }
    }
}

/// 对端投递地址：IP 身份 + 最近一次报文的源端口。
/// 端口不能假定是对端监听的固定端口 —— NAT 改写/临时端口发包时
/// 只有最新报文里的源端口保证可达。
fn peer_addr(peer: &PeerInfo) -> Option<SocketAddr> {
    Some(SocketAddr::from((
        peer.ip.parse::<IpAddr>().ok()?,
        peer.port,
    )))
}

/// 发现即预握手（spec §5）：对端上线类报文声明 ENCRYPTOPT、我方开关开启、
/// 尚未缓存对方公钥且对方未被标明文 → 立刻索取公钥，避免首条消息明文发送。
async fn maybe_start_handshake(
    ctx: &NetCtx,
    from: SocketAddr,
    pkt: &proto::Packet,
    key: &str,
    cfg: &Config,
) {
    if !cfg.encrypt
        || pkt.command & opt::ENCRYPTOPT == 0
        || ctx.st.peer_pubkey(key).is_some()
        || ctx.st.peer_marked_plain(key)
    {
        return;
    }
    send_getpubkey(ctx, from, cfg).await;
}

/// 能力撤回（与 maybe_start_handshake 互补，spec §7 补充）：对端此前广告过
/// 加密能力、我方已缓存其公钥，如今上线类报文却不再声明 ENCRYPTOPT ——
/// 说明对方已关闭加密。继续持有缓存会让我方向它发送密文（永远解不开），
/// 必须立刻丢弃缓存；之后的出站消息自然回退明文并重新触发握手探测。
fn maybe_withdraw_peer_key(
    ctx: &NetCtx,
    from: SocketAddr,
    pkt: &proto::Packet,
    key: &str,
    cfg: &Config,
) {
    if cfg.encrypt && pkt.command & opt::ENCRYPTOPT == 0 && ctx.st.peer_pubkey(key).is_some() {
        ctx.st.forget_peer_key(key);
        ctx.st.diag(&format!("<- {from} 上线通告未声明 ENCRYPTOPT，撤回该对端公钥缓存"));
    }
}

/// GETPUBKEY 构造与发送（Task 6 口径）：扩展部 = 我方能力位小写 hex
/// （与 entry_caps 一致，另带 CAPA_OUR_SEND 声明我方也会加密）。
/// 发出即返回，不等对方 ANSPUBKEY —— 调用方各自决定是否继续本次明文投递。
/// 每次发出都计入该 IP 的探测预算（会话键即对端 IP，见 handle_datagram 注释）；
/// 预算语义见 AppState::retire_probe_budget。
async fn send_getpubkey(ctx: &NetCtx, target: SocketAddr, cfg: &Config) {
    let n = ctx.st.record_probe(&target.ip().to_string());
    let capa = entry_caps(cfg) | crypto::CAPA_OUR_SEND;
    let mut g = proto::Packet::new(cmd::GETPUBKEY);
    g.extra = format!("{capa:x}").into_bytes();
    let bytes = g.encode(&my_user(cfg), &my_host());
    let _ = ctx.sock.send_to(&bytes, target).await;
    ctx.st
        .diag(&format!("-> {target} GETPUBKEY 预握手 capa={capa:x} 第 {n} 次探测"));
}

/// 密钥自愈：任一端换了密钥对（更新/重装程序后 ipmsg_key.json 重新生成，
/// 而对方缓存了旧公钥、且「有缓存不握手」导致持续用旧钥）时，入站消息
/// 会出现「验签失败（我方缓存旧）」或「会话钥解不开（对方持我方旧钥）」。
/// 此处双向刷新：GETPUBKEY 索取对方最新公钥更新我方缓存；主动 ANSPUBKEY
/// 把我方最新公钥推给对方（官方协议应答报，对端按普通应答缓存，无害）。
/// 限频由 AppState::try_rehandshake_gate（10s/IP）兜底。
async fn crypto_rehandshake(ctx: &NetCtx, from: SocketAddr, key: &str, reason: &str) {
    if !ctx.st.try_rehandshake_gate(key) {
        return;
    }
    let cfg = ctx.st.config();
    let capa = entry_caps(&cfg) | crypto::CAPA_OUR_SEND;
    // 1) 索取对方最新公钥
    let mut g = proto::Packet::new(cmd::GETPUBKEY);
    g.extra = format!("{capa:x}").into_bytes();
    let _ = ctx
        .sock
        .send_to(&g.encode(&my_user(&cfg), &my_host()), from)
        .await;
    // 2) 主动推送我方最新公钥（对方换钥后其缓存里可能还是我的旧钥）
    let mut a = proto::Packet::new(cmd::ANSPUBKEY);
    a.extra = crypto::build_anspubkey(capa, &ctx.st.own_keypair()).into_bytes();
    let _ = ctx
        .sock
        .send_to(&a.encode(&my_user(&cfg), &my_host()), from)
        .await;
    ctx.st
        .diag(&format!("-> {from} 密钥自愈重握手（{reason}）：GETPUBKEY + ANSPUBKEY"));
}

/* ================= 入站处理 ================= */

fn parse_ipdict_datagram(
    data: &[u8],
) -> Result<Option<(crate::ipdict::Dict, usize)>, String> {
    if !data.starts_with(crate::ipdict::IPDICT_HEAD.as_bytes()) {
        return Ok(None);
    }
    let (dict, used) = crate::ipdict::Dict::unpack(data)
        .ok_or_else(|| "IP2 外壳或内容长度无效".to_string())?;
    let suffix = &data[used..];
    if suffix.is_empty() {
        return Ok(Some((dict, 0)));
    }
    if suffix.len() == 64 && suffix.iter().all(|byte| *byte == 0) {
        return Ok(Some((dict, 64)));
    }
    Err(format!("IP2 非法尾随数据：{}B", suffix.len()))
}

fn resolve_ipdict_files(d: &crate::ipdict::Dict) -> Result<Vec<proto::FileEntry>, String> {
    let files = match d.try_get_dict_list(ipd::DICT_FILE)? {
        None => return Ok(Vec::new()),
        Some(files) if files.is_empty() => return Err("FILE 列表为空".into()),
        Some(files) => files,
    };
    files
        .into_iter()
        .map(|file| {
            let id = u32::try_from(file.get_int(ipd::DICT_FID).ok_or("FILE 缺 FI")?)
                .map_err(|_| "FILE FI 超出 u32")?;
            let name = file
                .get_str(ipd::DICT_FNAME)
                .ok_or("FILE 缺 FN")?
                .to_string();
            let size = file
                .get_int(ipd::DICT_FSIZE)
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0);
            let mtime = file
                .get_int(ipd::DICT_MTIME)
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0);
            let attr = file
                .get_int(ipd::DICT_FATTR)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(1);
            let mut ext_attrs = Vec::new();
            if let Some(clippos) = file
                .get_int(ipd::DICT_CLIPPOS)
                .and_then(|value| u32::try_from(value).ok())
            {
                ext_attrs.push((proto::extattr::CLIPBOARDPOS, clippos.to_string()));
            }
            Ok(proto::FileEntry {
                id,
                raw_id: format!("{id:x}"),
                name,
                size,
                mtime,
                attr,
                ext_attrs,
            })
        })
        .collect()
}

fn resolve_ipdict_packet(d: &crate::ipdict::Dict) -> Result<proto::Packet, String> {
    if d.get_int(ipd::DICT_VER) != Some(3) {
        return Err("VER 不是 IPMSG_NEW_VERSION(3)".into());
    }
    let pkt_no = u32::try_from(d.get_int(ipd::DICT_PKT).ok_or("缺 PKT")?)
        .map_err(|_| "PKT 超出 u32")?;
    let user = d.get_str(ipd::DICT_UID).ok_or("缺 UID")?.to_string();
    let host = d.get_str(ipd::DICT_HID).ok_or("缺 HID")?.to_string();
    let mode = u32::try_from(d.get_int(ipd::DICT_CMD).ok_or("缺 CMD")?)
        .map_err(|_| "CMD 超出 u32")?;
    let flags = u32::try_from(d.get_int(ipd::DICT_FLG).ok_or("缺 FLG")?)
        .map_err(|_| "FLG 超出 u32")?;
    let body = d.get_str(ipd::DICT_BODY).unwrap_or("");
    let mut extra = body.as_bytes().to_vec();

    let files = resolve_ipdict_files(d)?;
    if !files.is_empty() {
        extra.push(0);
        let encoded: Vec<String> = files.iter().map(|file| file.serialize("utf8")).collect();
        extra.extend_from_slice(encoded.join("\u{7}").as_bytes());
        extra.push(0x07);
    }
    Ok(proto::Packet {
        pkt_no,
        user,
        host,
        command: mode | flags,
        extra,
    })
}

async fn handle_datagram(ctx: &NetCtx, data: &[u8], from: SocketAddr) {
    // 自身回声先过滤（IPDict 与经典解析共用同一口径）
    if from.port() == ctx.port && is_self_ip(ctx, from.ip()) {
        return;
    }
    match parse_ipdict_datagram(data) {
        Ok(Some((dict, padding))) => {
            ctx.st.diag(&format!(
                "<- {from} IP2 stage=datagram len={} padding={}B keys={} pkt=? command=?",
                data.len(),
                padding,
                dict.items.len()
            ));
            if dict.has(ipd::DICT_ENCBODY) {
                handle_encipdict(ctx, &dict, from).await;
            } else {
                handle_dict_datagram(ctx, &dict, from).await;
            }
            return;
        }
        Err(error) => {
            ctx.st.diag(&format!(
                "<- {from} IP2 stage=datagram len={} keys=? pkt=? command=? 拒绝：{error}",
                data.len()
            ));
            return;
        }
        Ok(None) => {}
    }
    let Some(pkt) = proto::parse(data) else {
        return;
    };
    #[cfg(feature = "net_debug")]
    oim_log!("[udp] <- {from} cmd={:#010x} user={:?} extra_len={}", pkt.command, pkt.user, pkt.extra.len());
    // 基本命令取低 8 位（官方规范：所有选项标志位于 bit8 以上）
    let base = pkt.command & 0xFF;
    // 会话身份 = 对端 IP（同 IP 不同源端口是同一台主机，见 upsert_peer 注释）
    let key = from.ip().to_string();

    // 送达确认：带 SENDCHECKOPT 的消息必须立刻回 RECVMSG，否则发送方会认为
    // 没送到，把消息留在待发队列里，每次我方上线就重投一遍（离线留言反复出现
    // 就是这么来的）。这一步要在重复包过滤之前做 —— 对端正是因为没收到确认
    // 才重发的，重发包更需要回确认。
    if base == cmd::SENDMSG && pkt.command & opt::SENDCHECKOPT != 0 {
        let n = ctx.st.bump_ack(from.ip(), pkt.pkt_no);
        // 包编号的书写进制各实现不一：首次按十进制（协议头部就是十进制），
        // 对端若不认会重发，届时改用十六进制再确认一次
        let body = if n % 2 == 0 {
            pkt.pkt_no.to_string()
        } else {
            format!("{:x}", pkt.pkt_no)
        };
        let cfg = ctx.st.config();
        let mut r = proto::Packet::new(cmd::RECVMSG | opt::AUTORETOPT);
        r.extra = body.clone().into_bytes();
        let bytes = r.encode(&my_user(&cfg), &my_host());
        // 中继会话走代理回包，普通会话直发
        let _ = reply_send(ctx, &key, from, &bytes).await;
        ctx.st
            .diag(&format!("-> {from} RECVMSG 送达确认 pkt={body}（第 {} 次）", n + 1));
    }

    if !ctx.st.mark_seen(from.ip(), pkt.pkt_no) {
        return; // 重复包
    }

    // 线路诊断：记录入站报文摘要（含原始附加数据十六进制，便于定位互通格式差异）
    {
        use std::fmt::Write as _;
        let raw = &pkt.extra[..pkt.extra.len().min(1600)];
        let mut hexs = String::with_capacity(raw.len() * 3);
        for b in raw {
            let _ = write!(hexs, "{b:02x} ");
        }
        ctx.st.diag(&format!(
            "<- {from} cmd={:#010x} len={} user={:?} host={:?} extra[0..{}]={hexs}",
            pkt.command,
            data.len(),
            pkt.user,
            pkt.host,
            raw.len()
        ));
    }

    match base {
        cmd::BR_ENTRY => {
            let info = proto::parse_entry_extra(&pkt.extra, pkt.command);
            let added = ctx.st.upsert_peer(PeerInfo {
                key: key.clone(),
                ip: from.ip().to_string(),
                port: from.port(),
                nickname: info.nick,
                group: info.group,
                host: pkt.host.clone(),
                user: pkt.user.clone(),
                last_seen: now_secs(),
                absence: pkt.command & opt::ABSENCEOPT != 0,
                absence_text: None,
                vs: info.vs,
            });
            // 回应 ANSENTRY（单播），携带自己的昵称\0群组；能力位随总开关一起广告。
            // BR 应答遵循官方：不带 UTF8OPT（BR 系禁用），UTF-8 走 \0\nNN:/GN: 扩展
            let cfg = ctx.st.config();
            let ans = proto::Packet {
                extra: proto::build_entry_extra_ex(
                    &cfg.nickname,
                    &cfg.group,
                    &my_user(&cfg),
                    &my_host(),
                    &cfg.encoding,
                ),
                command: cmd::ANSENTRY
                    | opt::CAPUTF8OPT
                    // BR 系报文规范禁用 UTF8OPT（官方 HostStatus 同款），
                    // UTF-8 名字走上面 build_entry_extra_ex 的 \nNN:/GN: 扩展
                    | entry_caps(&cfg),
                ..proto::Packet::new(0)
            };
            let bytes = ans.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
            if added {
                ctx.st.emit("users-updated", json!({}));
            }
            maybe_start_handshake(ctx, from, &pkt, &key, &cfg).await;
            maybe_withdraw_peer_key(ctx, from, &pkt, &key, &cfg);
            // 对方刚上线（BR_ENTRY）：立即重投此前的离线消息
            flush_pending_for(ctx, &key).await;
            // 代理窗口内：把本段 entry 事件转发给 master（DIR_EVBROAD）
            maybe_forward_entry_event(ctx, &pkt, &key, base).await;
        }
        cmd::ANSENTRY | cmd::BR_ABSENCE => {
            let info = proto::parse_entry_extra(&pkt.extra, pkt.command);
            let absence = pkt.command & opt::ABSENCEOPT != 0;
            ctx.st.upsert_peer(PeerInfo {
                key: key.clone(),
                ip: from.ip().to_string(),
                port: from.port(),
                nickname: info.nick,
                group: info.group,
                host: pkt.host.clone(),
                user: pkt.user.clone(),
                last_seen: now_secs(),
                absence,
                absence_text: None,
                vs: info.vs,
            });
            ctx.st.emit("users-updated", json!({}));
            // 不在模式通告（BR_ABSENCE）说明对方状态切换：旧缓存的不在通知文作废
            if base == cmd::BR_ABSENCE {
                ctx.st.clear_peer_absence(&key);
            }
            // 对端应答里若声明加密能力，同样触发预握手（对方可能没收到我们的 BR_ENTRY）
            let cfg = ctx.st.config();
            maybe_start_handshake(ctx, from, &pkt, &key, &cfg).await;
            // 撤回只认 ANSENTRY：第三方客户端的下线通告（BR_ABSENCE）常不带
            // 能力位，若据此撤回会把仍在线、支持加密的对端公钥误丢 ——
            // 虽然下次上线会自愈，但表现为反复横跳的加解密抖动。
            if base == cmd::ANSENTRY {
                maybe_withdraw_peer_key(ctx, from, &pkt, &key, &cfg);
            }
            // ANSENTRY 通常是对我们上线通告的应答：对方在线，重投离线消息
            flush_pending_for(ctx, &key).await;
            maybe_forward_entry_event(ctx, &pkt, &key, base).await;
        }
        cmd::BR_EXIT => {
            // 退出广播可能来自临时端口（进程收尾时无法复用主 socket），
            // 因此按 IP 清理该主机的所有会话条目，而不是只按 ip:port 精确匹配
            let ip = from.ip().to_string();
            let removed: Vec<String> = {
                let mut peers = ctx.st.peers.lock().unwrap();
                let hit: Vec<String> = peers
                    .values()
                    .filter(|p| p.ip == ip)
                    .map(|p| p.key.clone())
                    .collect();
                for k in &hit {
                    peers.remove(k);
                    // 未确认的在线消息转入待投递（对方回来补投）
                    ctx.st.demote_retry_to_pending(k);
                    ctx.st.clear_peer_absence(k);
                }
                hit
            };
            if !removed.is_empty() {
                ctx.st.emit("users-updated", json!({}));
            }
            maybe_forward_entry_event(ctx, &pkt, &key, base).await;
        }
        cmd::RECVMSG => {
            // 对方确认送达（我们发 SENDCHECKOPT 时对方回 RECVMSG，附加数据是包号）：
            // 把对应的待投递离线消息出队，避免对方在线时每 45s 无限重投；
            // 同时移除在线重发队列项（官方 §4-12 確認・リトライ闭环）
            let first = pkt.extra.split(|&b| b == b':').next().unwrap_or(b"");
            let no = String::from_utf8_lossy(first).trim().to_string();
            if let Some(k) = resolve_session_key(ctx, from.ip()) {
                for c in id_candidates(&no) {
                    let acked_offline = ctx.st.ack_pending(&k, c);
                    let acked_retry = ctx.st.ack_retry(&k, c);
                    if acked_offline || acked_retry {
                        ctx.st
                            .diag(&format!("<- {from} RECVMSG 送达确认 pkt={c}（离线:{acked_offline} 重发:{acked_retry}）已送达"));
                    }
                }
            }
        }
        cmd::SENDMSG => {
            // 入站加密消息拦截：在进入普通明文处理路径前还原报文。
            // 只拦 base==SENDMSG 且带 ENCRYPTOPT 的包；其余报文零开销直通。
            let mut pkt = pkt; // 解密后要改写 command/extra，取得所有权
            let mut enc_meta: Option<bool> = None; // Some(sig_ok)：该消息曾加密
            if pkt.command & opt::ENCRYPTOPT != 0 {
                let cfg = ctx.st.config();
                if cfg.encrypt {
                    let peer_pubs = ctx.st.peer_pubkeys(&key);
                    match crypto::open_message(
                        &ctx.st.own_keypair(),
                        &String::from_utf8_lossy(&pkt.extra),
                        &peer_pubs,
                        pkt.pkt_no,
                    ) {
                        Ok(m) => {
                            // 解包明文已剥掉密封时附加的尾部 \0（crypto::OpenMsg.plain），
                            // 直接换回原扩展部并清掉 ENCRYPTOPT，重构出等价明文报文；
                            // 其余命令标志（READCHECKOPT 等）原样保留，下游按普通
                            // 明文路径处理。
                            let capa_head = String::from_utf8_lossy(&pkt.extra)
                                .split(':')
                                .next()
                                .unwrap_or("")
                                .to_string();
                            let head = String::from_utf8_lossy(
                                &m.plain[..m.plain.len().min(16)],
                            )
                            .into_owned();
                            ctx.st.diag(&format!(
                                "dec-msg {from} pkt={} capa={capa_head} len={} sig={} head={head:?}",
                                pkt.pkt_no,
                                m.plain.len(),
                                m.sig_ok
                            ));
                            // 验签失败但解密成功：最常见于对端换过密钥而缓存仍是旧钥
                            // （「有缓存不握手」从不刷新）。触发双向自愈重握手。
                            if !m.sig_ok && !peer_pubs.is_empty() {
                                crypto_rehandshake(ctx, from, &key, "sig=false").await;
                            }
                            pkt.extra = m.plain;
                            pkt.command &= !opt::ENCRYPTOPT;
                            enc_meta = Some(m.sig_ok);
                        }
                        Err(e) => {
                            // 无法解密的报文按垃圾丢弃（对端会重发或回退明文）；
                            // 会话钥解不开 = 对方持我方的旧公钥签发——主动推送新钥
                            // 并索取对方新钥（双向自愈）
                            ctx.st.diag(&format!("decrypt-fail {from}: {e}"));
                            crypto_rehandshake(ctx, from, &key, "decrypt-fail").await;
                            return;
                        }
                    }
                } else {
                    // 加密关闭但收到密文：以占位文本入会话，避免静默丢消息；
                    // 清掉 ENCRYPTOPT 让它走普通明文路径。sig 无法核验 → Some(false)
                    // 占位文案按界面语言（config.lang）取简中/英文
                    let lang = ctx.st.config().lang;
                    pkt.extra = if lang.eq_ignore_ascii_case("en") {
                        "🔒 Cannot decrypt (encryption is off)".as_bytes().to_vec()
                    } else {
                        "🔒 无法解密（加密已关闭）".as_bytes().to_vec()
                    };
                    pkt.command &= !opt::ENCRYPTOPT;
                    enc_meta = Some(false);
                }
            }
            handle_sendmsg(ctx, from, &pkt, &key, enc_meta).await;
        }
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
            oim_log!("[read] READMSG from {from} no={no:?}");
            if let Some(no) = no {
                if let Some(key) = resolve_session_key(ctx, from.ip()) {
                    let changed = ctx.st.mark_out_read(&key, no);
                    #[cfg(feature = "net_debug")]
                    oim_log!("[read] key={key} changed={changed}");
                    if changed {
                        ctx.st.emit("msg-read", json!({"key": key, "pkt": no}));
                    }
                }
            }
        }
        cmd::DELMSG => {
            // 封书破弃/消息撤回通知（官方语义：recvdlg 的「破弃」按钮；
            // 本客户端扩展：对方撤回我方会话里的消息）
            let no = String::from_utf8_lossy(&pkt.extra)
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u32>();
            if let Some(no) = no.ok() {
                let k = match resolve_session_key(ctx, from.ip()) {
                    Some(k) => k,
                    None => from.ip().to_string(), // 离线会话也可能被撤回
                };
                if ctx.st.update_history_pkt(&k, no, |rec| {
                    rec["recalled"] = json!(true);
                }) {
                    ctx.st.emit(
                        "msg-recalled",
                        json!({"key": k, "pkt": no, "peer": pkt.user}),
                    );
                    ctx.st.diag(&format!("<- {from} DELMSG 撤回 pkt={no}（会话 {k}）"));
                }
            }
        }
        cmd::ANSREADMSG => {
            // READMSG 带 READCHECKOPT 时的确认（8 版协议）：我方出站消息视为已读
            let no = String::from_utf8_lossy(&pkt.extra)
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u32>()
                .ok();
            if let Some(no) = no {
                if let Some(k) = resolve_session_key(ctx, from.ip()) {
                    if ctx.st.mark_out_read(&k, no) {
                        ctx.st.emit("msg-read", json!({"key": k, "pkt": no}));
                    }
                }
            }
        }
        cmd::GETABSENCEINFO => {
            // 官方 §3-11：不在模式成员返回不在通知文；非不在模式回占位串
            let cfg = ctx.st.config();
            let text = if cfg.absence_enabled && !cfg.absence_text.trim().is_empty() {
                cfg.absence_text.clone()
            } else {
                "Not absence mode".to_string()
            };
            let utf8 = proto::is_utf8_mode(&cfg.encoding);
            let mut r = proto::Packet::new(cmd::SENDABSENCEINFO | opt::AUTORETOPT | if utf8 { opt::UTF8OPT } else { 0 });
            r.extra = proto::encode_out(&text, &cfg.encoding);
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
        }
        cmd::SENDABSENCEINFO => {
            // 对端的不在通知文：缓存到会话，供前端展示
            let text = proto::text_of(&pkt);
            ctx.st.set_peer_absence(&key, &text);
            ctx.st.emit("absence-info", json!({"key": key, "text": text}));
        }
        cmd::BR_ISGETLIST => {
            // 主机列表能力探索：我方允许就回 OKGETLIST（官方 MsgBrIsGetList 同款）
            let cfg = ctx.st.config();
            if cfg.allow_send_list {
                let r = proto::Packet::new(cmd::OKGETLIST);
                let bytes = r.encode(&my_user(&cfg), &my_host());
                let _ = ctx.sock.send_to(&bytes, from).await;
            }
        }
        cmd::OKGETLIST => {
            // 对方可回主机列表：窗口期（启动/手动刷新后）内发起 GETLIST
            if ctx.st.hostlist_window_open() {
                let cfg = ctx.st.config();
                let mut r = proto::Packet::new(cmd::GETLIST);
                r.extra = b"0".to_vec(); // 起始索引
                let bytes = r.encode(&my_user(&cfg), &my_host());
                let _ = ctx.sock.send_to(&bytes, from).await;
            }
        }
        cmd::GETLIST => {
            // 对方请求主机列表：官方 ANSLIST 结构（分页续传）
            let cfg = ctx.st.config();
            if !cfg.allow_send_list {
                return;
            }
            let start = String::from_utf8_lossy(&pkt.extra)
                .trim()
                .parse::<usize>()
                .unwrap_or(0);
            let hosts: Vec<proto::HostListEntry> = {
                let peers = ctx.st.peers.lock().unwrap();
                peers
                    .values()
                    .map(|p| proto::HostListEntry {
                        user: p.user.clone(),
                        host: p.host.clone(),
                        status: cmd::BR_ENTRY | if p.absence { opt::ABSENCEOPT } else { 0 },
                        ip: p.ip.clone(),
                        port: p.port,
                        nick: p.nickname.clone(),
                        group: p.group.clone(),
                    })
                    .collect()
            };
            let (wire, _n) = proto::build_anslist(&hosts, start, 4000, &cfg.encoding);
            let utf8 = proto::is_utf8_mode(&cfg.encoding);
            let mut r = proto::Packet::new(cmd::ANSLIST | opt::AUTORETOPT | if utf8 { opt::UTF8OPT } else { 0 });
            r.extra = wire;
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
        }
        cmd::ANSLIST => {
            // 主机列表回包：并入用户表；续传索引非 0 则继续取下一批
            let (cont, hosts) = proto::parse_anslist(&pkt.extra, pkt.command);
            let mut changed = false;
            for h in hosts {
                let ip = match h.ip.parse::<IpAddr>() {
                    Ok(ip) => ip,
                    Err(_) => continue,
                };
                if h.status & 0xFF == cmd::BR_EXIT {
                    let mut peers = ctx.st.peers.lock().unwrap();
                    if peers.remove(ip.to_string().as_str()).is_some() {
                        changed = true;
                    }
                    continue;
                }
                let add = ctx.st.upsert_peer(PeerInfo {
                    key: ip.to_string(),
                    ip: ip.to_string(),
                    port: if h.port == 0 { 2425 } else { h.port },
                    nickname: if h.nick.is_empty() { h.user.clone() } else { h.nick.clone() },
                    group: h.group.clone(),
                    host: h.host.clone(),
                    user: h.user.clone(),
                    last_seen: now_secs(),
                    absence: h.status & opt::ABSENCEOPT != 0,
                    absence_text: None,
                    vs: None,
                });
                changed |= add;
            }
            if changed {
                ctx.st.emit("users-updated", json!({}));
            }
            if cont != 0 {
                let cfg = ctx.st.config();
                let mut r = proto::Packet::new(cmd::GETLIST);
                r.extra = cont.to_string().into_bytes();
                let bytes = r.encode(&my_user(&cfg), &my_host());
                let _ = ctx.sock.send_to(&bytes, from).await;
            }
        }
        cmd::ANSLIST_DICT => {
            // IPDict 版主机列表（v5）：纯 IPDict 报文，实际走 handle_dict_datagram；
            // 经典报文路径出现该命令时按普通 ANSLIST 语义忽略
            let _ = &pkt;
        }
        cmd::AGENT_REQ | cmd::AGENT_PROXYREQ => {
            // 代理侧：有人在找通向某目标的路由 —— 我们能否送到？
            let targ = String::from_utf8_lossy(&pkt.extra)
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let reachable = targ
                .parse::<IpAddr>()
                .map(|ip| {
                    ctx.st.peers.lock().unwrap().contains_key(&ip.to_string())
                        || local_ip_set().contains(&ip)
                })
                .unwrap_or(false);
            let cfg = ctx.st.config();
            let mut r = proto::Packet::new(cmd::AGENT_ANSREQ);
            r.extra = format!("{targ}:{}", if reachable { 1 } else { 0 }).into_bytes();
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
            ctx.st
                .diag(&format!("<- {from} AGENT_REQ {targ} → {}", if reachable { "可达" } else { "不可达" }));
        }
        cmd::AGENT_ANSREQ => {
            // 代理能力应答：仅记录
            ctx.st.diag(&format!(
                "<- {from} AGENT_ANSREQ {}",
                String::from_utf8_lossy(&pkt.extra)
            ));
        }
        cmd::AGENT_PACKET => {
            handle_agent_packet(ctx, &pkt, from).await;
        }
        cmd::GETINFO => {
            let cfg = ctx.st.config();
            let utf8 = proto::is_utf8_mode(&cfg.encoding);
            let reply = proto::Packet::new(cmd::SENDINFO | opt::AUTORETOPT | if utf8 { opt::UTF8OPT } else { 0 });
            let mut r = reply;
            r.extra = proto::encode_out(concat!("OpenIPMsg v", env!("CARGO_PKG_VERSION")), &cfg.encoding);
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
        }
        cmd::GETPUBKEY => {
            // 公钥握手请求（spec §5）：我方开关关闭时不广告、不应答，对端会回退明文
            let cfg = ctx.st.config();
            if cfg.encrypt {
                let capa = entry_caps(&cfg) | crypto::CAPA_OUR_SEND;
                let mut r = proto::Packet::new(cmd::ANSPUBKEY);
                r.extra = crypto::build_anspubkey(capa, &ctx.st.own_keypair()).into_bytes();
                let bytes = r.encode(&my_user(&cfg), &my_host());
                let _ = ctx.sock.send_to(&bytes, from).await;
                ctx.st.diag(&format!("-> {from} ANSPUBKEY capa={capa:X}"));
            } else {
                ctx.st.diag(&format!("<- {from} GETPUBKEY 忽略：本机加密已关闭"));
            }
        }
        cmd::ANSPUBKEY => {
            // 对端公钥应答：宽容解析后缓存能力位与公钥（持久化，重启免握手）
            // 「解析失败」时附上长度与冒号/连字符位置，便于定位官方 revendian
            // 或非标长度模数等互通格式差异（2026-08 现场排查用）
            match crypto::parse_anspubkey(&String::from_utf8_lossy(&pkt.extra)) {
                Some((capa, pubk)) => {
                    ctx.st.remember_peer_key(&key, capa, &pubk);
                    ctx.st.diag(&format!("<- {from} ANSPUBKEY 已缓存 capa={capa:X}"));
                }
                None => {
                    let s = String::from_utf8_lossy(&pkt.extra);
                    let colon = s.find(':').map(|i| i.to_string()).unwrap_or("-".into());
                    let dash = s.find('-').map(|i| i.to_string()).unwrap_or("-".into());
                    ctx.st.diag(&format!(
                        "<- {from} ANSPUBKEY 解析失败，忽略 len={} colon@{colon} dash@{dash} head={}",
                        s.len(),
                        s.chars().take(24).collect::<String>()
                    ));
                }
            }
        }
        cmd::RELEASEFILES => {
            // 对端放弃接收：释放对应的文件槽。包编号的进制约定各家不一，两种都试
            let first = pkt.extra.split(|&b| b == b':').next().unwrap_or(b"");
            let cands = id_candidates(&String::from_utf8_lossy(first));
            if !cands.is_empty() {
                ctx.st
                    .offered
                    .lock()
                    .unwrap()
                    .retain(|(p, _), _| !cands.contains(p));
            }
        }
        _ => {}
    }
}

async fn handle_sendmsg(
    ctx: &NetCtx,
    from: SocketAddr,
    pkt: &proto::Packet,
    key: &str,
    enc_meta: Option<bool>,
) {
    // 广播群发消息（BROADCASTOPT）：统一进「广播」会话（key=__broadcast__），
    // 不回任何确认、不触发不在自动应答（官方 MsgSendMsg 同款门控）
    let is_broadcast = pkt.command & opt::BROADCASTOPT != 0;
    let session_key: String = if is_broadcast {
        "255.255.255.255".to_string()
    } else {
        key.to_string()
    };

    // 不在模式自动应答（官方 MsgSendMsg 同款）：开启且对方非自动/广播报文时，
    // 以 AUTORETOPT 回不在通知文（自动应答不回自动应答，防乒乓）
    if !is_broadcast && pkt.command & opt::AUTORETOPT == 0 {
        let cfg = ctx.st.config();
        if cfg.absence_enabled && !cfg.absence_text.trim().is_empty() {
            let utf8 = proto::is_utf8_mode(&cfg.encoding);
            let mut r = proto::Packet::new(cmd::SENDMSG | opt::AUTORETOPT | if utf8 { opt::UTF8OPT } else { 0 });
            r.extra = proto::encode_out(&cfg.absence_text, &cfg.encoding);
            let bytes = r.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, from).await;
            ctx.st
                .diag(&format!("-> {from} 不在模式自动应答（AUTORETOPT）"));
        }
    }

    let attach = pkt.command & opt::FILEATTACHOPT != 0;
    let files = if attach {
        proto::parse_file_entries(&pkt.extra)
    } else {
        Vec::new()
    };

    // 被用户删除（隐藏）的联系人主动发来消息：视为对方再来联系，
    // 会话自动恢复（微信式删除语义），并让前端刷新列表把它放回来
    if !is_broadcast && ctx.st.is_hidden(key) {
        ctx.st.unhide_contact(key);
        ctx.st.emit("users-updated", json!({}));
    }

    let no_add_list = pkt.command & opt::NOADDLISTOPT != 0;
    if !is_broadcast && !no_add_list && !ctx.st.peers.lock().unwrap().contains_key(key) {
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
            absence: false,
            absence_text: None,
            vs: None,
        });
        if added {
            ctx.st.emit("users-updated", json!({}));
        }
    } else if !is_broadcast {
        ctx.st.touch_peer(key);
    }

    let display = {
        if is_broadcast {
            pkt.user.clone()
        } else {
            let peers = ctx.st.peers.lock().unwrap();
            peers
                .get(key)
                .map(|p| p.nickname.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| pkt.user.clone())
        }
    };

    let kind = if files.is_empty() { "text" } else { "file" };

    // 注意：对端"延迟发送"会以相同包号跨会话永久重发，
    // 不能按包号查历史去重（会把新会话里合法的附件公告误杀）。
    // 前端对连续重复的同包号消息做原地替换，保证既不刷屏也不丢文件。

    // 图片类小文件自动接收（聊天内直接预览）。对端延迟重发会反复投递同一包号，
    // 已有历史记录的文件继承其状态（failed 不自动重试，避免无意义循环；
    // 用户可在卡片上手动点重试），仅首次遇到时触发自动下载。
    let prev_rec = ctx.st.find_in_record(key, pkt.pkt_no);
    let prev_files: Vec<serde_json::Value> = prev_rec
        .as_ref()
        .and_then(|r| r.get("files").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let prev_file = |id: u32| -> Option<serde_json::Value> {
        prev_files
            .iter()
            .find(|f| f.get("id").and_then(|v| v.as_u64()) == Some(id as u64))
            .cloned()
    };

    // 所有附件都要进入消息记录，前端才能展示卡片并手动下载；
    // 其中小图片额外触发自动接收（聊天内直接预览）。
    let mut auto_ids: Vec<u32> = Vec::new();
    let mut file_jsons: Vec<serde_json::Value> = Vec::new();
    for f in files.iter() {
        let is_dir = f.attr & 0xFF == fileattr::DIR;
        let auto = !is_dir && f.size <= 30 * 1024 * 1024 && is_image_name(&f.name);
        let prev = prev_file(f.id);
        let inherited = prev
            .as_ref()
            .and_then(|p| p.get("state"))
            .and_then(|v| v.as_str())
            .map(String::from);
        if auto && matches!(inherited.as_deref(), None | Some("pending")) {
            auto_ids.push(f.id);
        }
        let default_state = if auto {
            "downloading"
        } else {
            "pending"
        };
        let mut obj = json!({
            "id": f.id, "rid": f.raw_id, "name": f.name, "size": f.size,
            "dir_entry": is_dir,
            "state": inherited.unwrap_or_else(|| default_state.into()),
        });
        if let Some(p) = prev.as_ref() {
            if p.get("state").and_then(|v| v.as_str()) == Some("done") {
                if let Some(path) = p.get("path") {
                    obj["path"] = path.clone();
                }
            }
            if let Some(err) = p.get("error") {
                obj["error"] = err.clone();
            }
        }
        file_jsons.push(obj);
    }

    // 对端"延迟发送"会用同一包号反复重投同一条消息：已读状态必须继承，
    // 否则每次重投都被当成新的未读消息，标记已读时又回一次 READMSG，
    // 对端就会反复弹"消息已���查看"
    let already_read = prev_rec
        .as_ref()
        .and_then(|r| r.get("read").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let text_end = pkt.extra.iter().position(|&b| b == 0).unwrap_or(pkt.extra.len());
    let text = proto::decode_for_command(&pkt.extra[..text_end], pkt.command);

    // 封书（SECRETOPT 位即可；SECRETEXOPT = SECRET|READCHECK 亦含此位）与密码锁
    // （PASSWORDOPT，且本机启用密码功能）：
    // 内容对用户隐藏，需「开封/输密码」后才展示；已读回执同样推迟到解锁之后
    let secret = pkt.command & opt::SECRETOPT != 0;
    let cfg_now = ctx.st.config();
    let locked = pkt.command & opt::PASSWORDOPT != 0 && cfg_now.password_use;
    let prev_unlocked = prev_rec
        .as_ref()
        .and_then(|r| r.get("unlocked").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let rec = json!({
        "dir": "in",
        "kind": kind,
        "text": text,
        "files": file_jsons,
        "ts": now_secs(),
        "pkt": pkt.pkt_no,
        "peer": {"key": session_key, "nickname": display, "host": pkt.host},
        // 对端要求已读回执：待用户查看后由 mark_read 发送 READMSG
        "need_read": pkt.command & opt::READCHECKOPT != 0,
        "read": already_read,
        // 曾加密（enc）；签名是否可核验（sig_ok）。未加密消息 sig_ok 恒为 true
        "enc": enc_meta.is_some(),
        "sig_ok": enc_meta.unwrap_or(true),
        // 封书：气泡展示锁定态，开封后置 unlocked 并补发已读回执
        "secret": secret,
        // 密码锁：输入本机密码后置 unlocked
        "locked": locked && !prev_unlocked,
        "unlocked": prev_unlocked,
        "broadcast": is_broadcast,
    });
    // 同包号原地更新，历史不再被重发副本撑爆
    let first_seen = ctx.st.upsert_in_record(&session_key, &rec);
    ctx.st.emit(
        "msg-in",
        json!({"key": session_key, "msg": rec, "resend": !first_seen}),
    );

    // 自动接收图片（广播会话不自动收附件）
    if !is_broadcast {
        for f in files.iter().filter(|f| auto_ids.contains(&f.id)) {
            let st2 = ctx.st.clone();
            let sock2 = ctx.sock.clone();
            let port2 = ctx.port;
            let k2 = key.to_string();
            let name = f.name.clone();
            let rid = f.raw_id.clone();
            let id = f.id;
            let size = f.size;
            let is_dir = false; // 自动接收只针对图片文件，目录一律等用户手动确认
            let pno = pkt.pkt_no;
            tokio::spawn(async move {
                let tmp = NetCtx {
                    st: st2,
                    sock: sock2,
                    v6_sock: tokio::sync::Mutex::new(None),
                    port: port2,
                };
                if let Err(e) =
                    download_file_task(&tmp, &k2, pno, id, &name, &rid, size, is_dir).await
                {
                    oim_log!("[auto-dl] {k2} #{id} {name}: {e}");
                }
            });
        }
    }
}

/* ================= 剪贴板图片 ================= */

/// 把剪贴板图片（base64）落盘到数据目录下的缓存目录，返回可发送的路径。
///
/// 按官方「粘贴图片」的命名约定 `ipmsgclip_s_<id>_<pos>.png` 落盘：
/// 发送时公告带 FILE_CLIPBOARD(0x20)+CLIPBOARDPOS（见 send_clipboard_image），
/// 官方对端据 attr 内嵌显示（share.cpp 同款格式）；本端接收方向本就兼容。
pub fn stage_clipboard_image(
    data_dir: &std::path::Path,
    b64: &str,
    mime: &str,
) -> Result<PathBuf, String> {
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        other => return Err(format!("不支持的图片类型：{other}")),
    };
    let id = FILE_ID_SEQ.fetch_add(1, Ordering::Relaxed).max(1);
    stage_blob(
        data_dir,
        "剪贴板图片",
        &format!("ipmsgclip_s_{id}_0.{ext}"),
        b64,
        32 * 1024 * 1024,
    )
}

/// 把剪贴板里"只有内容没有路径"的文件落盘后再发送。
///
/// Windows 资源管理器复制的文件粘贴进来时，webview 只给得到文件名与内容，
/// 拿不到原始路径，只能先写进缓存目录再按普通附件公告。
pub fn stage_clipboard_file(
    data_dir: &std::path::Path,
    name: &str,
    b64: &str,
) -> Result<PathBuf, String> {
    // 文件名只取最后一段并清洗，杜绝 ../ 之类的路径穿越
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .replace(':', "_");
    let base = base.trim();
    let safe = if base.is_empty() || base.trim_matches('.').is_empty() {
        format!("粘贴文件_{}", clipboard_stamp())
    } else {
        base.to_string()
    };
    stage_blob(data_dir, "剪贴板文件", &safe, b64, 256 * 1024 * 1024)
}

/// 落盘一份 base64 内容到数据目录下的缓存目录，返回可发送的路径
fn stage_blob(
    data_dir: &std::path::Path,
    sub_dir: &str,
    file_name: &str,
    b64: &str,
    max_bytes: usize,
) -> Result<PathBuf, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("剪贴板数据无法解码: {e}"))?;
    if bytes.is_empty() {
        return Err("剪贴板内容为空".into());
    }
    if bytes.len() > max_bytes {
        return Err(format!(
            "内容超过 {}MB，请改用拖放或「发送文件」",
            max_bytes / 1024 / 1024
        ));
    }

    // 发出的内容要一直可读（对端可能延后来取），只清理 7 天前的旧缓存
    let dir = data_dir.join(sub_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;
    prune_clipboard_cache(&dir);

    // 同名/同秒多次粘贴也不能互相覆盖
    let path = unique_path(&dir.join(file_name));
    std::fs::write(&path, &bytes).map_err(|e| format!("写入缓存失败: {e}"))?;
    Ok(path)
}

/// 缓存文件名时间戳 yyyymmdd-hhmmss（UTC，避免引入日期库依赖）
fn clipboard_stamp() -> String {
    let secs = now_secs();
    let tod = secs % 86_400;
    // 民用历法换算（Howard Hinnant 的 civil_from_days）
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// 清理 7 天前的剪贴板缓存图片
fn prune_clipboard_cache(dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(7 * 86_400);
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 常见图片扩展名判断（用于聊天内联预览与自动接收）
fn is_image_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/* ================= 出站消息 ================= */

/// 校验本地路径并生成公告条目（不登记文件槽，id 由调用方分配）。
/// 目录体积递归计算（仅供展示与进度分母）；直发、离线入队共用。
fn stat_file_entries(paths: &[String]) -> Result<Vec<proto::FileEntry>, String> {
    let mut entries = Vec::with_capacity(paths.len());
    for p in paths.iter() {
        let path = PathBuf::from(p);
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("无法读取文件 {}: {e}", path.display()))?;
        let is_dir = meta.is_dir();
        if !meta.is_file() && !is_dir {
            return Err(format!("不支持的文件类型：{}", path.display()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 目录公告体积 = 递归总字节数（仅供对端展示进度，实际以流内容为准）
        let size = if is_dir { dir_total_size(&path) } else { meta.len() };
        entries.push(proto::FileEntry {
            id: 0, // 调用方分配
            raw_id: String::new(),
            name,
            size,
            mtime,
            attr: if is_dir { fileattr::DIR } else { fileattr::REGULAR },
            ext_attrs: vec![],
        });
    }
    Ok(entries)
}

/// 校验路径、分配文件 ID 并登记文件槽（直发与离线重投共用，必须在发包前完成，
/// 对端可能立刻来取）。返回公告条目与已登记槽位（UDP 发送失败时回滚用）。
/// `utf8`：公告是否按 UTF-8 发出（对端取文件请求与目录流文件名的编码依据）。
fn register_offer_files(
    ctx: &NetCtx,
    pkt_no: u32,
    paths: &[String],
    utf8: bool,
) -> Result<(Vec<proto::FileEntry>, Vec<(u32, u32)>), String> {
    let stats = stat_file_entries(paths)?;
    ctx.st.prune_offered();
    let mut entries = Vec::with_capacity(stats.len());
    let mut inserted = Vec::with_capacity(stats.len());
    for (s, p) in stats.iter().zip(paths.iter()) {
        let id = FILE_ID_SEQ.fetch_add(1, Ordering::Relaxed).max(1);
        let mut e = s.clone();
        e.id = id;
        ctx.st.offered.lock().unwrap().insert(
            (pkt_no, id),
            OfferedFile {
                path: PathBuf::from(p),
                size: e.size,
                is_dir: e.attr & 0xFF == fileattr::DIR,
                ts: now_secs(),
                utf8,
            },
        );
        entries.push(e);
        inserted.push((pkt_no, id));
    }
    Ok((entries, inserted))
}

/// 对方离线时组装本地记录并入待投递队列（文本/附件通用）。
/// 附件路径此刻校验存在性；文件槽与公告 ID 在重投时登记分配。
fn offline_enqueue_record(
    ctx: &NetCtx,
    key: &str,
    pkt_no: u32,
    ts: u64,
    text: &str,
    paths: &[String],
) -> Result<serde_json::Value, String> {
    let mut entries = Vec::new();
    if !paths.is_empty() {
        let stats = stat_file_entries(paths)?;
        for mut e in stats {
            e.id = FILE_ID_SEQ.fetch_add(1, Ordering::Relaxed).max(1);
            entries.push(e);
        }
    }
    if ctx.st.enqueue_pending(PendingOut {
        key: key.to_string(),
        pkt: pkt_no,
        text: text.to_string(),
        ts,
        paths: paths.to_vec(),
    }) {
        ctx.st.diag(&format!(
            "-> {key} 离线消息入队 pkt={pkt_no}（附件 {} 项）",
            paths.len()
        ));
    }
    let kind = if entries.is_empty() { "text" } else { "file" };
    let rec = json!({
        "dir": "out",
        "kind": kind,
        "text": text,
        "files": entries.iter().zip(paths.iter()).map(|(e, p)| json!({
            "id": e.id, "name": e.name, "size": e.size, "path": p, "state": "queued",
            "dir_entry": e.attr & 0xFF == fileattr::DIR,
        })).collect::<Vec<_>>(),
        "pkt": pkt_no,
        "ts": ts,
        "peer": {"key": key, "nickname": "", "host": "", "group": ""},
        // 文件消息不请求已读回执（与直发一致）；文本保留回执
        "rcpt": entries.is_empty(),
        "read": false,
        "queued": true,
        // 入队时必然明文暂存；实际是否加密由重投时的 flush_pending_for 决定
        "enc": false, "sig_ok": true,
    });
    ctx.st.log_record(key, &rec);
    Ok(rec)
}

/// 发送文本/附件消息（加密模式长文本自动分段）。
///
/// 密封报文有明文预算上限（3400B，见 crypto::MAX_PLAIN_FOR_SEAL）：单条
/// 消息超出时不再整体失败，而是按字符边界切成多段、逐段独立发送（各自
/// 包号/已读回执/气泡）。
/// - 纯文本：每段 ≤ CHUNK_PLAIN_BUDGET；
/// - 带附件：首段文本预算再扣掉文件条目段（attachment_text_budget），
///   附件只挂在首段，其余段纯文本。
/// 只在「加密开关开启 + 已缓存对方公钥」时切分——此时 `send_message`
/// 必然走密封路径、存在预算约束；其余情况保持单条原样。
/// 返回每条发送记录组成的数组（未分段时长度为 1），调用方（send_text /
/// send_files / send_clipboard_image 命令）原样透传给前端逐条上屏。
pub async fn send_message_multi(
    ctx: &NetCtx,
    key: &str,
    text: &str,
    paths: Vec<String>,
) -> Result<Vec<Value>, String> {
    let cfg = ctx.st.config();
    // 与 send_message 的出站加密决策一致：encrypt 开启且已缓存对端公钥才会密封
    let will_seal = cfg.encrypt && ctx.st.peer_pubkey(key).is_some();
    let parts: Vec<String> = if !will_seal {
        vec![text.to_string()]
    } else if paths.is_empty() {
        split_text_for_seal(text, &cfg.encoding).unwrap_or_else(|| vec![text.to_string()])
    } else {
        // 附件公告：条目段也占密封预算；先校验路径并据条目开销算出文本预算，
        // 再按预算切分（首段带附件）。预算地板 64B：条目过大时退化为
        // 「首段仅 64B 文本 + 附件」，避免无意义的一堆纯文本段。
        let stats = stat_file_entries(&paths)?;
        let budget = attachment_text_budget(&stats, &cfg.encoding).max(64);
        let all = chunk_by_budget(text, &cfg.encoding, budget);
        // 首段装得下全部文本就不拆（与纯文本路径一致：单条发出）
        if all.len() <= 1 {
            vec![text.to_string()]
        } else {
            all
        }
    };

    let mut recs = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let p = if i == 0 { paths.clone() } else { vec![] };
        recs.push(send_message_opts(ctx, key, part, p, MsgSendOpts::default()).await?);
    }
    Ok(recs)
}

/// 带机密/群发标志的发送（前端封书/密码/群发按钮走这里；各段逐条上屏）
pub async fn send_message_multi_opts(
    ctx: &NetCtx,
    key: &str,
    text: &str,
    paths: Vec<String>,
    opts: MsgSendOpts,
) -> Result<Vec<Value>, String> {
    let cfg = ctx.st.config();
    // 与 send_message 的出站加密决策一致：encrypt 开启且已缓存对方公钥才会密封
    let will_seal = cfg.encrypt && ctx.st.peer_pubkey(key).is_some();
    let parts: Vec<String> = if !will_seal {
        vec![text.to_string()]
    } else if paths.is_empty() {
        split_text_for_seal(text, &cfg.encoding).unwrap_or_else(|| vec![text.to_string()])
    } else {
        // 附件公告：条目段也占密封预算；先校验路径并据条目开销算出文本预算，
        // 再按预算切分（首段带附件）。预算地板 64B：条目过大时退化为
        // 「首段仅 64B 文本 + 附件」，避免无意义的一堆纯文本段。
        let stats = stat_file_entries(&paths)?;
        let budget = attachment_text_budget(&stats, &cfg.encoding).max(64);
        let all = chunk_by_budget(text, &cfg.encoding, budget);
        // 首段装得下全部文本就不拆（与纯文本路径一致：单条发出）
        if all.len() <= 1 {
            vec![text.to_string()]
        } else {
            all
        }
    };

    let mut recs = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let p = if i == 0 { paths.clone() } else { vec![] };
        recs.push(send_message_opts(ctx, key, part, p, opts).await?);
    }
    Ok(recs)
}

/// 发送选项（机密性/群发标志；均由前端按钮决定）
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgSendOpts {
    /// 封书（SECRETOPT|READCHECKOPT = SECRETEXOPT，官方 senddlg 同款）
    pub secret: bool,
    /// 密码锁（PASSWORDOPT；仅本机启用密码功能时生效）
    pub password: bool,
    /// 多选群发（MULTICASTOPT：表示同一条消息同时发往多个目标）
    pub multicast: bool,
    /// 剪贴板贴图插入位置（官方「粘贴图片」语义）：设置后公告首条附件
    /// 带 FILE_CLIPBOARD(0x20) 属性与 CLIPBOARDPOS 扩展段，官方对端内嵌显示
    pub clip_pos: Option<u32>,
}

/// 发送文本/文件消息。paths 为空则纯文本。
pub async fn send_message(
    ctx: &NetCtx,
    key: &str,
    text: &str,
    paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    send_message_opts(ctx, key, text, paths, MsgSendOpts::default()).await
}

pub async fn send_message_opts(
    ctx: &NetCtx,
    key: &str,
    text: &str,
    paths: Vec<String>,
    opts: MsgSendOpts,
) -> Result<serde_json::Value, String> {
    let cfg = ctx.st.config();
    let pkt_no = proto::next_packet_no();
    let ts = now_secs();

    let peer = ctx.st.peers.lock().unwrap().get(key).cloned();
    let Some(peer) = peer else {
        // 对方不在线：文本与附件都进待投递队列（官方 IPMsg 语义，上线后自动
        // 重投，以原包号发送并带延迟尾注）。队列只存本地路径，文件槽与公告
        // ID 在重投时重新校验并登记（flush_pending_for）。
        return offline_enqueue_record(ctx, key, pkt_no, ts, text, &paths);
    };
    let target = peer_addr(&peer).ok_or("无效的对方地址")?;

    // UTF-8 编码模式决定公告字节序与取文件请求标志（官方 §3-9）
    let utf8 = proto::is_utf8_mode(&cfg.encoding);

    // 注册文件槽必须在发包前完成（对端可能立刻来取）
    let (mut entries, inserted) = if paths.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        register_offer_files(ctx, pkt_no, &paths, utf8)?
    };
    // 官方「粘贴图片」公告格式：首条附件 attr=FILE_CLIPBOARD(0x20) +
    // CLIPBOARDPOS=插入位置 扩展段（官方 share.cpp EncodeMsg 同款）
    if let Some(pos) = opts.clip_pos {
        if let Some(e) = entries.first_mut() {
            e.attr |= fileattr::CLIPBOARD;
            e.ext_attrs = vec![(proto::extattr::CLIPBOARDPOS, pos.to_string())];
        }
    }

    let mut extra = proto::encode_out(text, &cfg.encoding);
    if !entries.is_empty() {
        extra.push(0);
        let joined: Vec<String> = entries.iter().map(|e| e.serialize(&cfg.encoding)).collect();
        extra.extend_from_slice(joined.join("\u{7}").as_bytes());
        // 与真实客户端样本一致：公告末尾保留一个尾部 \a 分隔符
        extra.push(0x07);
    }

    // 文件消息不请求已读回执（减少未知标志组合被对端丢弃的风险）；
    // 纯文本消息保留回执；多选群发不回执（官方 MULTICASTOPT 语义）
    let want_rcpt = entries.is_empty() && !opts.multicast;

    // 出站加密决策（spec §5/§6）：开关开启且已缓存对方公钥 → 密封完整扩展部
    // （含尾部 \0）。seal_message 内置 UDP 上限保护，超限错误直接抛给前端分段。
    // 尚无缓存且对方未被标明文 → 本次仍明文发送，同时后台触发 GETPUBKEY
    // （发出即返回、不等应答），对方上线后预握手通常已完成，下条消息自然加密。
    // 探测不是无限的：retire_probe_budget 在第 PLAIN_PROBE_BUDGET 次探测后
    // 的首次评估时把对端标记为明文（spec §5「已标记无能力」），此后静默明文、
    // 不再对每个不支持加密的客户端永远反复 GETPUBKEY。
    let mut enc = false;
    let mut wire_extra = extra.clone();
    if cfg.encrypt {
        if let Some(pubk) = ctx.st.peer_pubkey(key) {
            // 现场诊断（2026-08 官方客户端互通排查）：出站加密前落盘实际
            // 公告明文（含分隔与尾部 \0），用于与官方规范逐字节比对
            let plain = plain_payload(&extra);
            let plain_dbg: String = plain
                .iter()
                .map(|&b| if b == 0 { '·'.to_string() } else if b == 7 { "\\a".to_string() } else { (b as char).to_string() })
                .collect::<Vec<_>>()
                .join("");
            ctx.st
                .diag(&format!("send-plain {key} enc len={} body={plain_dbg}", plain.len()));
            match crypto::seal_message(&pubk, &ctx.st.own_keypair(), &plain) {
                Ok(sealed) => {
                    wire_extra = sealed.into_bytes();
                    enc = true;
                }
                Err(e) => return Err(e), // 「消息过长…」等直接抛给前端
            }
        } else if ctx.st.retire_probe_budget(key) {
            // 预算用尽：内部恰在阈值穿越点 mark_peer_plain 一次，本条起走明文
        } else {
            send_getpubkey(ctx, target, &cfg).await;
        }
    }

    let command = cmd::SENDMSG
        // 官方默认语义：普通在线消息带 SENDCHECKOPT，未确认超时重发（§4-12）
        | if want_rcpt { opt::SENDCHECKOPT } else { 0 }
        | if want_rcpt { opt::READCHECKOPT } else { 0 }
        | if entries.is_empty() { 0 } else { opt::FILEATTACHOPT }
        | if utf8 { opt::UTF8OPT } else { 0 }
        // 封书 = SECRET|READCHECK（官方 SECRETEXOPT）；密码锁 = PASSWORDOPT
        | if opts.secret { opt::SECRETEXOPT } else { 0 }
        | if opts.password && cfg.password_use { opt::PASSWORDOPT } else { 0 }
        | if opts.multicast { opt::MULTICASTOPT } else { 0 }
        // 加密公告必须带 ENCEXTMSGOPT：官方解密后只在此位下拆分附件段（spec §5），
        // 缺位则对面只见文字、文件条目丢失（2026-08-26 官方客户端实测）
        | if enc && !entries.is_empty() { opt::ENCEXTMSGOPT } else { 0 }
        | if enc { opt::ENCRYPTOPT } else { 0 };
    let mut pkt = proto::Packet::new(command).with_pkt_no(pkt_no);
    pkt.extra = wire_extra;
    let bytes = pkt.encode(&my_user(&cfg), &my_host());

    // 线路诊断：记录我方出站公告原始字节（与 diag.log 入站样本对照用）
    {
        use std::fmt::Write as _;
        let raw = &pkt.extra[..pkt.extra.len().min(1600)];
        let mut hexs = String::with_capacity(raw.len() * 3);
        for b in raw {
            let _ = write!(hexs, "{b:02x} ");
        }
        ctx.st.diag(&format!(
            "-> {target} cmd={command:#010x} len={} extra[0..{}]={hexs}",
            bytes.len(),
            raw.len()
        ));
    }

    // 出站投递：中继会话（relay_agent 命中）或配置了代理时，把完整报文包成
    // AGENT_PACKET 发给代理，由代理转发；
    // 否则直发目标。
    let origin_ip: IpAddr = ctx
        .sock
        .local_addr()
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let inner_ip: IpAddr = peer.ip.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let (wire, send_to) = match ctx.st.relay_agent(key) {
        Some(agent) => (
            wrap_agent_packet(&cfg, &inner_ip, &origin_ip, &bytes),
            agent,
        ),
        None => match parse_agent_addr(&cfg.agent_addr) {
            Some(agent) => (
                wrap_agent_packet(&cfg, &inner_ip, &origin_ip, &bytes),
                agent,
            ),
            None => (bytes.clone(), target),
        },
    };
    if let Err(e) = ctx.sock.send_to(&wire, send_to).await {
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
            "dir_entry": e.attr & 0xFF == fileattr::DIR,
        })).collect::<Vec<_>>(),
        "ts": ts,
        "pkt": pkt_no,
        "peer": {"key": peer.key, "nickname": peer.nickname, "host": peer.host, "group": peer.group},
        "rcpt": want_rcpt,
        "read": false,
        // 本次实际是否加密发出；我方发出的消息签名恒可核验
        "enc": enc, "sig_ok": true,
        "secret": opts.secret,
        "locked": opts.password && cfg.password_use,
        "multicast": opts.multicast,
    });
    ctx.st.log_record(key, &rec);

    // 在线纯文本消息登记送达重发（官方 §4-12）：无 RECVMSG 时按 4s 间隔
    // 重发同一包号，累计 RETRY_MAX 次放弃；附件/群发消息不登记
    if want_rcpt && !opts.multicast {
        ctx.st.enqueue_retry(RetryOut {
            key: key.to_string(),
            pkt: pkt_no,
            text: text.to_string(),
            paths,
            entries,
            ts,
            attempts: 0,
        });
    }
    Ok(rec)
}

/* ================= 已读回执 ================= */

/// 解析 READMSG 来源对应的会话 key：会话身份即 IP，在线表里查得到就返回
fn resolve_session_key(ctx: &NetCtx, ip: IpAddr) -> Option<String> {
    let key = ip.to_string();
    if ctx.st.peers.lock().unwrap().contains_key(&key) {
        Some(key)
    } else {
        None
    }
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
    // 只对「要求回执且历史里仍未读」的消息发回执：前端可能因窗口焦点变化
    // 反复请求，对端的重发副本也会再次触发，这里做最后一道去重
    let todo = ctx.st.pending_receipts(key, pkts);
    ctx.st.mark_in_read(key, pkts);
    if todo.is_empty() {
        return Ok(0);
    }
    let pkts = &todo[..];

    let peer = ctx.st.peers.lock().unwrap().get(key).cloned();
    let Some(peer) = peer else {
        return Ok(0); // 对方不在线：仅本地标记
    };
    let Some(target) = peer_addr(&peer) else {
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

/* ================= 撤回 / 广播 / 群发 / 开封解锁 / 不在信息 ================= */

/// 撤回我方发出的某条消息（DELMSG，官方封书破弃语义的扩展用途）：
/// 向对端发 DELMSG(原包号)，并把本地记录标记为「已撤回」。
pub async fn recall_message(ctx: &NetCtx, key: &str, pkt: u32) -> Result<(), String> {
    // 只允许撤回我方发出的文本消息（附件消息撤回后对端无法再取，禁止）
    let rec = ctx.st.find_history_any(key, pkt);
    let Some(rec) = rec else {
        return Err("找不到该消息".into());
    };
    if rec.get("dir").and_then(|v| v.as_str()) != Some("out") {
        return Err("只能撤回自己发出的消息".into());
    }
    let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
    if kind != "text" {
        return Err("附件消息不支持撤回（对方可能已开始下载）".into());
    }
    let peer = ctx.st.peers.lock().unwrap().get(key).cloned();
    if let Some(peer) = peer {
        if let Some(target) = peer_addr(&peer) {
            let cfg = ctx.st.config();
            let mut p = proto::Packet::new(cmd::DELMSG);
            p.extra = pkt.to_string().into_bytes();
            let bytes = p.encode(&my_user(&cfg), &my_host());
            let _ = ctx.sock.send_to(&bytes, target).await;
        }
    }
    ctx.st.update_history_pkt(key, pkt, |r| {
        r["recalled"] = json!(true);
    });
    // 撤回后无需重发/回执跟踪
    let _ = ctx.st.ack_retry(key, pkt);
    let _ = ctx.st.ack_pending(key, pkt);
    ctx.st.emit("msg-recalled", json!({"key": key, "pkt": pkt, "peer": ""}));
    Ok(())
}

/// 广播群发（BROADCASTOPT 同报）：发给广播地址与所有在线成员，
/// 不回执、不触发对方不在自动应答（官方 MsgSendMsg 门控一致）。
/// 我方自己不落历史（官方 NOLOG 语义：广播不鼓励留痕）；
/// 对端收到进入其「广播」会话。
pub async fn broadcast_message(ctx: &NetCtx, text: &str) -> Result<(), String> {
    let cfg = ctx.st.config();
    let utf8 = proto::is_utf8_mode(&cfg.encoding);
    let mut pkt = proto::Packet::new(
        cmd::SENDMSG | opt::BROADCASTOPT | if utf8 { opt::UTF8OPT } else { 0 },
    );
    pkt.extra = proto::encode_out(text, &cfg.encoding);
    let bytes = pkt.encode(&my_user(&cfg), &my_host());
    let mut targets: Vec<SocketAddr> = broadcast_targets()
        .into_iter()
        .map(|ip| SocketAddr::from((ip, ctx.port)))
        .collect();
    {
        let peers = ctx.st.peers.lock().unwrap();
        targets.extend(peers.values().filter_map(peer_addr));
    } // 守卫在此释放，之后才 await
    let mut sent = 0;
    for t in targets {
        if ctx.sock.send_to(&bytes, t).await.is_ok() {
            sent += 1;
        }
    }
    ctx.st
        .diag(&format!("-> 广播群发「{}」到 {} 个目标", text.trim(), sent));
    Ok(())
}

/// 多选群发（MULTICASTOPT）：同一条文本依次发往多个会话。
/// 官方语义：多目标消息不回执；返回各目标的发送记录数组。
pub async fn multicast_message(
    ctx: &NetCtx,
    keys: &[String],
    text: &str,
) -> Result<Vec<Value>, String> {
    if keys.is_empty() {
        return Err("未选择发送目标".into());
    }
    let mut out = Vec::new();
    for k in keys {
        match send_message_opts(ctx, k, text, vec![], MsgSendOpts { multicast: true, ..Default::default() }).await {
            Ok(rec) => out.push(rec),
            Err(e) => return Err(format!("发给 {k} 失败：{e}")),
        }
    }
    Ok(out)
}

/// 封书/密码锁开封：校验密码（密码锁场景）后把记录标记 unlocked，
/// 并补发已读回执（官方 recvdlg 开封即回 READMSG）。
/// 返回是否成功开封（密码错/不满足条件返回 Err）。
pub async fn unlock_message(
    ctx: &NetCtx,
    key: &str,
    pkt: u32,
    password: Option<String>,
) -> Result<(), String> {
    let rec = ctx.st.find_history_pkt(key, pkt).ok_or("找不到该消息")?;
    let is_locked = rec.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
    let is_secret = rec.get("secret").and_then(|v| v.as_bool()).unwrap_or(false);
    if !is_locked && !is_secret {
        return Ok(()); // 无需解锁
    }
    if is_locked {
        let cfg = ctx.st.config();
        if cfg.password_use {
            let pw = password.unwrap_or_default();
            if !pw.eq(&cfg.password) || cfg.password.is_empty() {
                return Err("密码错误".into());
            }
        }
    }
    ctx.st.update_history_pkt(key, pkt, |r| {
        r["unlocked"] = json!(true);
        r["locked"] = json!(false);
        // 开封才计已读：清掉旧标记，让补发回执走 standard 去重通道
        if r.get("need_read").and_then(|v| v.as_bool()).unwrap_or(false) {
            r["read"] = json!(false);
        }
    });
    // 开封后补已读回执（此前 pending_receipts 已把未开封消息排除）
    let _ = mark_read_and_receipt(ctx, key, &[pkt]).await;
    ctx.st.emit("msg-unlocked", json!({"key": key, "pkt": pkt}));
    Ok(())
}

/// 主动索取对端不在通知文（GETABSENCEINFO → SENDABSENCEINFO）
pub async fn request_absence_info(ctx: &NetCtx, key: &str) -> Result<(), String> {
    let peer = ctx
        .st
        .peers
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or("对方不在线")?;
    let Some(target) = peer_addr(&peer) else {
        return Ok(());
    };
    let cfg = ctx.st.config();
    let p = proto::Packet::new(cmd::GETABSENCEINFO);
    let bytes = p.encode(&my_user(&cfg), &my_host());
    let _ = ctx.sock.send_to(&bytes, target).await;
    ctx.st.diag(&format!("-> {key} GETABSENCEINFO"));
    Ok(())
}

/// 主动发起主机列表交换：广播 BR_ISGETLIST 并打开获取窗口
pub async fn request_hostlist(ctx: &NetCtx) -> Result<(), String> {
    ctx.st.open_hostlist_window(30);
    let cfg = ctx.st.config();
    let p = proto::Packet::new(cmd::BR_ISGETLIST | opt::RETRYOPT);
    let bytes = p.encode(&my_user(&cfg), &my_host());
    for ip in broadcast_targets() {
        let _ = ctx
            .sock
            .send_to(&bytes, SocketAddr::from((ip, ctx.port)))
            .await;
    }
    ctx.st.diag("-> 广播 BR_ISGETLIST（主机列表交换）");
    Ok(())
}

/* ================= 成员主目录服务（DIR_MASTER / IPDict） ================= */

/// 组装 IPDict 报文的公共字段（官方 InitIPDict 同款：VER/PKT/DATE/UID/HID/
/// CMD/FLG/CVER/GRP/NCK/STAT）
fn dict_init(
    cfg: &Config,
    cmd: u32,
    flags: u32,
    user: &str,
    host: &str,
) -> crate::ipdict::Dict {
    use crate::ipdict::*;
    let mut d = Dict::new();
    d.put_int(DICT_VER, 3)
        .put_int(DICT_PKT, proto::next_packet_no() as i64)
        .put_int(DICT_DATE, now_secs() as i64)
        .put_str(DICT_UID, user)
        .put_str(DICT_HID, host)
        .put_int(DICT_CMD, cmd as i64)
        .put_int(DICT_FLG, flags as i64)
        .put_str(DICT_CVER, &my_ver_hex_info())
        .put_str(DICT_GRP, &cfg.group)
        .put_str(DICT_NCK, &cfg.nickname)
        .put_int(DICT_STAT, entry_caps(cfg) as i64);
    d
}

/// 签名 IPDict（官方 SignIPDict 同款）：PUB_E/PUB_N/EF/EC，SIGN = RSA-SHA256
/// 覆盖除 SIGN 外的全部打包字节。官方内部字节序怪癖（swap_s）在真实互通
/// 中已被推翻（见 crypto.rs 注释），这里按标准大端实现，解析端宽容。
fn dict_sign(ctx: &NetCtx, d: &mut crate::ipdict::Dict) -> Result<(), String> {
    let capa = entry_caps(&ctx.st.config()) | crypto::CAPA_OUR_SEND;
    crypto::sign_ipdict(d, &ctx.st.own_keypair(), capa)
}

/// 校验 IPDict 签名：从 PUB_E/PUB_N/SIGN 重建公钥核验（SHA-256）。
/// 无 SIGN（我方的未签名报文）返回 Ok(false)；解析失败按校验失败处理。
fn dict_verify(d: &crate::ipdict::Dict) -> Result<bool, String> {
    crypto::verify_ipdict(d).map(|verified| verified.is_some())
}

/// 一台主机 → IPDict 主机字典（官方 MakeHostDict 同款字段：
/// IPAD/PORT/STAT/NCK/GRP/UID/HID）
fn make_host_dict(p: &PeerInfo) -> crate::ipdict::Dict {
    use crate::ipdict::*;
    let mut d = Dict::new();
    d.put_str(DICT_IPAD, &p.ip)
        .put_int(DICT_PORT, p.port as i64)
        .put_int(DICT_STAT, (cmd::BR_ENTRY | if p.absence { opt::ABSENCEOPT } else { 0 }) as i64)
        .put_str(DICT_UID, &p.user)
        .put_str(DICT_HID, &p.host)
        .put_str(DICT_NCK, &p.nickname)
        .put_str(DICT_GRP, &p.group);
    d
}

/// 从 IPDict 主机字典还原 PeerInfo（DIR_PACKET/ANSLIST_DICT/ANSBROAD 共用）
fn peer_from_host_dict(d: &crate::ipdict::Dict) -> Option<PeerInfo> {
    use crate::ipdict::*;
    let ip = d.get_str(DICT_IPAD)?.to_string();
    ip.parse::<IpAddr>().ok()?;
    let status = d.get_int(DICT_STAT).unwrap_or(0) as u32;
    let nick = d.get_str(DICT_NCK).unwrap_or("").to_string();
    let group = d.get_str(DICT_GRP).unwrap_or("").to_string();
    let user = d.get_str(DICT_UID).unwrap_or("").to_string();
    let host = d.get_str(DICT_HID).unwrap_or("").to_string();
    let port = d.get_int(DICT_PORT).unwrap_or(2425) as u16;
    Some(PeerInfo {
        key: ip.clone(),
        ip,
        port,
        nickname: if nick.is_empty() { user.clone() } else { nick },
        group,
        host,
        user,
        last_seen: now_secs(),
        absence: status & opt::ABSENCEOPT != 0,
        absence_text: None,
        vs: None,
    })
}

/* ================= 官方 v5 密文消息（EncIPDict） ================= */

/// 处理一封官方 v5 IPDict 密文消息（完整 `IP2:...:Z` 外层含 EF/EI/EK/EB）。
async fn handle_encipdict(ctx: &NetCtx, outer: &crate::ipdict::Dict, from: SocketAddr) {
    let key = from.ip().to_string();
    if !ctx.st.config().encrypt {
        ctx.st.diag(&format!(
            "<- {from} EncIPDict stage=config len={} keys={} pkt=? command=? 被拒绝：本机加密已关闭",
            outer.pack().len(),
            outer.items.len()
        ));
        return;
    }
    let inner = match crypto::open_encipdict(&ctx.st.own_keypair(), outer) {
        Ok(inner) => inner,
        Err(error) => {
            ctx.st.diag(&format!(
                "<- {from} EncIPDict stage=decrypt len={} keys={} pkt=? command=? 失败：{error}",
                outer.pack().len(),
                outer.items.len()
            ));
            crypto_rehandshake(ctx, from, &key, "encipdict-decrypt-fail").await;
            return;
        }
    };
    let inner_len = inner.pack().len();
    let packet = match resolve_ipdict_packet(&inner) {
        Ok(packet) => packet,
        Err(error) => {
            ctx.st.diag(&format!(
                "<- {from} EncIPDict stage=resolve len={inner_len} keys={} pkt=? command=? 失败：{error}",
                inner.items.len()
            ));
            return;
        }
    };
    let (public, capa) = match crypto::verify_ipdict(&inner) {
        Ok(Some(verified)) => verified,
        Ok(None) => {
            ctx.st.diag(&format!(
                "<- {from} EncIPDict stage=verify len={inner_len} keys={} pkt={} command={:#010x} 缺 SIGN",
                inner.items.len(),
                packet.pkt_no,
                packet.command
            ));
            return;
        }
        Err(error) => {
            ctx.st.diag(&format!(
                "<- {from} EncIPDict stage=verify len={inner_len} keys={} pkt={} command={:#010x} 失败：{error}",
                inner.items.len(),
                packet.pkt_no,
                packet.command
            ));
            return;
        }
    };
    ctx.st.remember_peer_key(&key, capa, &public);
    ctx.st.diag(&format!(
        "<- {from} EncIPDict stage=verified len={inner_len} keys={} pkt={} command={:#010x}",
        inner.items.len(),
        packet.pkt_no,
        packet.command
    ));

    if !ctx.st.mark_seen(from.ip(), packet.pkt_no) {
        ack_encipdict(ctx, from, packet.command, packet.pkt_no).await;
        return;
    }
    if packet.command & 0xff != cmd::SENDMSG {
        ctx.st.diag(&format!(
            "<- {from} EncIPDict stage=dispatch len={inner_len} keys={} pkt={} command={:#010x} 非 SENDMSG",
            inner.items.len(),
            packet.pkt_no,
            packet.command
        ));
        return;
    }
    handle_sendmsg(ctx, from, &packet, &key, Some(true)).await;
    ack_encipdict(ctx, from, packet.command, packet.pkt_no).await;
}

/// EncIPDict 送达确认（RECVMSG，包号取消息字典 PKT —— 官方应答口径）
async fn ack_encipdict(ctx: &NetCtx, from: SocketAddr, command: u32, pkt_no: u32) {
    if command & opt::SENDCHECKOPT == 0
        || command & (opt::BROADCASTOPT | opt::AUTORETOPT) != 0
    {
        return;
    }
    let cfg = ctx.st.config();
    let mut r = proto::Packet::new(cmd::RECVMSG | opt::AUTORETOPT);
    r.extra = pkt_no.to_string().into_bytes();
    let bytes = r.encode(&my_user(&cfg), &my_host());
    let _ = reply_send(ctx, &from.ip().to_string(), from, &bytes).await;
    ctx.st
        .diag(&format!("-> {from} EncIPDict RECVMSG 送达确认 pkt={pkt_no}"));
}

/// 纯 IPDict 报文分发（官方 ResolveDictMsg 同款）：DIR_* 与 ANSLIST_DICT
async fn handle_dict_datagram(ctx: &NetCtx, dict: &crate::ipdict::Dict, from: SocketAddr) {
    use crate::ipdict::*;
    if from.port() == ctx.port && local_ip_set().contains(&from.ip()) {
        return;
    }
    let Some(cmd_val) = dict.get_int(DICT_CMD) else {
        return;
    };
    let base = cmd_val as u32 & 0xFF;
    let key = from.ip().to_string();
    match base {
        cmd::DIR_POLL => {
            // 成员主侧：登记成员 → 按网段选举代理（POLLAGENT + BROADCAST）
            let cfg = ctx.st.config();
            if cfg.dir_mode != "master" {
                return;
            }
            let seg = {
                poll_networks(dict)
                .iter()
                .filter_map(|d| {
                    let addr = d.get_str(DICT_ADDR)?;
                    let mask = d.get_int(DICT_MASK).unwrap_or(24);
                    Some(format!("{addr}/{mask}"))
                })
                .collect::<Vec<_>>()
                .join(",")
            };
            // 该网段已有代理就不重复任命；否则给新 POLL 成员发 POLLAGENT+BROADCAST
            let already_agent = ctx
                .st
                .dir_members_snapshot()
                .iter()
                .any(|m| m.seg == seg && m.is_agent);
            ctx.st.upsert_dir_member(DirMember {
                key: key.clone(),
                port: from.port(),
                last_poll: now_secs(),
                agent_secs: if already_agent { 0 } else { 300 },
                seg,
                is_agent: !already_agent,
            });
            if !already_agent {
                let mut d = dict_init(&cfg, cmd::DIR_POLLAGENT, 0, &my_user(&cfg), &my_host());
                d.put_int(DICT_AGS, 300);
                d.put_str(DICT_TARG, &key);
                let _ = dict_sign(ctx, &mut d);
                // 回执目标必须用成员的源端口（成员可能不在 2425）
                let member_addr = SocketAddr::from((from.ip(), from.port()));
                let dbytes = d.pack();
                let _ = ctx.sock.send_to(&dbytes, member_addr).await;
                let mut b = dict_init(&cfg, cmd::DIR_BROADCAST, 0, &my_user(&cfg), &my_host());
                b.put_int(DICT_AGS, 300);
                let _ = dict_sign(ctx, &mut b);
                let bbytes = b.pack();
                let _ = ctx.sock.send_to(&bbytes, member_addr).await;
                ctx.st
                    .diag(&format!("dir-master: 任命 {key} 为代理（POLLAGENT+BROADCAST）"));
            }
            // 成员自身也并入全网列表
            let mut hd = crate::ipdict::Dict::new();
            hd.put_str(DICT_IPAD, &key)
                .put_int(DICT_PORT, 2425)
                .put_int(DICT_STAT, (cmd::BR_ENTRY | entry_caps(&cfg)) as i64)
                .put_str(DICT_UID, dict.get_str(DICT_UID).unwrap_or(""))
                .put_str(DICT_HID, dict.get_str(DICT_HID).unwrap_or(""))
                .put_str(DICT_NCK, dict.get_str(DICT_NCK).unwrap_or(""))
                .put_str(DICT_GRP, dict.get_str(DICT_GRP).unwrap_or(""));
            ctx.st.merge_master_hosts(&[hd]);
            push_dir_packet(ctx, "成员 POLL 后广播").await;
        }
        cmd::DIR_POLLAGENT => {
            // 成员侧：自己被任命代理（AGS 生效窗口）
            let ags = dict.get_int(DICT_AGS).unwrap_or(60).max(10) as u64;
            ctx.st.set_agent_until(&from.ip().to_string(), ags);
            ctx.st
                .diag(&format!("<- {from} DIR_POLLAGENT：本机成为代理（{ags}s）"));
        }
        cmd::DIR_BROADCAST => {
            // 成员侧（代理）：立即在本段广播 BR_ENTRY，收集成员列表后经
            // DIR_ANSBROAD 回报 master；期间 entry 事件也转发 master（EVBROAD）
            let cfg = ctx.st.config();
            let ags = dict.get_int(DICT_AGS).unwrap_or(60).max(10) as u64;
            ctx.st.set_agent_until(&from.ip().to_string(), ags);
            announce(ctx).await;
            tokio::time::sleep(Duration::from_millis(4000)).await;
            let hosts: Vec<crate::ipdict::Dict> = {
                let peers = ctx.st.peers.lock().unwrap();
                peers.values().map(make_host_dict).collect()
            };
            if let Some(master) = cfg_master_addr(&cfg) {
                let mut d = dict_init(&cfg, cmd::DIR_ANSBROAD, 0, &my_user(&cfg), &my_host());
                d.put_dict_list(DICT_HLST, &hosts);
                d.put_int(DICT_DIRECT, 1);
                let _ = dict_sign(ctx, &mut d);
                let dbytes = d.pack();
                let _ = ctx.sock.send_to(&dbytes, master).await;
                ctx.st
                    .diag(&format!("-> {master} DIR_ANSBROAD：回报 {} 台成员", hosts.len()));
            }
        }
        cmd::DIR_ANSBROAD | cmd::DIR_EVBROAD => {
            // 成员主侧：合并代理回报的成员列表并重发 DIR_PACKET
            let cfg = ctx.st.config();
            if cfg.dir_mode != "master" {
                return;
            }
            let hosts: Vec<crate::ipdict::Dict> = dict.get_dict_list(DICT_HLST);
            let changed = ctx.st.merge_master_hosts(&hosts);
            ctx.st.diag(&format!(
                "<- {from} {}：并入 {}/{} 台（净增 {changed}）",
                if base == cmd::DIR_ANSBROAD { "DIR_ANSBROAD" } else { "DIR_EVBROAD" },
                hosts.len(),
                hosts.len()
            ));
            push_dir_packet(ctx, "代理回报").await;
        }
        cmd::DIR_PACKET => {
            // 成员侧：master 全网列表分发 → 并入用户表
            let hosts: Vec<crate::ipdict::Dict> = dict.get_dict_list(DICT_HLST);
            // 验签（master 报文自嵌公钥；无签名/验签失败仅记诊断，不阻断展示）
            match dict_verify(dict) {
                Ok(true) => {}
                Ok(false) => ctx
                    .st
                    .diag(&format!("<- {from} DIR_PACKET 无签名（忽略校验）")),
                Err(e) => ctx
                    .st
                    .diag(&format!("<- {from} DIR_PACKET 验签失败：{e}")),
            }
            let mut changed = false;
            for h in &hosts {
                if let Some(p) = peer_from_host_dict(h) {
                    // 过滤自身条目（主侧列表中会包含本机）
                    if p.ip.parse::<IpAddr>().map(|ip| is_self_ip(ctx, ip)).unwrap_or(false) {
                        continue;
                    }
                    changed |= ctx.st.upsert_peer(p);
                }
            }
            if changed {
                ctx.st.emit("users-updated", json!({}));
            }
            ctx.st
                .diag(&format!("<- {from} DIR_PACKET：并入 {} 台", hosts.len()));
        }
        cmd::DIR_REQUEST | cmd::DIR_AGENTPACKET => {
            // 成员主协议的包中转（官方 DIR_REQUEST/AGENTPACKET）：我方作代理时
            // 把指向本段成员的请求原样转交；成员侧收到即按普通 IPDict 处理。
            // 极简实现：DIAG 记录并丢弃（官方 Win 客户端同样未完成该链路）
            ctx.st
                .diag(&format!("<- {from} DIR_REQUEST/AGENTPACKET 忽略（中继链路未启用）"));
        }
        cmd::DIR_AGENTREJECT => {
            ctx.st.diag(&format!("<- {from} DIR_AGENTREJECT：代理任命被拒"));
        }
        cmd::ANSLIST_DICT => {
            // IPDict 版主机列表（v5）：并入用户表
            let hosts: Vec<crate::ipdict::Dict> = dict.get_dict_list(DICT_HLST);
            let mut changed = false;
            for h in &hosts {
                if let Some(p) = peer_from_host_dict(h) {
                    changed |= ctx.st.upsert_peer(p);
                }
            }
            if changed {
                ctx.st.emit("users-updated", json!({}));
            }
            ctx.st
                .diag(&format!("<- {from} ANSLIST_DICT：并入 {} 台", hosts.len()));
        }
        _ => {}
    }
}

/// DIR_POLL 的 NADRS 可按官方 list 发送，也兼容旧单 dict 形态。
fn poll_networks(dict: &crate::ipdict::Dict) -> Vec<crate::ipdict::Dict> {
    let nets = dict.get_dict_list(crate::ipdict::DICT_NADDRS);
    if nets.is_empty() {
        dict.get_dict(crate::ipdict::DICT_NADDRS)
            .into_iter()
            .collect()
    } else {
        nets
    }
}



/// 成员主周期任务（spawn_dir_loop）：成员侧发 POLL；主侧定期 push 全网列表
pub(crate) async fn dir_tick(ctx: &NetCtx) {
    let cfg = ctx.st.config();
    match cfg.dir_mode.as_str() {
        "user" => {
            // 成员侧：向 master 发 DIR_POLL（官方 PollSend 同款，含本机网段）
            let Some(master) = cfg_master_addr(&cfg) else {
                return;
            };
            let mut d = dict_init(&cfg, cmd::DIR_POLL, 0, &my_user(&cfg), &my_host());
            let mut list: Vec<crate::ipdict::Dict> = Vec::new();
            if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
                for (_, ip) in ifaces {
                    let IpAddr::V4(v4) = ip else { continue };
                    if v4.is_loopback() || v4.is_unspecified() {
                        continue;
                    }
                    let o = v4.octets();
                    let mut nd = crate::ipdict::Dict::new();
                    nd.put_str(
                        crate::ipdict::DICT_ADDR,
                        &format!("{}.{}.{}.0", o[0], o[1], o[2]),
                    )
                    .put_int(crate::ipdict::DICT_MASK, 24);
                    list.push(nd);
                }
            }
            d.put_dict_list(crate::ipdict::DICT_NADDRS, &list);
            let _ = dict_sign(ctx, &mut d);
            let dbytes = d.pack();
            let _ = ctx.sock.send_to(&dbytes, master).await;
            ctx.st
                .diag(&format!("-> {master} DIR_POLL（{} 段网段）", list.len()));
        }
        "master" => {
            // 主侧：周期 push 全网列表 + 清理离线成员
            let gone = ctx.st.prune_dir_members(3 * 60);
            if !gone.is_empty() {
                ctx.st
                    .diag(&format!("dir-master: 清理离线成员 {gone:?}"));
            }
            push_dir_packet(ctx, "周期广播").await;
        }
        _ => {}
    }
}

/// 成员主侧：把所有成员列表以 DIR_PACKET 分发（签名）
pub(crate) async fn push_dir_packet(ctx: &NetCtx, reason: &str) {
    let cfg = ctx.st.config();
    if cfg.dir_mode != "master" {
        return;
    }
    let members = ctx.st.dir_members_snapshot();
    if members.is_empty() {
        return;
    }
    let mut hosts = ctx.st.master_hosts();
    // 主侧自身恒在列表首位（成员侧据此发现主；自检回环下也用得到真实端口）
    if let Ok(la) = ctx.sock.local_addr() {
        let mut hd = crate::ipdict::Dict::new();
        hd.put_str(ipd::DICT_IPAD, &la.ip().to_string())
            .put_int(ipd::DICT_PORT, la.port() as i64)
            .put_int(ipd::DICT_STAT, (cmd::BR_ENTRY | entry_caps(&cfg)) as i64)
            .put_str(ipd::DICT_UID, &my_user(&cfg))
            .put_str(ipd::DICT_HID, &my_host())
            .put_str(ipd::DICT_NCK, &cfg.nickname)
            .put_str(ipd::DICT_GRP, &cfg.group);
        hosts.insert(0, hd);
    }
    let mut d = dict_init(
        &cfg,
        cmd::DIR_PACKET,
        0,
        &my_user(&cfg),
        &my_host(),
    );
    d.put_int(crate::ipdict::DICT_START, 0)
        .put_int(crate::ipdict::DICT_NUM, hosts.len() as i64)
        .put_int(crate::ipdict::DICT_TOTAL, hosts.len() as i64);
    d.put_dict_list(crate::ipdict::DICT_HLST, &hosts);
    if let Err(e) = dict_sign(ctx, &mut d) {
        ctx.st.diag(&format!("dir-master: DIR_PACKET 签名失败：{e}"));
        return;
    }
    let bytes = d.pack();
    for m in &members {
        let ip = match m.key.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let port = if m.port != 0 { m.port } else { proto::DEFAULT_PORT };
        let _ = ctx.sock.send_to(&bytes, SocketAddr::from((ip, port))).await;
    }
    ctx.st
        .diag(&format!("dir-master: DIR_PACKET 分发 {reason}（{} 台，{} 成员）", hosts.len(), members.len()));
}

/* ================= NAT 中继代理（AGENT 协议，自洽设计） ================= */

/// AGENT_PACKET 附加数据布局（本实现的自洽约定，官方未定义线格式）：
/// `<目标IP>:<来源IP>:<被包裹的完整经典报文>`
/// - 代理收到后转发给目标IP（代理用 relay_peers 记录的真实地址）；
/// - 目的地解包后按「来源IP」建会话，应答包回包成 AGENT_PACKET 走代理。

/// 把一条完整报文包成 AGENT_PACKET 发给代理（目标=对端 IP）
fn wrap_agent_packet(cfg: &Config, target_ip: &IpAddr, origin_ip: &IpAddr, inner: &[u8]) -> Vec<u8> {
    let mut w = proto::Packet::new(cmd::AGENT_PACKET);
    w.extra = format!("{target_ip}:{origin_ip}:").into_bytes();
    w.extra.extend_from_slice(inner);
    w.encode(&my_user(cfg), &my_host())
}

/// 解包 AGENT_PACKET：返回 (目标IP, 来源IP, 内层报文字节)
fn unwrap_agent_packet(extra: &[u8]) -> Option<(IpAddr, IpAddr, Vec<u8>)> {
    let s = String::from_utf8_lossy(extra);
    let (targ, rest) = s.split_once(':')?;
    let (orig, inner) = rest.split_once(':')?;
    let targ = targ.parse::<IpAddr>().ok()?;
    let orig = orig.parse::<IpAddr>().ok()?;
    if inner.len() > 64000 {
        return None;
    }
    // 内层可能是「带 NUL 的消息体」之外的任意字节——但包头必须是 ASCII，
    // 这里按原始字节截取（split_once 已在字符串上做过，含校验和的边界
    // 以字符串形式存在；NUL 字节可能被丢弃——对本协议自洽实现可接受）
    Some((targ, orig, inner.as_bytes().to_vec()))
}

/// 成员侧配置的 master 地址（"ip[:port]"；端口缺省 2425）
fn cfg_master_addr(cfg: &Config) -> Option<SocketAddr> {
    let s = cfg.master_addr.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    s.parse::<IpAddr>()
        .ok()
        .map(|ip| SocketAddr::from((ip, proto::DEFAULT_PORT)))
}

/// 代理地址解析："ip:port" 或裸 ip（默认 2425）
fn parse_agent_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    s.parse::<IpAddr>()
        .ok()
        .map(|ip| SocketAddr::from((ip, proto::DEFAULT_PORT)))
}


/// 本机身份判定：绑定了具体 IP 的套接字（自检多实例 127.0.0.x、NAT 中继
/// 目标识别）以绑定地址为准；绑定 0.0.0.0 时按网卡枚举地址集（含 127.0.0.1）。
fn is_self_ip(ctx: &NetCtx, ip: IpAddr) -> bool {
    let l = ctx
        .sock
        .local_addr()
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if l.is_unspecified() {
        local_ip_set().contains(&ip)
    } else {
        l == ip
    }
}

/// 应答投递：对端是经代理中继的会话（relay_agent 命中）时把应答也包成
/// AGENT_PACKET 走代理；否则直发。
async fn reply_send(ctx: &NetCtx, key: &str, from: SocketAddr, bytes: &[u8]) -> bool {
    if let Some(agent) = ctx.st.relay_agent(key) {
        let target: IpAddr = match key.parse() {
            Ok(ip) => ip,
            Err(_) => return false,
        };
        // origin = 回包者自身（对端据它建立返回会话）；from 仍是代理地址
        let origin: IpAddr = ctx
            .sock
            .local_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cfg = ctx.st.config();
        let w = wrap_agent_packet(&cfg, &target, &origin, bytes);
        return ctx.sock.send_to(&w, agent).await.is_ok();
    }
    ctx.sock.send_to(bytes, from).await.is_ok()
}

/// 我方作为代理：收到 AGENT_PACKET 时的转发/本地投递决策
async fn handle_agent_packet(ctx: &NetCtx, pkt: &proto::Packet, from: SocketAddr) {
    let Some((target, origin, inner)) = unwrap_agent_packet(&pkt.extra) else {
        ctx.st.diag(&format!("<- {from} AGENT_PACKET 解析失败"));
        return;
    };
    ctx.st
        .diag(&format!("<- {from} AGENT_PACKET target={target} origin={origin} len={}", inner.len()));
    // 登记来源对端的真实地址（转发目标）
    ctx.st.remember_relay_peer(&origin.to_string(), from);
    // 目标是本机 → 本地解包投递（会话键=来源 IP；应答回包走代理）。
    // 递归经 Box::pin 打破（async 递归限制）
    if is_self_ip(ctx, target) {
        ctx.st.remember_relay_agent(&origin.to_string(), from);
        let orig_addr = SocketAddr::from((origin, 2425));
        Box::pin(handle_datagram(ctx, &inner, orig_addr)).await;
        return;
    }
    // 目标是已知的代理对端 → 用其真实地址转发（AGENT_PACKET 原样第二跳）
    if let Some(real) = ctx.st.relay_peer(&target.to_string()) {
        let cfg = ctx.st.config();
        let bytes = pkt.encode(&my_user(&cfg), &my_host());
        let _ = ctx.sock.send_to(&bytes, real).await;
        ctx.st
            .diag(&format!("-> {real} AGENT_PACKET 转发（目标 {target}）"));
        return;
    }
    // 目标是局域网内普通成员 → 直发其 2425
    if !target.is_loopback() {
        let cfg = ctx.st.config();
        let bytes = pkt.encode(&my_user(&cfg), &my_host());
        let _ = ctx
            .sock
            .send_to(&bytes, SocketAddr::from((target, ctx.port)))
            .await;
    }
    ctx.st
        .diag(&format!("<- {from} AGENT_PACKET 目标 {target} 未知，尽力直发"));
}

/// 代理窗口内：本段 entry 事件（BR_ENTRY/BR_EXIT/BR_ABSENCE）转发给 master
/// （官方 AgentDirHost 同款，DIR_EVBROAD + HLST 单条主机字典 + DRCT=1）
async fn maybe_forward_entry_event(ctx: &NetCtx, pkt: &proto::Packet, key: &str, base: u32) {
    let cfg = ctx.st.config();
    if cfg.dir_mode != "user" {
        return;
    }
    let Some(master) = cfg_master_addr(&cfg) else {
        return;
    };
    if !ctx.st.agent_active(&master.ip().to_string()) {
        return;
    }
    // 自身回声与 master 自身的 entry 不转发
    let ip: IpAddr = match key.parse() {
        Ok(ip) => ip,
        Err(_) => return,
    };
    if is_self_ip(ctx, ip) || key == master.ip().to_string() {
        return;
    }
    let mut hd = crate::ipdict::Dict::new();
    hd.put_str(ipd::DICT_IPAD, key)
        .put_int(ipd::DICT_PORT, 2425)
        .put_int(ipd::DICT_STAT, base as i64)
        .put_str(ipd::DICT_UID, &pkt.user)
        .put_str(ipd::DICT_HID, &pkt.host)
        .put_str(ipd::DICT_NCK, &pkt.user);
    let mut d = dict_init(&cfg, cmd::DIR_EVBROAD, 0, &my_user(&cfg), &my_host());
    d.put_dict_list(ipd::DICT_HLST, &[hd]);
    d.put_int(ipd::DICT_DIRECT, 1);
    let _ = dict_sign(ctx, &mut d);
    let dbytes = d.pack();
    let _ = ctx.sock.send_to(&dbytes, master).await;
    ctx.st
        .diag(&format!("-> {master} DIR_EVBROAD：转发 {key} 的 entry 事件（cmd={base:#x}）"));
}

/* ================= 单元测试 ================= */

// ---------------------------------------------------------------------------
// 加密长文本自动分段
// ---------------------------------------------------------------------------

/// 单段文本按当前发送编码编码后的最大字节数。
/// `seal_message` 的明文预算为 3400 字节（含尾部 \0，见 crypto::MAX_PLAIN_FOR_SEAL）；
/// 这里留出离线延迟重投时官方尾注（约 45B）的余量，保证分段后的每一条在
/// 离线补投追加尾注后仍能保持加密（不会降级明文）。
pub const CHUNK_PLAIN_BUDGET: usize = 3300;

/// 文本按当前发送编码超出密封预算时，返回按**字符边界**切好的分段
/// （每段编码后 ≤ CHUNK_PLAIN_BUDGET 字节，拼接还原原文），否则返回 None。
pub fn split_text_for_seal(text: &str, encoding: &str) -> Option<Vec<String>> {
    // 逐字符编码：UTF-8 与 GBK 都是变长编码，必须按实际编码字节数累计，
    // 且切分点只落在字符边界（绝不劈开多字节字符）。
    let total: usize = text
        .chars()
        .map(|c| proto::encode_out(&c.to_string(), encoding).len())
        .sum();
    if total <= CHUNK_PLAIN_BUDGET {
        return None;
    }
    Some(chunk_by_budget(text, encoding, CHUNK_PLAIN_BUDGET))
}

/// 离线延迟重投官方尾注的最大编码字节数
/// （"\n----\n(IPMsg Delayed Send: MM/DD HH:MM )"，ASCII 各单位 1B，留余量）。
pub const DELAYED_NOTE_BUDGET: usize = 48;

/// 附件公告里条目段的编码字节数上界：Σ(条目序列化长度 + 1 条目间分隔符 \a)。
/// 条目 id 按登记方 FILE_ID_SEQ 风格的 10 位十进制最坏情况估算（实际通常 9 位，
/// 多算的 1 位就是余量）。
pub fn entries_wire_bytes_upper(entries: &[proto::FileEntry], encoding: &str) -> usize {
    let mut total = 0;
    for e in entries {
        let mut w = e.clone();
        w.id = 1_000_000_000;
        total += w.serialize(encoding).len() + 1;
    }
    total
}

/// 附件公告中**文本段**的最大字节数：密封明文预算（CHUNK_PLAIN_BUDGET）减去
/// 条目段、正文/条目分隔符（正文后 \0 + 尾部 \a + 整体尾部 \0 共 3B）
/// 与离线尾注余量；条目过大时饱和到 0。
pub fn attachment_text_budget(entries: &[proto::FileEntry], encoding: &str) -> usize {
    CHUNK_PLAIN_BUDGET
        .saturating_sub(entries_wire_bytes_upper(entries, encoding) + 3 + DELAYED_NOTE_BUDGET)
}

/// 按任意字节预算把文本切成段（字符边界；每段编码后 ≤ budget，拼接还原原文）。
/// 装得下时返回单元素；空文本返回空数组。
pub fn chunk_by_budget(text: &str, encoding: &str, budget: usize) -> Vec<String> {
    let encoded: Vec<Vec<u8>> = text
        .chars()
        .map(|c| proto::encode_out(&c.to_string(), encoding))
        .collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for (ch, bytes) in text.chars().zip(encoded) {
        if cur_len > 0 && cur_len + bytes.len() > budget {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        cur.push(ch);
        cur_len += bytes.len();
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_data_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "open-ipmsg-net-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    async fn encipdict_test_ctx(
        label: &str,
    ) -> (NetCtx, Arc<AppState>, Arc<std::net::UdpSocket>, PathBuf) {
        let data_dir = test_data_dir(label);
        let st = Arc::new(AppState::new(data_dir.clone()));
        let sock = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
        let peer = Arc::new(std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let port = sock.local_addr().unwrap().port();
        let ctx = NetCtx {
            st: st.clone(),
            sock,
            v6_sock: tokio::sync::Mutex::new(None),
            port,
        };
        (ctx, st, peer, data_dir)
    }

    fn seal_raw_ipdict(
        peer_pub: &rsa::RsaPublicKey,
        inner: &crate::ipdict::Dict,
    ) -> crate::ipdict::Dict {
        use ctr::cipher::{KeyIvInit, StreamCipher};
        use rand::RngCore as _;

        let mut key = [0u8; 32];
        let mut iv = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut key);
        rand::thread_rng().fill_bytes(&mut iv);
        let mut body = inner.pack();
        let mut cipher = ctr::Ctr128BE::<aes::Aes256>::new_from_slices(&key, &iv).unwrap();
        cipher.apply_keystream(&mut body);
        let encrypted_key = peer_pub
            .encrypt(&mut rand::thread_rng(), rsa::Pkcs1v15Encrypt, &key)
            .unwrap();
        let mut outer = crate::ipdict::Dict::new();
        outer
            .put_int(crate::ipdict::DICT_EF, crate::ipdict::ENCIPDICT_EF)
            .put_bytes(crate::ipdict::DICT_ENCIV, &iv)
            .put_bytes(crate::ipdict::DICT_ENCKEY, &encrypted_key)
            .put_bytes(crate::ipdict::DICT_ENCBODY, &body);
        outer
    }

    fn official_sendmsg_dict(body: &str, flags: u32) -> crate::ipdict::Dict {
        let mut d = crate::ipdict::Dict::new();
        d.put_int(crate::ipdict::DICT_VER, 3)
            .put_int(crate::ipdict::DICT_PKT, 665500)
            .put_str(crate::ipdict::DICT_UID, "sender")
            .put_str(crate::ipdict::DICT_HID, "win-host")
            .put_int(crate::ipdict::DICT_CMD, crate::protocol::cmd::SENDMSG as i64)
            .put_int(crate::ipdict::DICT_FLG, flags as i64)
            .put_str(crate::ipdict::DICT_BODY, body);
        d
    }

    #[test]
    fn resolve_ipdict_packet_preserves_numeric_body_and_flags() {
        let flags = crate::protocol::opt::SENDCHECKOPT
            | crate::protocol::opt::SECRETOPT
            | crate::protocol::opt::ENCRYPTOPT
            | crate::protocol::opt::UTF8OPT;
        let wire = official_sendmsg_dict("1111111", flags).pack();
        let (parsed, _) = crate::ipdict::Dict::unpack(&wire).unwrap();
        let packet = super::resolve_ipdict_packet(&parsed).unwrap();

        assert_eq!(packet.pkt_no, 665500);
        assert_eq!(packet.extra, b"1111111");
        assert_eq!(packet.command, crate::protocol::cmd::SENDMSG | flags);
    }

    #[test]
    fn resolve_ipdict_packet_rejects_missing_required_fields() {
        for missing in ["VER", "PKT", "UID", "HID", "CMD", "FLG"] {
            let mut d = official_sendmsg_dict("body", 0);
            d.items.retain(|(key, _)| key != missing);
            assert!(
                super::resolve_ipdict_packet(&d).is_err(),
                "missing {missing}"
            );
        }
    }

    #[test]
    fn resolve_ipdict_packet_rejects_file_without_id_or_name() {
        for missing in [crate::ipdict::DICT_FID, crate::ipdict::DICT_FNAME] {
            let mut file = crate::ipdict::Dict::new();
            file.put_int(crate::ipdict::DICT_FID, 7)
                .put_str(crate::ipdict::DICT_FNAME, "report.txt");
            file.items.retain(|(key, _)| key != missing);
            let mut d = official_sendmsg_dict("body", crate::protocol::opt::FILEATTACHOPT);
            d.put_dict_list(crate::ipdict::DICT_FILE, &[file]);

            assert!(
                super::resolve_ipdict_packet(&d).is_err(),
                "missing {missing}"
            );
        }
    }

    #[test]
    fn resolve_ipdict_packet_rejects_malformed_or_empty_file_list() {
        for raw in [&b"not-a-list"[..], &b""[..]] {
            let mut d = official_sendmsg_dict("body", crate::protocol::opt::FILEATTACHOPT);
            d.put_bytes(crate::ipdict::DICT_FILE, raw);

            assert!(
                super::resolve_ipdict_packet(&d).is_err(),
                "FILE raw={raw:?}"
            );
        }
    }

    #[tokio::test]
    async fn encipdict_missing_signature_has_no_side_effects() {
        let (ctx, st, peer, data_dir) = encipdict_test_ctx("unsigned").await;
        let receiver = st.own_keypair();
        let inner = official_sendmsg_dict("unsigned", crate::protocol::opt::SENDCHECKOPT);
        let outer = seal_raw_ipdict(&receiver.public_key(), &inner);
        let from = peer.local_addr().unwrap();

        super::handle_encipdict(&ctx, &outer, from).await;

        assert!(st.find_in_record(&from.ip().to_string(), 665500).is_none());
        assert!(st.peer_pubkey(&from.ip().to_string()).is_none());
        assert!(peer.recv_from(&mut [0u8; 2048]).is_err(), "不能回 ACK");
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn encipdict_invalid_signature_has_no_side_effects() {
        let (ctx, st, peer, data_dir) = encipdict_test_ctx("bad-signature").await;
        let receiver = st.own_keypair();
        let sender = crate::crypto::KeyPair::generate().unwrap();
        let mut inner = official_sendmsg_dict("signed", crate::protocol::opt::SENDCHECKOPT);
        crate::crypto::sign_ipdict(&mut inner, &sender, crate::crypto::CAPA_OUR_SEND).unwrap();
        inner.put_str(crate::ipdict::DICT_BODY, "tampered");
        let outer = seal_raw_ipdict(&receiver.public_key(), &inner);
        let from = peer.local_addr().unwrap();

        super::handle_encipdict(&ctx, &outer, from).await;

        assert!(st.find_in_record(&from.ip().to_string(), 665500).is_none());
        assert!(st.peer_pubkey(&from.ip().to_string()).is_none());
        assert!(peer.recv_from(&mut [0u8; 2048]).is_err(), "不能回 ACK");
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn encipdict_persists_before_ack_and_dedups_by_inner_packet() {
        let (ctx, st, peer, data_dir) = encipdict_test_ctx("ack-order").await;
        let receiver = st.own_keypair();
        let sender = crate::crypto::KeyPair::generate().unwrap();
        let flags = crate::protocol::opt::SENDCHECKOPT
            | crate::protocol::opt::SECRETOPT
            | crate::protocol::opt::ENCRYPTOPT
            | crate::protocol::opt::UTF8OPT;
        let inner = official_sendmsg_dict("signed-body", flags);
        let outer = crate::crypto::seal_encipdict(&receiver.public_key(), &sender, &inner).unwrap();
        let from = peer.local_addr().unwrap();
        let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observations_in_event = observations.clone();
        let st_in_event = st.clone();
        let peer_in_event = peer.clone();
        st.set_event(Box::new(move |event, _| {
            if event == "msg-in" {
                let persisted = st_in_event
                    .find_in_record(&from.ip().to_string(), 665500)
                    .is_some();
                let ack_absent = peer_in_event.recv_from(&mut [0u8; 2048]).is_err();
                observations_in_event
                    .lock()
                    .unwrap()
                    .push((persisted, ack_absent));
            }
        }));

        super::handle_encipdict(&ctx, &outer, from).await;

        assert_eq!(&*observations.lock().unwrap(), &[(true, true)]);
        assert!(st.peer_pubkey(&from.ip().to_string()).is_some());
        let mut ack = [0u8; 2048];
        let (len, _) = peer.recv_from(&mut ack).expect("ACK timeout");
        let ack = crate::protocol::parse(&ack[..len]).unwrap();
        assert_eq!(ack.command & 0xff, crate::protocol::cmd::RECVMSG);
        assert_eq!(ack.extra, b"665500");

        super::handle_encipdict(&ctx, &outer, from).await;

        assert_eq!(
            observations.lock().unwrap().len(),
            1,
            "相同 inner PKT 只持久化和 emit 一次"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn handle_datagram_dispatches_full_ip2_encbody() {
        let (ctx, st, peer, data_dir) = encipdict_test_ctx("dispatch").await;
        let receiver = st.own_keypair();
        let sender = crate::crypto::KeyPair::generate().unwrap();
        let inner = official_sendmsg_dict("official", 0);
        let outer = crate::crypto::seal_encipdict(&receiver.public_key(), &sender, &inner).unwrap();
        let from = peer.local_addr().unwrap();

        super::handle_datagram(&ctx, &outer.pack(), from).await;

        assert!(st.find_in_record(&from.ip().to_string(), 665500).is_some());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn ipdict_datagram_accepts_exact_official_retry_padding() {
        let mut d = crate::ipdict::Dict::new();
        d.put_int(crate::ipdict::DICT_EF, crate::ipdict::ENCIPDICT_EF);
        let wire = d.pack();

        let (_, pad0) = super::parse_ipdict_datagram(&wire).unwrap().unwrap();
        assert_eq!(pad0, 0);

        let mut retry = wire;
        retry.extend_from_slice(&[0; 64]);
        let (_, pad64) = super::parse_ipdict_datagram(&retry).unwrap().unwrap();
        assert_eq!(pad64, 64);
    }

    #[test]
    fn ipdict_datagram_rejects_partial_or_wrong_suffix_without_classic_fallback() {
        let wire = crate::ipdict::Dict::new().put_int("A", 1).pack();
        for suffix in [&[0u8; 63][..], &[0u8; 65][..], &[1u8][..]] {
            let mut bad = wire.clone();
            bad.extend_from_slice(suffix);
            assert!(super::parse_ipdict_datagram(&bad).is_err());
        }
        assert!(super::parse_ipdict_datagram(b"IP2:5:bad:Z").is_err());
    }

    #[test]
    fn ipdict_datagram_leaves_classic_packets_untouched() {
        let classic = b"1:42:user:host:32:hello";
        assert!(super::parse_ipdict_datagram(classic).unwrap().is_none());
        assert_eq!(crate::protocol::parse(classic).unwrap().extra, b"hello");
    }

    #[test]
    fn poll_networks_accept_a_single_nadrs_dict() {
        let mut network = crate::ipdict::Dict::new();
        network
            .put_str(crate::ipdict::DICT_ADDR, "192.168.10.0")
            .put_int(crate::ipdict::DICT_MASK, 24);
        let mut poll = crate::ipdict::Dict::new();
        poll.put_dict(crate::ipdict::DICT_NADDRS, &network);

        let networks = poll_networks(&poll);

        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].get_str(crate::ipdict::DICT_ADDR), Some("192.168.10.0"));
        assert_eq!(networks[0].get_int(crate::ipdict::DICT_MASK), Some(24));
    }

    #[test]
    fn peer_addr_uses_ip_and_latest_port() {
        let p = PeerInfo {
            key: "10.0.0.3".into(),
            ip: "10.0.0.3".into(),
            port: 50000,
            nickname: String::new(),
            group: String::new(),
            host: String::new(),
            user: String::new(),
            last_seen: 0,
            absence: false,
            absence_text: None,
            vs: None,
        };
        assert_eq!(
            peer_addr(&p).expect("addr").to_string(),
            "10.0.0.3:50000",
            "投递地址 = 对端 IP + 最新一次报文的源端口，而不是假定对端监听固定端口"
        );
    }

    #[test]
    fn stat_file_entries_checks_and_annotates_paths() {
        let dir = std::env::temp_dir().join(format!("oim-stat-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 8]).unwrap();

        let entries = stat_file_entries(&[
            dir.join("a.txt").to_string_lossy().into_owned(),
            dir.join("sub").to_string_lossy().into_owned(),
        ])
        .expect("存在即通过");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 5);
        assert_eq!(entries[0].attr & 0xFF, fileattr::REGULAR);
        assert_eq!(entries[1].name, "sub");
        assert_eq!(entries[1].size, 8, "目录体积 = 递归字节总数");
        assert_eq!(entries[1].attr & 0xFF, fileattr::DIR);

        // 不存在的路径必须报错（离线入队/重投前都会先校验）
        assert!(stat_file_entries(&[dir.join("missing").to_string_lossy().into_owned()]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn id_candidates_both_bases() {
        assert_eq!(id_candidates("10"), vec![10, 16]); // 十进制优先，十六进制兜底
        assert_eq!(id_candidates("a"), vec![10]); // 纯十六进制
        assert_eq!(id_candidates("1"), vec![1]); // 两进制同值去重
        assert_eq!(id_candidates("0x1f"), vec![31]);
        assert!(id_candidates("").is_empty());
        assert!(id_candidates("zz").is_empty());
    }

    #[test]
    fn entry_caps_follow_switch() {
        let mut cfg = crate::state::Config::default();
        // FILEATTACHOPT|CLIPBOARDOPT 恒声明（TCP 服务可达即声明，与官方 HostStatus
        // 一致）；加密位随开关。CLIPBOARDOPT 是官方发送「粘贴图片」附件的先决条件
        //（senddlg.cpp SendMsgSetUsers 对方无此位 → 撤销 FILEATTACHOPT）
        assert_eq!(
            super::entry_caps(&cfg),
            opt::ENCRYPTOPT
                | opt::CAPFILEENCOPT
                | opt::ENCEXTMSGOPT
                | opt::FILEATTACHOPT
                | opt::CLIPBOARDOPT
                | opt::CAPIPDICTOPT
        );
        cfg.encrypt = false;
        assert_eq!(
            super::entry_caps(&cfg),
            opt::FILEATTACHOPT | opt::CLIPBOARDOPT | opt::CAPIPDICTOPT
        );
        // 成员主模式额外声明 DIR_MASTER|DIALUPOPT（官方 HostStatus 同款）
        cfg.dir_mode = "master".into();
        assert!(super::entry_caps(&cfg) & opt::DIR_MASTER != 0);
        assert!(super::entry_caps(&cfg) & opt::DIALUPOPT != 0);
    }

    /// 线上常量钉死：官方 ipmsg.h L119 ENCFILEOPT=0x800（与 MULTICASTOPT
    /// 按命令类别复用）。曾误写 0x20000000（SIGN_SHA1 位）导致真机互通必败。
    #[test]
    fn encfileopt_matches_official_wire_value() {
        assert_eq!(opt::ENCFILEOPT, 0x0000_0800);
    }

    #[test]
    fn plain_payload_appends_single_nul() {
        assert_eq!(super::plain_payload(b"hi"), b"hi\0");
        // 文件公告尾部 \a 分隔符（Rust 字节串写作 \x07）之后补且仅补一个 \0
        assert_eq!(
            super::plain_payload(b"a\0f.zip:1:1:1:\x07"),
            b"a\0f.zip:1:1:1:\x07\0"
        );
    }

    #[test]
    fn split_short_text_stays_single() {
        assert_eq!(super::split_text_for_seal("你好 world", "utf8"), None);
        assert_eq!(super::split_text_for_seal("", "gbk"), None);
        // 恰好占满预算（单字节字符）也不拆
        assert_eq!(
            super::split_text_for_seal(&"a".repeat(super::CHUNK_PLAIN_BUDGET), "utf8"),
            None
        );
    }

    #[test]
    fn split_long_text_preserves_content_and_budget() {
        // 🌍=4B + 密=3B + a=1B + b=1B + c=1B → 10B/组 × 800 = 8000B > 3300
        let s = "🌍密abc".repeat(800);
        let chunks =
            super::split_text_for_seal(&s, "utf8").expect("超预算应自动分段");
        assert!(chunks.len() >= 2, "应拆成至少两段");
        assert_eq!(chunks.concat(), s, "分段拼接必须完整还原原文");
        for c in &chunks {
            assert!(
                proto::encode_out(c, "utf8").len() <= super::CHUNK_PLAIN_BUDGET,
                "每段编码后不得超过预算"
            );
        }
    }

    #[test]
    fn split_never_splits_multi_byte_char() {
        // 中文字符 3B：3300 不是 3 的倍数，切分不能劈开汉字
        let s = "密".repeat(1200); // 3600B > 3300
        let chunks = super::split_text_for_seal(&s, "utf8").expect("应分段");
        assert_eq!(chunks.concat(), s);
        assert!(
            chunks.iter().all(|c| c.chars().all(|ch| ch == '密')),
            "禁止从多字节字符中间切开"
        );
    }

    #[test]
    fn split_gbk_uses_encoded_byte_budget() {
        // GBK 下汉字 2B/字：4000B > 3300，须按 GBK 字节数切分
        let s = "密".repeat(2000);
        let chunks = super::split_text_for_seal(&s, "gbk").expect("应分段");
        assert!(chunks.len() >= 2);
        assert_eq!(chunks.concat(), s);
        for c in &chunks {
            assert!(proto::encode_out(c, "gbk").len() <= super::CHUNK_PLAIN_BUDGET);
        }
    }

    #[test]
    fn chunk_by_budget_respects_custom_budget() {
        // 逐字符按预算切分、不劈字符
        assert_eq!(
            super::chunk_by_budget("abcd", "utf8", 3),
            vec!["abc".to_string(), "d".to_string()]
        );
        // 中文 3B：预算 3 → 每段恰一字
        assert_eq!(
            super::chunk_by_budget("密密密", "utf8", 3),
            vec!["密".to_string(), "密".to_string(), "密".to_string()]
        );
        // 装得下 → 单段
        assert_eq!(super::chunk_by_budget("你好", "gbk", 4), vec!["你好".to_string()]);
        // 空文本 → 空数组
        assert!(super::chunk_by_budget("", "utf8", 3300).is_empty());
    }

    #[test]
    fn entries_wire_bytes_upper_counts_serialize_plus_sep() {
        let e = proto::FileEntry {
            id: 0,
            raw_id: String::new(),
            name: "a.txt".into(),
            size: 0,
            mtime: 0,
            attr: 0,
            ext_attrs: vec![],
        };
        // id 按 10 位十进制占位：1000000000:a.txt:0:0:0: = 23B + 1 分隔符
        assert_eq!(super::entries_wire_bytes_upper(&[e.clone()], "utf8"), 24);
        assert_eq!(
            super::entries_wire_bytes_upper(&[e.clone(), e], "utf8"),
            48
        );
    }

    #[test]
    fn attachment_text_budget_accounts_overhead() {
        // 无条目：3300 − 3 分隔符 − 48 尾注余量
        assert_eq!(super::attachment_text_budget(&[], "utf8"), 3300 - 51);
        let e = proto::FileEntry {
            id: 0,
            raw_id: String::new(),
            name: "a.txt".into(),
            size: 0,
            mtime: 0,
            attr: 0,
            ext_attrs: vec![],
        };
        assert_eq!(
            super::attachment_text_budget(&[e], "utf8"),
            3300 - 51 - 24
        );
    }

    #[test]
    fn attachment_text_budget_saturates_when_entries_oversize() {
        let e = proto::FileEntry {
            id: 0,
            raw_id: String::new(),
            name: "x".repeat(100_000),
            size: 0,
            mtime: 0,
            attr: 0,
            ext_attrs: vec![],
        };
        assert_eq!(super::attachment_text_budget(&[e], "utf8"), 0);
    }

    #[test]
    fn clipboard_image_staging() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("oim-clip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = b"\x89PNG\r\n\x1a\n-fake-image-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);

        let p1 = stage_clipboard_image(&dir, &b64, "image/png").expect("stage");
        assert_eq!(std::fs::read(&p1).unwrap(), png);
        assert_eq!(p1.extension().unwrap(), "png");
        // 扩展名要能被内联预览识别，否则对端收到也不会直接显示
        assert!(is_image_name(&p1.file_name().unwrap().to_string_lossy()));

        // 同一秒内连续粘贴不能互相覆盖
        let p2 = stage_clipboard_image(&dir, &b64, "image/png").expect("stage 2");
        assert_ne!(p1, p2);
        assert!(p1.exists() && p2.exists());

        assert!(stage_clipboard_image(&dir, &b64, "image/tiff").is_err());
        assert!(stage_clipboard_image(&dir, "", "image/png").is_err());
        assert!(stage_clipboard_image(&dir, "@@not-base64@@", "image/png").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clipboard_file_staging_sanitizes_name() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("oim-clipfile-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"hello");

        let p = stage_clipboard_file(&dir, "报告.docx", &b64).expect("stage");
        assert_eq!(p.file_name().unwrap(), "报告.docx");
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
        // 同名再粘一次不能覆盖
        let p2 = stage_clipboard_file(&dir, "报告.docx", &b64).expect("stage 2");
        assert_ne!(p, p2);
        assert!(p.exists() && p2.exists());

        // 路径穿越必须被挡住：只取最后一段，且不能落到缓存目录之外
        let evil = stage_clipboard_file(&dir, "../../evil.sh", &b64).expect("stage evil");
        assert_eq!(evil.file_name().unwrap(), "evil.sh");
        assert!(evil.starts_with(dir.join("剪贴板文件")));
        let evil2 = stage_clipboard_file(&dir, "..", &b64).expect("stage dots");
        assert!(evil2.file_name().unwrap().to_string_lossy().starts_with("粘贴文件_"));

        assert!(stage_clipboard_file(&dir, "a.bin", "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clipboard_stamp_format() {
        let s = clipboard_stamp();
        assert_eq!(s.len(), 15, "yyyymmdd-hhmmss");
        assert_eq!(&s[8..9], "-");
        assert!(s.chars().filter(|c| c.is_ascii_digit()).count() == 14);
        assert!(s.starts_with("20"));
    }

    #[test]
    fn dir_header_length_is_self_inclusive() {
        let h = dir_header("a.txt", 0x1234, dirattr::REGULAR);
        assert_eq!(h, b"0012:a.txt:1234:1:".to_vec());
        assert_eq!(
            usize::from_str_radix(std::str::from_utf8(&h[..4]).unwrap(), 16).unwrap(),
            h.len()
        );
        // 超长名字导致长度字段扩位时仍自洽
        let long = "x".repeat(0x10000);
        let h = dir_header(&long, 0, dirattr::ENTER);
        let colon = h.iter().position(|&b| b == b':').unwrap();
        let n = usize::from_str_radix(std::str::from_utf8(&h[..colon]).unwrap(), 16).unwrap();
        assert_eq!(n, h.len());
    }

    #[test]
    fn safe_component_blocks_traversal() {
        assert_eq!(safe_component("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_component(".."), "unnamed");
        assert_eq!(safe_component("  "), "unnamed");
        assert_eq!(safe_component("C:\\win\\evil"), "C__win_evil");
        assert_eq!(safe_component("正常.txt"), "正常.txt");
    }

    #[test]
    fn num_flex_dec_first_prefers_decimal() {
        assert_eq!(num_flex_dec_first("10"), Some(10));
        assert_eq!(num_flex_dec_first("0x1f"), Some(31));
        assert_eq!(num_flex_dec_first("ff"), Some(255));
    }

    /// spec §2.3 钉死：密封内层的续传偏移是 **十进制** `<offset_dec>`。
    /// 777 的十六进制是 309，全数字 —— 若误用 {offset:x} 渲染成 ":309:"，
    /// 我方服务端 num_flex_dec_first 会按十进制读成 309，静默从错误位置续传。
    #[test]
    fn file_request_inner_offset_is_decimal() {
        let key_hex = "00".repeat(32);
        let inner = file_request_inner("1f", "2a", 777, &key_hex);
        assert!(
            inner.contains(":777:"),
            "偏移必须按十进制渲染（spec §2.3 offset_dec），实际内层：{inner}"
        );
        assert!(!inner.contains(":309:"), "不得按十六进制渲染偏移：{inner}");
        assert_eq!(inner, format!("1f:2a:777:900000:{key_hex}"));
    }

    /// 无续传（offset=0）时内层不携带偏移段 —— 保持既有线上格式
    #[test]
    fn file_request_inner_omits_offset_when_zero() {
        let inner = file_request_inner("1f", "2a", 0, &"ab".repeat(32));
        assert_eq!(inner, format!("1f:2a:900000:{}", "ab".repeat(32)));
    }
}

/* ================= TCP 文件传输 ================= */

fn spawn_tcp_server(ctx: Arc<NetCtx>) {
    tokio::spawn(async move {
        let listener = match TcpListener::bind(("0.0.0.0", ctx.port)).await {
            Ok(l) => l,
            Err(e) => {
                oim_log!("[tcp] bind {} failed: {e}", ctx.port);
                return;
            }
        };
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let ctx2 = ctx.clone();
                    tokio::spawn(async move {
                        serve_getfile(&ctx2, stream, peer).await;
                    });
                }
                Err(e) => {
                    oim_log!("[tcp] accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
    });
}

/// 服务端：解析 GETFILEDATA 请求并回传文件字节流
async fn serve_getfile(ctx: &NetCtx, mut stream: TcpStream, peer: std::net::SocketAddr) {
    // 读请求行：容忍有无结尾换行。上限按加密封包预算放宽（spec §2：
    // MAX_ENCRYPTED_PACKET=8000）——加密的取文件请求扩展部约 1.2KB，
    // 明文请求几十字节就完成解析，不会多等。
    //
    // 完整性判定（2026-08 现场故障根因）：proto::parse 只校验头部五个冒号，
    // 扩展部截断也会"解析成功"。加密请求 ~1.3KB 在真实网络上被 TCP 拆包后，
    // 服务端把截断的密封串当完整请求处理 → 解封失败 → 静默断开 → 对端 10054
    // （环回单包送达故自检永不复现；明文请求过短也永远安全）。
    // 因此：解析成功后若带 '\n'（我方加密/新方言请求恒以换行收尾）→ 立即
    // 视为完整；否则进入 80ms 安静确认期，仍无新数据才确定完整（兼容无
    // 换行的老明文方言）；总时限 3s 兜底网络慢/重传。
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = vec![0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut req_pkt = None;
    loop {
        if buf.len() >= crypto::MAX_ENCRYPTED_PACKET || Instant::now() > deadline {
            break;
        }
        // 已解析成功：带 \n 立即完成；否则给一小段安静期确认没有后续分段
        if req_pkt.is_some() {
            if buf.ends_with(b"\n") {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(80), stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Ok(Ok(n)) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = proto::parse(&buf) {
                        req_pkt = Some(p);
                    }
                    continue;
                }
                Err(_) => break, // 安静期无新数据：请求完整
            }
        }
        match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = proto::parse(&buf) {
                    req_pkt = Some(p);
                }
            }
            Ok(_) => break, // EOF / 读错误
            Err(_) => {
                if req_pkt.is_some() {
                    break; // 总时间内头部已齐且无更多数据
                }
                // 头部未齐：继续等待剩余分段（总时限兜底）
            }
        }
    }
    let Some(req) = req_pkt else { return };
    let base = req.command & 0xFF;
    if base != cmd::GETFILEDATA && base != cmd::GETDIRFILES {
        return;
    }
    // extra: pkt_id:file_id[:offset]（GETDIRFILES 无断点续传，offset 可缺省）。
    // 带 ENCRYPTOPT 时扩展部是密封的取文件请求（spec §7）：先解封再取字段，
    // 槽位查找放在解封之后 —— 未解开的请求连槽位都不该探到。
    // 加密请求是方言无关的：内层数字按官方约定写十六进制；
    // id_candidates 十进制/十六进制都试，保持既有宽容度。
    let mut ctr_key: Option<[u8; 32]> = None;
    let parts: Vec<String>;
    if req.command & opt::ENCRYPTOPT != 0 {
        let inner = match crypto::open_file_request(
            &ctx.st.own_keypair(),
            &String::from_utf8_lossy(&req.extra),
            req.pkt_no,
        ) {
            Ok((inner, true)) => {
                // enc_body：内层倒数第二段恒为 900000，末段是 64 位 hex 的 AES-256 钥
                let segs: Vec<&str> = inner.split(':').collect();
                match crypto::hex_decode_loose(segs[segs.len() - 1]) {
                    Some(k) if k.len() == 32 => {
                        ctr_key = Some(k.try_into().expect("长度已在上一行校验为 32"));
                    }
                    _ => {
                        ctx.st.diag("tcp-enc-bad-key 文件请求的 CTR 钥不是 32 字节，断开");
                        return;
                    }
                }
                inner
            }
            Ok((inner, false)) => inner, // NOENC_FILEBODY：验证通过，回明文流
            Err(e) => {
                // 解不开的请求直接断开不给任何反馈：可能是敌意探测，也可能
                // 是对方还持着已被我们撤换的旧公钥
                oim_log!("[tcp] {peer} 加密取文件请求解封失败: {e}");
                ctx.st.diag(&format!("tcp-enc-open-fail {peer}: {e}"));
                return;
            }
        };
        // Task 10 自检依赖此标记确认加密路径被真实执行（而非回退明文）。
        // enc=1 表示正文确定走密钥流；绝不记录内层原文 —— 它的末段就是本次
        // 会话的 CTR 钥。
        ctx.st.diag(&format!("tcp-hit enc={} {peer}", u8::from(ctr_key.is_some())));
        let segs: Vec<String> = inner.split(':').map(|s| s.trim().to_string()).collect();
        // 掐掉尾部的加密参数段（enc: "900000"+"key" 两段；noenc: "4000000" 一段），
        // 剩下的才是 pkt:id[:offset]
        let body_len = if ctr_key.is_some() {
            segs.len().saturating_sub(2)
        } else {
            segs.len().saturating_sub(1)
        };
        parts = segs[..body_len.min(segs.len())].to_vec();
    } else {
        parts = String::from_utf8_lossy(&req.extra)
            .split(':')
            .map(|s| s.trim().to_string())
            .collect();
    }
    if parts.len() < 2 {
        return;
    }
    let offset = parts.get(2).and_then(|s| num_flex_dec_first(s)).unwrap_or(0);

    // 各客户端对 ID 字段的进制约定不一（官方十六进制、部分实现十进制），
    // 对两种解释都尝试匹配，最大化兼容
    let pkt_cands = id_candidates(&parts[0]);
    let fid_cands = id_candidates(&parts[1]);
    let mut slot: Option<(PathBuf, u64, bool)> = None;
    {
        let offered = ctx.st.offered.lock().unwrap();
        'outer: for p in &pkt_cands {
            for f in &fid_cands {
                if let Some(o) = offered.get(&(*p, *f)) {
                    slot = Some((o.path.clone(), o.size, o.is_dir));
                    break 'outer;
                }
            }
        }
    }
    let Some((path, size, is_dir)) = slot else {
        oim_log!(
            "[tcp] GETFILEDATA 未命中: from={peer} extra={:?} 候选pkt={pkt_cands:?} 候选id={fid_cands:?} 已提供={:?}",
            req.extra,
            ctx.st.offered.lock().unwrap().keys().take(8).collect::<Vec<_>>()
        );
        ctx.st.diag(&format!(
            "tcp-miss {peer} extra={:?} pkt候选={pkt_cands:?} id候选={fid_cands:?}",
            String::from_utf8_lossy(&req.extra)
        ));
        return;
    };
    let fname = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    oim_log!("[tcp] {peer} 请求{} {fname} (offset {offset})", if is_dir { "目录" } else { "文件" });
    ctx.st.diag(&format!("tcp-hit {peer} file={fname} dir={is_dir} offset={offset}"));

    if is_dir {
        // 目录必须走 GETDIRFILES 流；对端若误用 GETFILEDATA 则无法解析，直接断开
        if base != cmd::GETDIRFILES {
            ctx.st
                .diag(&format!("tcp-dir-wrong-cmd {peer} file={fname} cmd={:#x}", req.command));
            return;
        }
        // 整条目录流（头部+内容）统一过密钥流：包装在 writer 抽象上，
        // serve_dir_stream 对加密与否无感知。目录无断点续传，密钥流从流头起步
        // （显式传 0：GETDIRFILES 请求内层不含偏移）。
        let mut w: Box<dyn AsyncWrite + Unpin + Send> = match ctr_key {
            Some(k) => Box::new(crypto::EncStream::new(stream, &k, req.pkt_no, 0)),
            None => Box::new(stream),
        };
        let sent = serve_dir_stream(&mut w, &path, &fname).await;
        let _ = w.flush().await;
        let _ = w.shutdown().await;
        oim_log!("[tcp] 已向 {peer} 发送目录 {fname}: {sent} 字节");
        ctx.st.diag(&format!("tcp-sent-dir {peer} dir={fname} bytes={sent} enc={}", ctr_key.is_some()));
        return;
    }
    if base == cmd::GETDIRFILES {
        // 普通文件被按目录请求：拒绝，避免对端解析出乱七八糟的目录树
        ctx.st.diag(&format!("tcp-file-wrong-cmd {peer} file={fname}"));
        return;
    }

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            ctx.st
                .diag(&format!("tcp-open-fail {}: {e}", path.display()));
            return;
        }
    };
    if offset > 0 {
        if offset >= size {
            // 断点续传请求越界：直接关连接，别把整个文件重发一遍
            ctx.st
                .diag(&format!("tcp-bad-offset {peer} file={fname} offset={offset} size={size}"));
            return;
        }
        if file.seek(io::SeekFrom::Start(offset)).await.is_err() {
            return;
        }
    }
    // 正文写出统一走 writer 抽象：加密请求时整条流过 AES-CTR 密钥流，
    // 密钥流位置 = 文件绝对偏移（EncStream::new 内部 seek），与对端读端对齐。
    let mut w: Box<dyn AsyncWrite + Unpin + Send> = match ctr_key {
        Some(k) => Box::new(crypto::EncStream::new(stream, &k, req.pkt_no, offset)),
        None => Box::new(stream),
    };
    // 只发公告时声明的字节数：文件在公告后被追加写入时，多发的部分会让
    // 对端按大小校验失败
    let mut remain = size.saturating_sub(offset);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut sent: u64 = 0;
    while remain > 0 {
        let want = (remain as usize).min(chunk.len());
        match file.read(&mut chunk[..want]).await {
            Ok(0) => break,
            Ok(n) => {
                if w.write_all(&chunk[..n]).await.is_err() {
                    break;
                }
                sent += n as u64;
                remain -= n as u64;
            }
            Err(_) => break,
        }
    }
    let _ = w.flush().await;
    // 主动关闭写端：对端据此判断传输结束
    let _ = w.shutdown().await;
    oim_log!("[tcp] 已向 {peer} 发送 {fname}: {sent} 字节");
    ctx.st.diag(&format!("tcp-sent {peer} file={fname} bytes={sent}"));
}

/* ---------- 目录传输（GETDIRFILES 流） ----------

官方格式：头部 `<头部长度16进制>:<名称>:<大小16进制>:<属性16进制>:`，
紧跟 <大小> 字节的内容。属性取值：1=普通文件，2=进入目录，3=返回上级
（此时名称固定为 "."，无内容）。目录树按深度优先展开。 */

/// 目录流内的条目属性
mod dirattr {
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

/// 一次目录传输最多接收的条目数，防御异常/恶意对端把磁盘写满
const DIR_MAX_ENTRIES: usize = 50_000;

/// 目录递归遍历上限，防御符号链接环与超深目录
const DIR_MAX_DEPTH: usize = 64;

/// 目录递归总字节数（用于公告体积/进度分母）
fn dir_total_size(root: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path, depth: usize, acc: &mut u64) {
        if depth > DIR_MAX_DEPTH {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for ent in rd.flatten() {
            // 不跟随符号链接（symlink_metadata）
            let Ok(meta) = ent.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                walk(&ent.path(), depth + 1, acc);
            } else if meta.is_file() {
                *acc += meta.len();
            }
        }
    }
    let mut acc = 0;
    walk(root, 0, &mut acc);
    acc
}

/// 目录流的一个操作
enum DirOp {
    Enter(String),
    File(PathBuf, String, u64),
    Ret,
}

/// 深度优先展开目录树为流操作序列
fn collect_dir_ops(root: &std::path::Path, root_name: &str) -> Vec<DirOp> {
    fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<DirOp>) {
        if depth > DIR_MAX_DEPTH {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for ent in items {
            let Ok(meta) = ent.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if meta.is_dir() {
                out.push(DirOp::Enter(name));
                walk(&ent.path(), depth + 1, out);
                out.push(DirOp::Ret);
            } else if meta.is_file() {
                out.push(DirOp::File(ent.path(), name, meta.len()));
            }
        }
    }
    let mut out = vec![DirOp::Enter(root_name.to_string())];
    walk(root, 0, &mut out);
    out.push(DirOp::Ret);
    out
}

/// 组装目录流头部（长度字段自身也计入总长）
fn dir_header(name: &str, size: u64, attr: u32) -> Vec<u8> {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | '\0' | '\r' | '\n' => '_',
            _ => c,
        })
        .collect();
    let body = format!(":{cleaned}:{size:x}:{attr:x}:");
    // 官方用 4 位十六进制；超长文件名时自然扩展位数并迭代收敛
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

/// 服务端：把目录树按流发给对端，返回发出的内容字节数。
/// writer 用泛型抽象：明文传 TcpStream，加密时传 EncStream<TcpStream>，
/// 整条流（头部+内容）无差别过密钥流。
async fn serve_dir_stream<W: AsyncWrite + Unpin>(
    w: &mut W,
    root: &std::path::Path,
    root_name: &str,
) -> u64 {
    let mut sent = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    for op in collect_dir_ops(root, root_name) {
        match op {
            DirOp::Enter(name) => {
                if w.write_all(&dir_header(&name, 0, dirattr::ENTER)).await.is_err() {
                    return sent;
                }
            }
            DirOp::Ret => {
                if w.write_all(&dir_header(".", 0, dirattr::RETPARENT)).await.is_err() {
                    return sent;
                }
            }
            DirOp::File(path, name, size) => {
                // 以登记时的大小为准：发送期间文件被改写也要保证头部与内容一致
                if w.write_all(&dir_header(&name, size, dirattr::REGULAR)).await.is_err() {
                    return sent;
                }
                let Ok(mut f) = tokio::fs::File::open(&path).await else {
                    // 打开失败：内容按 0 字节补齐，流结构不能错位
                    if !write_zeros(w, size, &mut chunk).await {
                        return sent;
                    }
                    sent += size;
                    continue;
                };
                let mut remain = size;
                while remain > 0 {
                    let want = (remain as usize).min(chunk.len());
                    match f.read(&mut chunk[..want]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if w.write_all(&chunk[..n]).await.is_err() {
                                return sent;
                            }
                            sent += n as u64;
                            remain -= n as u64;
                        }
                        Err(_) => break,
                    }
                }
                // 文件变短：补零到头部声明的长度，避免对端错位解析后续条目
                if remain > 0 {
                    if !write_zeros(w, remain, &mut chunk).await {
                        return sent;
                    }
                    sent += remain;
                }
            }
        }
    }
    sent
}

async fn write_zeros<W: AsyncWrite + Unpin>(w: &mut W, mut n: u64, buf: &mut [u8]) -> bool {
    for b in buf.iter_mut() {
        *b = 0;
    }
    while n > 0 {
        let wr = (n as usize).min(buf.len());
        if w.write_all(&buf[..wr]).await.is_err() {
            return false;
        }
        n -= wr as u64;
    }
    true
}

fn num_flex_dec_first(t: &str) -> Option<u64> {
    let t = t.trim();
    if let Ok(v) = t.parse::<u64>() {
        return Some(v);
    }
    u64::from_str_radix(t.trim_start_matches("0x"), 16).ok()
}

/// ID 字段的所有可能数值解释：十进制与十六进制（去重）
fn id_candidates(s: &str) -> Vec<u32> {
    let t = s.trim();
    let mut out = Vec::new();
    if t.is_empty() {
        return out;
    }
    if let Ok(d) = t.parse::<u32>() {
        out.push(d);
    }
    if let Ok(h) = u32::from_str_radix(t.trim_start_matches("0x").trim_start_matches("0X"), 16) {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// 客户端：从对端下载一个附件到配置的接收目录（后台任务，进度走事件）。
/// `rid` 为对端公告中的原始 ID 字符串（进制不明，必须原样回传）；
/// 为空时回退用 file_id 的十六进制形式（兼容旧记录）。
/// `expect_size` 为公告中的字节数，用于进度百分比与截断校验（0 表示未知）。
#[allow(clippy::too_many_arguments)]
pub async fn download_file_task(
    ctx: &NetCtx,
    key: &str,
    pkt_no: u32,
    file_id: u32,
    name: &str,
    rid: &str,
    expect_size: u64,
    is_dir: bool,
) -> Result<PathBuf, String> {
    let peer = ctx
        .st
        .peers
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or_else(|| "对方不在线".to_string())?;
    let target = peer_addr(&peer).ok_or("无效的对方地址")?;

    let cfg = ctx.st.config();
    let dir = PathBuf::from(if cfg.download_dir.is_empty() {
        ctx.st.data_dir.join("接收文件").to_string_lossy().into_owned()
    } else {
        cfg.download_dir.clone()
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建接收目录失败: {e}"))?;

    let mut safe_name: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '_' } else { c })
        .collect();
    // 防御对端构造的路径穿越/空名
    if safe_name.trim().is_empty() || safe_name.trim_matches('.').is_empty() {
        safe_name = format!("file-{file_id:x}");
    }
    // 临时文件/目录与最终目标同级、独立命名（不能用 with_extension，会吃掉真实扩展名）
    let tmp_path = dir.join(format!(
        ".oim-{:x}-{}.part",
        file_id,
        DL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    if is_dir {
        return download_dir_task(
            ctx, key, target, pkt_no, file_id, rid, expect_size, &dir, &safe_name, &tmp_path,
        )
        .await;
    }

    // 对端接受连接却不回数据，多半是请求里数字字段的进制不合它的口味：
    // 换一种方言重试，命中后记住，后续下载不再多花往返。
    //
    // 例外：公告大小就是 0（空文件）——空文件的完整传输恰好也是 0 字节，
    // 与「对端不认方言」在字节层面无法区分。此时收到 0 字节就是完整成功，
    // 立即收工，不再花往返试其余方言（结果不会因方言而不同），否则 4 种
    // 方言全部返回 0 字节后会被误判成「对方未返回文件数据」而失败。
    let mut result: Result<(u64, bool), String> = Ok((0, false));
    for idx in ctx.st.dialect_order(&peer.ip, DIALECTS.len()) {
        let d = DIALECTS[idx];
        result = fetch_to_file(
            ctx, key, target, pkt_no, file_id, rid, expect_size, d, &tmp_path,
        )
        .await;
        match &result {
            Ok(_) if expect_size == 0 => break, // 空文件：0 字节即完整传输
            Ok((0, _)) => {
                ctx.st.diag(&format!(
                    "dl-try {key} pkt={pkt_no} id={file_id:x} 方言#{idx}({d:?}) -> 0 字节，换下一种"
                ));
                let _ = tokio::fs::remove_file(&tmp_path).await;
                continue;
            }
            Ok((n, _)) => {
                ctx.st.remember_dialect(&peer.ip, idx);
                ctx.st
                    .diag(&format!("dl-try {key} 方言#{idx} 命中，收到 {n} 字节"));
                break;
            }
            // 连接层面的失败换方言也没用，直接报错
            Err(_) => break,
        }
    }
    match result {
        Ok((0, _)) if expect_size > 0 => {
            // 对端接受了连接但没有回数据，而公告大小非 0：
            // 空文件已经在上面的方言循环里直接成功，走到这里必然是传输异常
            let _ = tokio::fs::remove_file(&tmp_path).await;
            ctx.st
                .diag(&format!("dl-empty {key} pkt={pkt_no} id={file_id:x}: 对端未返回数据"));
            let e = "对方未返回文件数据：文件可能已被对方撤回或过期，\
                     也可能是对方客户端不兼容（已尝试全部请求方言）"
                .to_string();
            fail_download(ctx, key, pkt_no, file_id, &e);
            Err(e)
        }
        Ok((total, _)) if expect_size > 0 && total < expect_size => {
            // 连接中途断开：落盘半截文件会静默损坏，按失败处理
            let _ = tokio::fs::remove_file(&tmp_path).await;
            let e = format!("传输中断：只收到 {total}/{expect_size} 字节");
            ctx.st
                .diag(&format!("dl-short {key} pkt={pkt_no} id={file_id:x}: {e}"));
            fail_download(ctx, key, pkt_no, file_id, &e);
            Err(e)
        }
        Ok((total, enc)) => {
            // 重名判定推迟到落盘瞬间，避免并发下载抢同一个目标名
            let final_path = unique_path(&dir.join(&safe_name));
            std::fs::rename(&tmp_path, &final_path)
                .map_err(|e| format!("保存文件失败: {e}"))?;
            ctx.st.diag(&format!(
                "dl-done {key} pkt={pkt_no} id={file_id:x} -> {} ({total}B) enc={enc}",
                final_path.display()
            ));
            ctx.st.emit(
                "file-progress",
                json!({"key": key, "pkt": pkt_no, "file_id": file_id,
                       "transferred": total, "total": total, "done": true,
                       "enc": enc,
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
            ctx.st
                .diag(&format!("dl-fail {key} pkt={pkt_no} id={file_id:x}: {e}"));
            fail_download(ctx, key, pkt_no, file_id, &e);
            Err(e)
        }
    }
}

/// 客户端：以 GETDIRFILES 流方式接收整个目录树
#[allow(clippy::too_many_arguments)]
async fn download_dir_task(
    ctx: &NetCtx,
    key: &str,
    target: SocketAddr,
    pkt_no: u32,
    file_id: u32,
    rid: &str,
    expect_size: u64,
    parent: &std::path::Path,
    safe_name: &str,
    tmp_root: &std::path::Path,
) -> Result<PathBuf, String> {
    // 与取文件同理：对端不认我方请求方言时换一种重试
    let peer_ip = target.ip().to_string();
    let mut res: Result<Option<u64>, String> = Ok(None);
    for idx in ctx.st.dialect_order(&peer_ip, DIALECTS.len()) {
        res = fetch_dir_tree(
            ctx, key, target, pkt_no, file_id, rid, expect_size, DIALECTS[idx], tmp_root,
        )
        .await;
        match &res {
            Ok(None) => {
                ctx.st.diag(&format!(
                    "dl-dir-try {key} pkt={pkt_no} id={file_id:x} 方言#{idx} -> 无数据，换下一种"
                ));
                let _ = tokio::fs::remove_dir_all(tmp_root).await;
                continue;
            }
            Ok(Some(_)) => {
                ctx.st.remember_dialect(&peer_ip, idx);
                break;
            }
            // 目录流解析出错说明对端确实在回数据，只是内容有问题，换方言无益
            Err(_) => break,
        }
    }
    match res {
        Ok(None) => {
            let _ = tokio::fs::remove_dir_all(tmp_root).await;
            let e = "对方未返回目录数据：可能已被撤回，或对方客户端不兼容\
                     （已尝试全部请求方言）"
                .to_string();
            ctx.st
                .diag(&format!("dl-dir-empty {key} pkt={pkt_no} id={file_id:x}"));
            fail_download(ctx, key, pkt_no, file_id, &e);
            Err(e)
        }
        Ok(Some(total)) => {
            // 落盘瞬间才定名，避免并发接收抢同一个目录名
            let final_path = unique_path(&parent.join(safe_name));
            std::fs::rename(tmp_root, &final_path).map_err(|e| format!("保存目录失败: {e}"))?;
            ctx.st.diag(&format!(
                "dl-dir-done {key} pkt={pkt_no} id={file_id:x} -> {} ({total}B)",
                final_path.display()
            ));
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
            let _ = tokio::fs::remove_dir_all(tmp_root).await;
            ctx.st
                .diag(&format!("dl-dir-fail {key} pkt={pkt_no} id={file_id:x}: {e}"));
            fail_download(ctx, key, pkt_no, file_id, &e);
            Err(e)
        }
    }
}

/// 目录流接收：解析头部序列并按深度重建目录树，返回写入的内容字节数
#[allow(clippy::too_many_arguments)]
async fn fetch_dir_tree(
    ctx: &NetCtx,
    key: &str,
    target: SocketAddr,
    pkt_no: u32,
    file_id: u32,
    rid: &str,
    expect_size: u64,
    d: Dialect,
    tmp_root: &std::path::Path,
) -> Result<Option<u64>, String> {
    let (mut stream, _enc) =
        open_transfer(ctx, target, pkt_no, file_id, rid, cmd::GETDIRFILES, d, 0).await?;
    std::fs::create_dir_all(tmp_root).map_err(|e| format!("创建临时目录失败: {e}"))?;

    let mut rd = StreamReader::new(stream_timeout());
    let mut cur = tmp_root.to_path_buf();
    let mut depth: usize = 0;
    let mut total = 0u64;
    let mut got_root = false;
    let mut entries = 0usize;
    let mut last_emit = Instant::now();

    loop {
        let Some(head) = rd.read_header(&mut stream).await? else {
            break; // 流正常结束
        };
        let (name, size, attr) = head;
        entries += 1;
        if entries > DIR_MAX_ENTRIES {
            return Err(format!("目录条目超过 {DIR_MAX_ENTRIES} 个，已中止"));
        }
        // 扩展位不参与类型判断（官方类型值在低 8 位）
        match attr & 0xFF {
            dirattr::ENTER => {
                if depth > DIR_MAX_DEPTH {
                    return Err("目录层级过深，已中止".into());
                }
                if !got_root {
                    // 首个 ENTER 是目录自身，直接落在临时根上，不再嵌套一层
                    got_root = true;
                    depth += 1;
                    continue;
                }
                cur = cur.join(safe_component(&name));
                depth += 1;
                std::fs::create_dir_all(&cur).map_err(|e| format!("创建子目录失败: {e}"))?;
            }
            dirattr::RETPARENT => {
                if depth == 0 {
                    return Err("目录流结构异常（多余的返回上级）".into());
                }
                depth -= 1;
                if depth == 0 {
                    break; // 根目录结束
                }
                cur = cur
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| tmp_root.to_path_buf());
                // 越界保护：任何情况下都不能爬到临时根之上
                if !cur.starts_with(tmp_root) {
                    return Err("目录流试图越出接收目录".into());
                }
            }
            dirattr::REGULAR => {
                if !got_root {
                    return Err("目录流未以目录条目开始".into());
                }
                let path = cur.join(safe_component(&name));
                let mut f = tokio::fs::File::create(&path)
                    .await
                    .map_err(|e| format!("创建文件失败: {e}"))?;
                let written = rd.copy_exact(&mut stream, &mut f, size).await?;
                f.flush().await.map_err(|e| format!("写入失败: {e}"))?;
                total += written;
                if written < size {
                    return Err(format!("传输中断：{name} 只收到 {written}/{size} 字节"));
                }
                if last_emit.elapsed() >= Duration::from_millis(150) {
                    last_emit = Instant::now();
                    ctx.st.emit(
                        "file-progress",
                        json!({"key": key, "pkt": pkt_no, "file_id": file_id,
                               "transferred": total, "total": expect_size, "done": false}),
                    );
                }
            }
            // 符号链接/设备文件/资源分支等：不落盘，但内容必须读掉以免流错位
            other => {
                let skipped = rd.skip_exact(&mut stream, size).await?;
                ctx.st.diag(&format!(
                    "dl-dir-skip {key} 跳过非常规条目 {name}（类型 {other}，{skipped} 字节）"
                ));
                if skipped < size {
                    return Err(format!("传输中断：{name} 只收到 {skipped}/{size} 字节"));
                }
            }
        }
    }
    if !got_root {
        // 一条头部都没读到：交给上层换方言重试
        return Ok(None);
    }
    Ok(Some(total))
}

/// 目录名/文件名逐段清洗：拒绝路径分隔符与 `..`，防止流内容写到接收目录之外
fn safe_component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            _ => c,
        })
        .collect();
    let t = cleaned.trim();
    if t.is_empty() || t.trim_matches('.').is_empty() {
        return "unnamed".into();
    }
    t.to_string()
}

fn stream_timeout() -> Duration {
    Duration::from_secs(60)
}

/// 带缓冲的目录流读取器（头部与内容在同一 TCP 流里交错）
struct StreamReader {
    buf: Vec<u8>,
    pos: usize,
    timeout: Duration,
}

impl StreamReader {
    fn new(timeout: Duration) -> Self {
        StreamReader {
            buf: Vec::new(),
            pos: 0,
            timeout,
        }
    }

    fn avail(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// 补充至少 1 字节；返回 false 表示对端已关闭
    async fn fill<R: AsyncRead + Unpin>(&mut self, stream: &mut R) -> Result<bool, String> {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        let mut chunk = [0u8; 64 * 1024];
        let n = match tokio::time::timeout(self.timeout, stream.read(&mut chunk)).await {
            Ok(r) => r.map_err(|e| format!("接收数据失败: {e}"))?,
            Err(_) => return Err("接收超时（对方长时间无数据）".into()),
        };
        if n == 0 {
            return Ok(false);
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(true)
    }

    /// 读一个条目头部：`<总长16进制>:<名称>:<大小16进制>:<属性16进制>:`
    async fn read_header<R: AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
    ) -> Result<Option<(String, u64, u32)>, String> {
        // 先取长度字段（第一个冒号之前）
        let colon;
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == b':') {
                colon = i;
                break;
            }
            if self.avail() > 64 {
                return Err("目录流头部异常（长度字段过长）".into());
            }
            if !self.fill(stream).await? {
                return if self.avail() == 0 {
                    Ok(None) // 干净结束
                } else {
                    Err("目录流在头部中途中断".into())
                };
            }
        }
        let len_str = String::from_utf8_lossy(&self.buf[self.pos..self.pos + colon])
            .trim()
            .to_string();
        let head_len = u64::from_str_radix(len_str.trim_start_matches("0x"), 16)
            .map_err(|_| format!("目录流头部长度无法解析: {len_str:?}"))? as usize;
        if head_len <= len_str.len() || head_len > 4096 {
            return Err(format!("目录流头部长度不合理: {head_len}"));
        }
        while self.avail() < head_len {
            if !self.fill(stream).await? {
                return Err("目录流在头部中途中断".into());
            }
        }
        let head = self.buf[self.pos..self.pos + head_len].to_vec();
        self.pos += head_len;

        // 名称之后的字段固定为 大小:属性[:扩展]，从右往左定位，容忍名称里的冒号
        let body = &head[len_str.len() + 1..];
        let body = body.strip_suffix(b":").unwrap_or(body);
        let mut fields: Vec<&[u8]> = body.split(|&b| b == b':').collect();
        if fields.len() < 3 {
            return Err("目录流头部字段不足".into());
        }
        // 扩展属性（key=value）在尾部，剥掉后剩下 名称/大小/属性
        while fields.len() > 3 && fields.last().map(|f| f.contains(&b'=')).unwrap_or(false) {
            fields.pop();
        }
        let attr = num_flex_hex(&String::from_utf8_lossy(fields[fields.len() - 1]))
            .ok_or("目录流属性字段无法解析")? as u32;
        let size = num_flex_hex(&String::from_utf8_lossy(fields[fields.len() - 2]))
            .ok_or("目录流大小字段无法解析")?;
        let name = proto::decode_bytes(&fields[..fields.len() - 2].join(&b':'));
        Ok(Some((proto::strip_control(&name), size, attr)))
    }

    /// 丢弃接下来的 want 字节（用于跳过不落盘的条目），返回实际跳过量
    async fn skip_exact<R: AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
        want: u64,
    ) -> Result<u64, String> {
        let mut left = want;
        while left > 0 {
            if self.avail() == 0 && !self.fill(stream).await? {
                break;
            }
            let take = (left as usize).min(self.avail());
            self.pos += take;
            left -= take as u64;
        }
        Ok(want - left)
    }

    /// 把接下来的 want 字节写入文件，返回实际写入量（不足即为对端提前断流）
    async fn copy_exact<R: AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
        out: &mut tokio::fs::File,
        want: u64,
    ) -> Result<u64, String> {
        let mut left = want;
        while left > 0 {
            if self.avail() == 0 && !self.fill(stream).await? {
                break;
            }
            let take = (left as usize).min(self.avail());
            out.write_all(&self.buf[self.pos..self.pos + take])
                .await
                .map_err(|e| format!("写入文件失败: {e}"))?;
            self.pos += take;
            left -= take as u64;
        }
        Ok(want - left)
    }
}

fn num_flex_hex(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    u64::from_str_radix(t.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

/* ---------- 取文件请求的方言 ----------

GETFILEDATA/GETDIRFILES 的附加数据是 `包编号:文件ID:偏移`，但各实现对这三个
数字用什么进制书写并不一致：官方 IP Messenger 用十六进制，部分中文客户端沿用
公告里的十进制。我方服务端两种都认（见 id_candidates），但请求方向只能二选一，
之前固定发十进制 —— 对按官方约定解析的对端（飞秋等）就永远对不上号，
表现为"连接成功但一个字节都收不到"。

这里改成按方言表依次尝试：只有在对端接受连接却返回 0 字节时才换下一种，
命中后记住该对端的方言，后续下载直接用对的那种。 */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    /// 包编号用十六进制书写
    hex_pkt: bool,
    /// 文件 ID 用十六进制书写（否则原样回传对端公告里的字符串）
    hex_id: bool,
    /// 请求行以换行结尾
    newline: bool,
}

pub const DIALECTS: [Dialect; 4] = [
    // 1) 现状：包编号十进制 + ID 原样回显（本客户端与部分实现）
    Dialect { hex_pkt: false, hex_id: false, newline: true },
    // 2) 官方包编号十六进制 + ID 仍按对端公告原样
    Dialect { hex_pkt: true, hex_id: false, newline: false },
    // 3) 全按官方约定：包编号与 ID 都是十六进制
    Dialect { hex_pkt: true, hex_id: true, newline: false },
    // 4) 包编号十进制 + ID 十六进制
    Dialect { hex_pkt: false, hex_id: true, newline: true },
];

/// 密封取文件请求的内层串（spec §2.3）：
/// `<pkt_hex>:<fileid_hex>[:<offset_dec>]:900000:<key_hex>`。
/// 偏移只在断点续传（offset > 0）时携带，且按 **十进制** 书写 —— 官方口径，
/// 我们的服务端也以 num_flex_dec_first 十进制优先解析该字段；若误写十六进制，
/// 全数字的偏移（如 777=0x309）会被静默读错、从错误位置续传。
fn file_request_inner(pkt_field: &str, id_field: &str, offset: u64, key_hex: &str) -> String {
    let mut inner = format!("{pkt_field}:{id_field}");
    if offset > 0 {
        inner.push_str(&format!(":{offset}")); // 断点续传才带偏移（十进制，spec §2.3）
    }
    inner.push_str(":900000:");
    inner.push_str(key_hex);
    inner
}

/// 建立传输连接并发出取文件/取目录请求，返回供下载循环读取的流。
///
/// 加密路径（spec §7）：对端声明 CAPFILEENC 且我方加密开关开启时，扩展部改为
/// 密封的内层 `{pkt:x}:{id:x}[:{offset:x}]:900000:{key_hex}`（随机会话钥，
/// 请求方言无关），命令追加 ENCRYPTOPT|ENCFILEOPT；返回的读取端用同一把钥
/// 包上 EncStream —— 密钥流位置 = 流绝对偏移，断点续传时 seek(offset) 免费对齐。
/// 返回类型用 trait object 而非 TcpStream/枚举：两个调用点只读不写，
/// 统一读端让下载循环零分支。
/// pub(crate)：selftest 的断点续传腿直接以非零偏移调用本下载路径做回归验证。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn open_transfer(
    ctx: &NetCtx,
    target: SocketAddr,
    pkt_no: u32,
    file_id: u32,
    rid: &str,
    command: u32,
    d: Dialect,
    offset: u64,
) -> Result<(Box<dyn AsyncRead + Unpin + Send>, bool), String> {
    let cfg = ctx.st.config();
    let mut stream = tokio::time::timeout(Duration::from_secs(6), TcpStream::connect(target))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|e| format!("连接失败: {e}"))?;
    let pkt_field = if d.hex_pkt {
        format!("{pkt_no:x}")
    } else {
        pkt_no.to_string()
    };
    let id_field = if d.hex_id || rid.trim().is_empty() {
        format!("{file_id:x}")
    } else {
        rid.trim().to_string()
    };
    // CTR nonce 由 TCP 请求行的包号派生：收发双方都拿它当密钥流种子（spec §7）
    let req_pkt_no = proto::next_packet_no();

    let peer_ip = target.ip().to_string();
    // 密封失败（如公钥缺失）直接报错而不是回退明文：
    // 对端既然广告了文件流加密能力，静默明文会让用户误以为传输是加密的
    let enc = if cfg.encrypt && ctx.st.peer_capa(&peer_ip) & crypto::CAPA_CAPFILEENC != 0 {
        let peer_pub = ctx.st.peer_pubkey(&peer_ip).ok_or_else(|| {
            format!("{peer_ip} 广告了文件流加密能力但缺少公钥缓存，拒绝明文回退")
        })?;
        let key: [u8; 32] = rand::random();
        let inner = file_request_inner(
            &format!("{pkt_no:x}"),
            &format!("{file_id:x}"),
            offset,
            &crypto::hex_lower(&key),
        );
        let sealed =
            crypto::seal_file_request(&peer_pub, &ctx.st.own_keypair(), req_pkt_no, &inner)?;
        Some((key, sealed))
    } else {
        None
    };

    // 加密请求在命令位上声明：ENCRYPTOPT（扩展部密封）| ENCFILEOPT（正文走密钥流）
    let command = if enc.is_some() {
        command | opt::ENCRYPTOPT | opt::ENCFILEOPT
    } else {
        command
    };
    // 明文请求保持既有线上字节（extra = pkt:id + 终止符 0）；
    // 加密请求的扩展部整体是密封串，绝不能再拼 ":0" —— 那会被当成
    // 内层最后一个段，破坏解封。
    let mut req = format!(
        "1:{}:{}:{}:{}:",
        req_pkt_no,
        my_user(&cfg),
        my_host(),
        command
    );
    match &enc {
        Some((_, sealed)) => {
            // 加密请求方言无关：恒以换行收尾
            req.push_str(sealed);
            req.push('\n');
        }
        None => {
            req.push_str(&pkt_field);
            req.push(':');
            req.push_str(&id_field);
            req.push_str(":0");
            if d.newline {
                req.push('\n');
            }
        }
    }
    oim_log!(
        "[download] 连接 {target} 请求 cmd={command:#x} pkt={pkt_field} id={id_field} enc={}",
        enc.is_some()
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("发送请求失败: {e}"))?;
    Ok((
        match enc {
            Some((key, _)) => Box::new(crypto::EncStream::new(stream, &key, req_pkt_no, offset)),
            None => Box::new(stream),
        },
        enc.is_some(), // 是否加密流（UI 区分「下载/解密」状态）
    ))
}

/// 下载失败的统一收尾：推事件 + 回写历史，避免卡片永远停在"下载中"
fn fail_download(ctx: &NetCtx, key: &str, pkt_no: u32, file_id: u32, err: &str) {
    ctx.st.emit(
        "file-progress",
        json!({"key": key, "pkt": pkt_no, "file_id": file_id,
               "transferred": 0, "total": 0, "done": false, "error": err}),
    );
    ctx.st.update_history_file(key, pkt_no, file_id, |f| {
        f["state"] = "failed".into();
        f["error"] = Value::String(err.to_string());
    });
}

#[allow(clippy::too_many_arguments)]
async fn fetch_to_file(
    ctx: &NetCtx,
    key: &str,
    target: SocketAddr,
    pkt_no: u32,
    file_id: u32,
    rid: &str,
    expect_size: u64,
    d: Dialect,
    tmp: &std::path::Path,
) -> Result<(u64, bool), String> {
    // 读取端可能是 TcpStream 或解密包装（open_transfer 决定）；enc 用于
    // 前端区分「下载中（加密流）」与「解密完成」两个状态
    let (mut stream, enc) =
        open_transfer(ctx, target, pkt_no, file_id, rid, cmd::GETFILEDATA, d, 0).await?;

    let mut file = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    let mut total: u64 = 0;
    let mut last_emit = Instant::now();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        // 已收满公告字节数就停：部分实现收完不主动关连接，读到 EOF 会一直阻塞
        if expect_size > 0 && total >= expect_size {
            break;
        }
        let want = if expect_size > 0 {
            ((expect_size - total) as usize).min(chunk.len())
        } else {
            chunk.len()
        };
        // 长时间无数据视为对端异常，避免任务永久挂起
        let n = match tokio::time::timeout(Duration::from_secs(60), stream.read(&mut chunk[..want]))
            .await
        {
            Ok(r) => r.map_err(|e| format!("接收数据失败: {e}"))?,
            Err(_) => return Err("接收超时（对方长时间无数据）".into()),
        };
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
                       "transferred": total, "total": expect_size, "done": false,
                       "enc": enc}),
            );
        }
    }
    file.flush().await.map_err(|e| format!("写入失败: {e}"))?;
    drop(file);
    oim_log!("[download] 从 {target} 接收完成: {total} 字节 enc={enc}");
    Ok((total, enc))
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
