//! 无头自检模式 (`--selftest`)：
//! 在本机回环地址上启动完整网络栈，并内置一个「假对端」完成互通验证：
//!   1. 上线发现（BR_ENTRY → ANSENTRY 注册）
//!   2. 文本消息收发（双向，含 READMSG 回执与陌生来源注册）
//!   3. 发送附件 —— 假对端作为 TCP 客户端把文件完整取回并逐字节比对
//!   4. 接收附件 —— 假对端作为 TCP 服务端供我们下载并逐字节比对
//!   5. 下线广播（BR_EXIT）
//! 全部通过打印 PASS 行并以退出码 0 结束；任一失败打印 FAIL 且退出码非 0。

use crate::net;
use crate::protocol as proto;
use crate::protocol::{cmd, fileattr, opt};
use crate::state::{AppState, Config};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

struct Log(Vec<(String, bool)>);

impl Log {
    fn check(&mut self, name: &str, ok: bool) {
        println!("[{}] {}", if ok { "PASS" } else { "FAIL" }, name);
        self.0.push((name.into(), ok));
    }
    fn all_ok(&self) -> bool {
        self.0.iter().all(|(_, ok)| *ok)
    }
}

#[derive(Default)]
struct PeerShared {
    texts: Vec<String>,
    fetched: Vec<Vec<u8>>,
    receipts: Vec<u32>,
    exit_received: bool,
}

pub fn run() -> bool {
    println!("== OpenIPMsg 无头自检 (--selftest) ==");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async_run())
}

async fn free_udp_port() -> u16 {
    let s = UdpSocket::bind(("127.0.0.1", 0)).await.expect("bind :0");
    s.local_addr().expect("local_addr").port()
}

async fn wait_for(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn async_run() -> bool {
    let port_app = free_udp_port().await;
    let port_peer = free_udp_port().await;

    let data_dir =
        std::env::temp_dir().join(format!("open-ipmsg-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    // 应用侧状态与事件采集
    let st = Arc::new(AppState::new(data_dir.clone()));
    let mut cfg = Config::default();
    cfg.nickname = "自检用户".into();
    cfg.group = "测试组".into();
    cfg.encoding = "utf8".into();
    cfg.download_dir = data_dir.join("dl").to_string_lossy().into_owned();
    std::fs::create_dir_all(&cfg.download_dir).unwrap();
    st.set_config(cfg);

    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let ev = events.clone();
        st.set_event(Box::new(move |e, v| ev.lock().unwrap().push((e.to_string(), v))));
    }

    let ctx = net::start_network(st.clone(), port_app)
        .await
        .expect("start network");
    println!("[..] 网络栈已启动 (UDP/TCP 端口 {port_app})");

    /* ---------------- 假对端 ---------------- */
    let fake_content: Vec<u8> = b"FAKE-PEER-FILE-CONTENT-".repeat(4000); // ~92KB
    let offer_pkt_no = proto::next_packet_no();
    let shared = Arc::new(Mutex::new(PeerShared::default()));
    let peer_tasks =
        spawn_fake_peer(port_peer, port_app, shared.clone(), fake_content.clone(), offer_pkt_no);
    println!("[..] 假对端已就绪 (端口 {port_peer})");
    // 假对端只绑定在 127.0.0.1，广播到不了它 —— 按真实场景做单播发现
    tokio::time::sleep(Duration::from_millis(60)).await;
    let peer_addr: SocketAddr = format!("127.0.0.1:{port_peer}").parse().unwrap();
    net::announce_unicast(&ctx, &[peer_addr]).await;

    let mut log = Log(vec![]);
    let peer_key = format!("127.0.0.1:{port_peer}");

    /* ---- 1. 发现 ---- */
    let discovered = wait_for(2500, || st.peers.lock().unwrap().contains_key(&peer_key)).await;
    log.check("发现假对端（BR_ENTRY→ANSENTRY 注册）", discovered);
    if !discovered {
        return finish(log, peer_tasks, &data_dir);
    }
    let info_ok = wait_for(2500, || {
        st.peers
            .lock()
            .unwrap()
            .get(&peer_key)
            .map(|p| p.nickname == "假对端" && p.group == "测试组")
            .unwrap_or(false)
    })
    .await;
    log.check("对端昵称/群组解析正确", info_ok);

    /* ---- 2a. 出站文本 ---- */
    let rec = net::send_message(&ctx, &peer_key, "你好，假对端！", vec![])
        .await
        .expect("send text");
    log.check("发送文本返回 out 记录", rec["dir"] == "out");
    let got_text = wait_for(3000, || {
        shared.lock().unwrap().texts.iter().any(|t| t == "你好，假对端！")
    })
    .await;
    log.check("假对端收到文本", got_text);

    /* ---- 2b. 入站文本（陌生来源主动发来） ---- */
    let in_evt = wait_for(3000, || {
        events.lock().unwrap().iter().any(|(e, v)| {
            e == "msg-in"
                && v["msg"]["text"] == "你好，我是假对端"
                && v["key"] == peer_key.as_str()
        })
    })
    .await;
    log.check("收到陌生来源消息：注册用户+落库+事件通知", in_evt);

    let hist = st.read_history(&peer_key, 50);
    log.check(
        "JSONL 会话历史包含双向记录",
        hist.iter().any(|r| r["dir"] == "in") && hist.iter().any(|r| r["dir"] == "out"),
    );

    /* ---- 3. 发送附件（对端经 TCP 取回比对） ---- */
    let content: Vec<u8> = (0..250_000u32).map(|i| ((i * 31 + 7) % 251) as u8).collect();
    let send_path = data_dir.join("upload.bin");
    std::fs::write(&send_path, &content).unwrap();
    let rec = net::send_message(
        &ctx,
        &peer_key,
        "文件给你",
        vec![send_path.to_string_lossy().into_owned()],
    )
    .await
    .expect("send file");
    log.check(
        "SENDMSG 携带 FILEATTACHOPT 与文件项",
        rec["files"].as_array().map(|a| a.len()) == Some(1),
    );
    let fetched_ok = wait_for(6000, || {
        let p = shared.lock().unwrap();
        p.fetched.last().map(|b| *b == content).unwrap_or(false)
    })
    .await;
    log.check("假对端经 TCP 完整取回文件且逐字节一致", fetched_ok);

    /* ---- 4. 接收附件（对端提供 TCP 服务） ---- */
    let offered = wait_for(3000, || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|(e, v)| e == "msg-in" && v["msg"]["text"] == "请收文件")
    })
    .await;
    log.check("收到带附件的入站消息", offered);
    if offered {
        match net::download_file_task(&ctx, &peer_key, offer_pkt_no, 9, "假对端文件.bin", "").await
        {
            Ok(path) => {
                let saved = std::fs::read(&path).unwrap_or_default();
                log.check("下载完成且逐字节一致", saved == fake_content);
                let hist = st.read_history(&peer_key, 20);
                let state_ok = hist
                    .iter()
                    .rev()
                    .find(|r| r["kind"] == "file" && r["dir"] == "in")
                    .and_then(|r| r["files"][0]["state"].as_str())
                    .map(|s| s == "done")
                    .unwrap_or(false);
                log.check("历史记录中附件状态更新为 done", state_ok);
            }
            Err(e) => {
                eprintln!("      下载错误: {e}");
                log.check("下载完成且逐字节一致", false);
            }
        }
    }

    /* ---- 5. 已读回执 ---- */
    // 出站方向：我们发的消息带 READCHECKOPT，假对端回复 READMSG 后应标记为已读
    let out_read = wait_for(3000, || {
        st.read_history(&peer_key, 10)
            .iter()
            .find(|r| r["dir"] == "out" && r["text"] == "你好，假对端！")
            .and_then(|r| r.get("read"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })
    .await;
    log.check("收到 READMSG 回执后出站消息标记已读", out_read);

    // 入站方向：标记已读 → 向假对端发出 READMSG；本地历史同步标记
    let sent = net::mark_read_and_receipt(&ctx, &peer_key, &[777123])
        .await
        .expect("mark_read");
    log.check(
        "标记入站消息已读并回执（need_read 才发）",
        sent == 1,
    );
    let receipt_seen = wait_for(2500, || {
        shared.lock().unwrap().receipts.contains(&777123)
    })
    .await;
    log.check("假对端收到 READMSG 回执", receipt_seen);
    let in_marked = wait_for(1500, || {
        st.read_history(&peer_key, 20)
            .iter()
            .find(|r| r["pkt"].as_u64() == Some(777123))
            .and_then(|r| r.get("read"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })
    .await;
    log.check("历史记录中入站消息已标记已读", in_marked);

    /* ---- 6. 下线广播 ---- */
    net::announce_exit_blocking(&st, port_app);
    let exit_seen = wait_for(1500, || shared.lock().unwrap().exit_received).await;
    log.check("BR_EXIT 广播送达对端", exit_seen);

    finish(log, peer_tasks, &data_dir)
}

fn finish(log: Log, tasks: Vec<tokio::task::JoinHandle<()>>, data_dir: &Path) -> bool {
    for t in tasks {
        t.abort();
    }
    let ok = log.all_ok();
    let _ = std::fs::remove_dir_all(data_dir);
    println!("== 自检{} ==", if ok { "全部通过 ✔" } else { "存在失败项 ✘" });
    ok
}

/* ==================== 假对端实现 ==================== */

fn spawn_fake_peer(
    port_peer: u16,
    port_app: u16,
    shared: Arc<Mutex<PeerShared>>,
    serve_content: Vec<u8>,
    offer_pkt_no: u32,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();

    let std_sock = std::net::UdpSocket::bind(("127.0.0.1", port_peer)).expect("fake bind udp");
    std_sock.set_nonblocking(true).unwrap();
    let ps = Arc::new(UdpSocket::from_std(std_sock).expect("fake convert udp"));

    /* -- 主动问候（测试「陌生 SENDMSG 注册」路径） -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let mut pkt = proto::Packet::new(cmd::SENDMSG);
            pkt.extra = "你好，我是假对端".as_bytes().to_vec();
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;
        }
    }));

    /* -- 主动提供文件下载（SENDMSG + FILEATTACHOPT） -- */
    let content_len = serve_content.len() as u64;
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let entry = proto::FileEntry {
                id: 9,
                raw_id: String::new(),
                name: "假对端文件.bin".into(),
                size: content_len,
                mtime: 123,
                attr: fileattr::REGULAR,
            };
            let mut extra = "请收文件".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize().as_bytes());
            let pkt = proto::Packet {
                pkt_no: offer_pkt_no,
                user: "假对端".into(),
                host: "fake-host".into(),
                command: cmd::SENDMSG | opt::FILEATTACHOPT,
                extra,
            };
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;
        }
    }));

    /* -- 主动发送需要已读回执的消息（SENDMSG + READCHECKOPT） -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let pkt = proto::Packet {
                pkt_no: 777123,
                user: "假对端".into(),
                host: "fake-host".into(),
                command: cmd::SENDMSG | opt::READCHECKOPT,
                extra: "带回执的消息".as_bytes().to_vec(),
            };
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;
        }
    }));

    /* -- UDP 循环：应答发现、收消息、取附件、记下线与回执 -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        let shared = shared.clone();
        async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let (n, from) = match ps.recv_from(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                let Some(pkt) = proto::parse(&buf[..n]) else { continue };
                match pkt.command & 0xFF {
                    cmd::BR_ENTRY => {
                        let mut a = proto::Packet::new(cmd::ANSENTRY);
                        a.extra = proto::build_entry_extra("假对端", "测试组", "utf8");
                        let _ = ps.send_to(&a.encode("假对端", "fake-host"), from).await;
                    }
                    cmd::SENDMSG => {
                        let text = proto::text_of(&pkt);
                        shared.lock().unwrap().texts.push(text);
                        if pkt.command & opt::FILEATTACHOPT != 0 {
                            for f in proto::parse_file_entries(&pkt.extra) {
                                match fetch_file(port_app, pkt.pkt_no, f.id).await {
                                    Ok(bytes) => shared.lock().unwrap().fetched.push(bytes),
                                    Err(e) => eprintln!(
                                        "[fake-peer] 取文件失败 pkt={} id={}: {e}",
                                        pkt.pkt_no, f.id
                                    ),
                                }
                            }
                        }
                        let mut r = proto::Packet::new(cmd::READMSG | opt::AUTORETOPT);
                        r.extra = pkt.pkt_no.to_string().into_bytes();
                        let _ = ps.send_to(&r.encode("假对端", "fake-host"), from).await;
                    }
                    cmd::BR_EXIT => {
                        shared.lock().unwrap().exit_received = true;
                    }
                    cmd::READMSG => {
                        let no = String::from_utf8_lossy(&pkt.extra)
                            .split(':')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .parse::<u32>()
                            .ok();
                        if let Some(no) = no {
                            shared.lock().unwrap().receipts.push(no);
                        }
                    }
                    _ => {}
                }
            }
        }
    }));

    /* -- TCP 服务：向应用的下载请求回传内容 -- */
    tasks.push(tokio::spawn(async move {
        let listener = match TcpListener::bind(("127.0.0.1", port_peer)).await {
            Ok(l) => l,
            Err(_) => return,
        };
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { continue };
            let content = serve_content.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await; // 读掉请求行
                if stream.write_all(&content).await.is_err() {
                    return;
                }
                let _ = stream.flush().await;
            });
        }
    }));

    tasks
}

/// 假对端作为 TCP 客户端，从应用侧 GETFILEDATA 取文件
async fn fetch_file(port_app: u16, pkt_no: u32, file_id: u32) -> Result<Vec<u8>, String> {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port_app)))
            .await
            .map_err(|_| "连接超时".to_string())?
            .map_err(|e| format!("connect: {e}"))?;
    let req = format!(
        "1:{}:fake:fake-host:{}:{}:{:x}:0\n",
        proto::next_packet_no(),
        cmd::GETFILEDATA,
        pkt_no,
        file_id
    );
    stream.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.map_err(|e| e.to_string())?;
    Ok(out)
}
