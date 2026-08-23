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
    /// 收到的送达确认（RECVMSG）包编号 —— 十进制与十六进制两种写法都收
    recv_acks: Vec<u32>,
    /// 777456 还在待发队列里（没收到送达确认），对方一上线就重投
    queued_ack: bool,
    /// 777789 同理，但这个"对端"只认十六进制书写的包编号（方言差异）
    queued_hex: bool,
    /// 置位后：每当对方上线就重投一次「延迟发送」消息（飞秋等实现的行为）
    delayed_armed: bool,
    texts: Vec<String>,
    fetched: Vec<Vec<u8>>,
    /// 经 GETDIRFILES 取回的目录树：(相对路径, 内容)
    fetched_dirs: Vec<Vec<(String, Vec<u8>)>>,
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
    let img_pkt_no = proto::next_packet_no();
    let peer_tasks = spawn_fake_peer(
        port_peer,
        port_app,
        shared.clone(),
        fake_content.clone(),
        offer_pkt_no,
        img_pkt_no,
    );
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

    /* ---- 3b. 发送目录（对端经 GETDIRFILES 取回整棵树） ---- */
    let send_dir = data_dir.join("upload_dir");
    std::fs::create_dir_all(send_dir.join("sub")).unwrap();
    std::fs::write(send_dir.join("root.txt"), "根目录文件".as_bytes()).unwrap();
    std::fs::write(send_dir.join("sub/inner.bin"), (0..9000u32).map(|i| (i % 253) as u8).collect::<Vec<u8>>()).unwrap();
    let rec = net::send_message(
        &ctx,
        &peer_key,
        "目录给你",
        vec![send_dir.to_string_lossy().into_owned()],
    )
    .await
    .expect("send dir");
    log.check(
        "SENDMSG 目录项属性为 DIR 且体积为递归总字节数",
        rec["files"][0]["dir_entry"] == true
            && rec["files"][0]["size"].as_u64() == Some(9000 + "根目录文件".len() as u64),
    );
    let want_tree = read_tree(&send_dir);
    let dir_fetch_ok = wait_for(8000, || {
        let p = shared.lock().unwrap();
        p.fetched_dirs.last().map(|t| *t == want_tree).unwrap_or(false)
    })
    .await;
    log.check("假对端经 GETDIRFILES 取回整棵目录树且逐字节一致", dir_fetch_ok);

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
    // 回归保护：入站附件必须出现在消息记录里，否则前端无从展示/下载
    {
        let ev = events.lock().unwrap();
        let files_ok = ev
            .iter()
            .find(|(e, v)| e == "msg-in" && v["msg"]["text"] == "请收文件")
            .and_then(|(_, v)| v["msg"]["files"].as_array())
            .map(|a| {
                a.len() == 1
                    && a[0]["name"] == "假对端文件.bin"
                    && a[0]["size"].as_u64() == Some(fake_content.len() as u64)
                    && a[0]["state"] == "pending"
            })
            .unwrap_or(false);
        log.check("入站消息携带附件列表（含大小，非图片为待下载）", files_ok);
    }
    if offered {
        match net::download_file_task(
            &ctx,
            &peer_key,
            offer_pkt_no,
            9,
            "假对端文件.bin",
            "",
            fake_content.len() as u64,
            false,
        )
        .await
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

    /* ---- 4a-2. 请求方言协商（对端只认官方十六进制） ---- */
    {
        let diag = std::fs::read_to_string(data_dir.join("diag.log")).unwrap_or_default();
        log.check(
            "首选方言被对端拒绝（0 字节）后自动换用官方十六进制方言",
            diag.contains("方言#0") && diag.contains("方言#1 命中"),
        );
    }

    /* ---- 4b. 接收目录（对端以 GETDIRFILES 流回传） ---- */
    match net::download_file_task(
        &ctx,
        &peer_key,
        offer_pkt_no,
        11,
        "假对端目录",
        "",
        0,
        true,
    )
    .await
    {
        Ok(path) => {
            let got = read_tree(&path);
            let want: Vec<(String, Vec<u8>)> = fake_dir_files()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            let mut want = want;
            want.sort();
            log.check("接收目录：目录树重建完整且逐字节一致", got == want);
            log.check(
                "接收目录：落盘目录名与公告一致",
                path.file_name().map(|n| n == "假对端目录").unwrap_or(false),
            );
        }
        Err(e) => {
            log.check(&format!("接收目录失败: {e}"), false);
        }
    }

    /* ---- 4c. 入站图片自动接收（无需用户点下载，直接内联显示） ---- */
    let img_auto = wait_for(8000, || {
        let hist = st.read_history(&peer_key, 50);
        hist.iter().any(|r| {
            r["pkt"].as_u64() == Some(img_pkt_no as u64)
                && r["files"][0]["state"] == "done"
                && r["files"][0]["name"] == "截图.png"
        })
    })
    .await;
    log.check("入站图片自动接收完成（state=done）", img_auto);
    if img_auto {
        let hist = st.read_history(&peer_key, 50);
        let saved = hist
            .iter()
            .find(|r| r["pkt"].as_u64() == Some(img_pkt_no as u64))
            .and_then(|r| r["files"][0]["path"].as_str().map(String::from))
            .unwrap_or_default();
        log.check(
            "自动接收的图片落盘且逐字节一致",
            std::fs::read(&saved).unwrap_or_default() == fake_content,
        );
        // 前端内联预览依赖后端这个读图命令能识别扩展名
        log.check(
            "落盘图片保留 .png 扩展名（内联预览的前提）",
            saved.to_lowercase().ends_with(".png"),
        );
    }

    /* ---- 4d. 发送剪贴板图片（落盘缓存 → 按附件公告 → 对端取回比对） ---- */
    let png_bytes: Vec<u8> = b"\x89PNG\r\n\x1a\n".iter().copied().chain(0..200u8).collect();
    let b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    };
    match net::stage_clipboard_image(&data_dir, &b64, "image/png") {
        Ok(staged) => {
            log.check(
                "剪贴板图片落盘为 .png 缓存文件",
                staged.extension().map(|e| e == "png").unwrap_or(false)
                    && std::fs::read(&staged).unwrap_or_default() == png_bytes,
            );
            let rec = net::send_message(
                &ctx,
                &peer_key,
                "带图的消息",
                vec![staged.to_string_lossy().into_owned()],
            )
            .await
            .expect("send clipboard image");
            log.check(
                "剪贴板图片以普通附件公告（正文与附件同一条消息）",
                rec["text"] == "带图的消息"
                    && rec["files"].as_array().map(|a| a.len()) == Some(1)
                    && rec["files"][0]["name"]
                        .as_str()
                        .map(|n| n.ends_with(".png"))
                        .unwrap_or(false),
            );
            let img_fetch_ok = wait_for(6000, || {
                let p = shared.lock().unwrap();
                p.fetched.contains(&png_bytes)
            })
            .await;
            log.check("假对端取回剪贴板图片且逐字节一致", img_fetch_ok);
        }
        Err(e) => log.check(&format!("剪贴板图片落盘失败: {e}"), false),
    }

    /* ---- 4e. 送达确认（RECVMSG）---- */
    // 对端每条消息都带 SENDCHECKOPT：收到后必须立刻回 RECVMSG，
    // 否则它认为没送达，会把消息留在队列里，每次我方上线就重投一遍
    // ——「离线留言删了又出现」就是这么来的。
    let acked = wait_for(3000, || shared.lock().unwrap().recv_acks.contains(&777456)).await;
    log.check("收到带送达确认请求的消息后立即回 RECVMSG", acked);
    log.check(
        "对端已把该消息移出待发队列",
        !shared.lock().unwrap().queued_ack,
    );
    log.check(
        "不给未要求确认的消息发送达确认",
        !shared.lock().unwrap().recv_acks.contains(&777123),
    );

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

    /* ---- 5a-2. 标记不要求回执的消息为已读：只落库，不打扰对端 ---- */
    // 前端"用户正在看这个会话"时会把所有未读入站消息都提交标记已读（未读数才能清零），
    // 后端必须只对带 READCHECKOPT 的那些回 READMSG。
    let greet_pkt = st
        .read_history(&peer_key, 50)
        .iter()
        .find(|r| r["dir"] == "in" && r["text"] == "你好，我是假对端")
        .and_then(|r| r["pkt"].as_u64())
        .map(|p| p as u32);
    match greet_pkt {
        Some(pkt) => {
            let sent = net::mark_read_and_receipt(&ctx, &peer_key, &[pkt])
                .await
                .expect("mark_read greeting");
            log.check("标记「无需回执」的消息已读不发 READMSG", sent == 0);
            tokio::time::sleep(Duration::from_millis(300)).await;
            log.check(
                "对端未收到多余的已读通知",
                !shared.lock().unwrap().receipts.contains(&pkt),
            );
            log.check(
                "该消息在历史中仍被标记为已读（未读数据以此为准）",
                st.read_history(&peer_key, 50)
                    .iter()
                    .find(|r| r["pkt"].as_u64() == Some(pkt as u64))
                    .and_then(|r| r["read"].as_bool())
                    .unwrap_or(false),
            );
        }
        None => log.check("找不到问候消息，无法验证已读标记", false),
    }

    /* ---- 5b. 重启后对端重投延迟消息：不重复落库、不重复回执 ---- */
    // 真实场景：关掉程序再打开，对端把没确认的历史消息按原包号重投一遍。
    // 这里用「同一数据目录 + 新端口的第二套网络栈」模拟一次重启。
    let receipts_before = shared.lock().unwrap().receipts.iter().filter(|p| **p == 777123).count();
    let port_app2 = free_udp_port().await;
    let st2 = Arc::new(AppState::new(data_dir.clone()));
    let mut cfg2 = Config::default();
    cfg2.nickname = "自检用户".into();
    cfg2.group = "测试组".into();
    cfg2.encoding = "utf8".into();
    cfg2.download_dir = data_dir.join("dl").to_string_lossy().into_owned();
    st2.set_config(cfg2);
    let ctx2 = net::start_network(st2.clone(), port_app2)
        .await
        .expect("restart network");
    net::announce_unicast(&ctx2, &[peer_addr]).await;

    let resent = wait_for(4000, || {
        st2.read_history(&peer_key, 100).iter().any(|r| {
            r["pkt"].as_u64() == Some(777123)
                && r["text"].as_str().map(|t| t.contains("Delayed Send")).unwrap_or(false)
        })
    })
    .await;
    log.check("重启后收到对端重投的延迟消息", resent);

    let hist = st2.read_history(&peer_key, 200);
    let copies = hist
        .iter()
        .filter(|r| r["dir"] == "in" && r["pkt"].as_u64() == Some(777123))
        .count();
    log.check(
        &format!("重投不产生重复历史记录（实际 {copies} 条）"),
        copies == 1,
    );
    log.check(
        "重投副本继承已读状态（不再被当成新未读）",
        hist.iter()
            .find(|r| r["pkt"].as_u64() == Some(777123))
            .and_then(|r| r["read"].as_bool())
            .unwrap_or(false),
    );

    // 模拟重启后打开会话/窗口重新聚焦：前端会再次请求标记已读
    let again = net::mark_read_and_receipt(&ctx2, &peer_key, &[777123])
        .await
        .expect("mark_read again");
    log.check("重复标记已读不再发回执", again == 0);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let receipts_after = shared.lock().unwrap().receipts.iter().filter(|p| **p == 777123).count();
    log.check(
        &format!("对端不会反复收到「消息已被查看」（{receipts_before} → {receipts_after} 次）"),
        receipts_after == receipts_before,
    );

    /* ---- 5b-2. 已确认送达的消息，重启后不再被重投 ---- */
    let copies_456 = st2
        .read_history(&peer_key, 200)
        .iter()
        .filter(|r| r["dir"] == "in" && r["pkt"].as_u64() == Some(777456))
        .count();
    log.check(
        &format!("已确认送达的消息重启后不再重投（历史 {copies_456} 条）"),
        copies_456 == 1,
    );

    /* ---- 5b-3. 对端只认十六进制包编号时，送达确认能自动收敛 ---- */
    // 首次确认按协议头部的十进制书写；对端不认就会重投，
    // 重投时我方改用十六进制再确认一次，一个会话内即可收敛。
    let hex_ok = wait_for(3000, || !shared.lock().unwrap().queued_hex).await;
    if !hex_ok {
        // 再触发一次上线通告，让对端重投 → 我方换十六进制确认
        net::announce_unicast(&ctx2, &[peer_addr]).await;
    }
    let hex_ok = hex_ok || wait_for(4000, || !shared.lock().unwrap().queued_hex).await;
    log.check("对端只认十六进制包编号时，重投后自动换写法确认成功", hex_ok);

    /* ---- 5c. 清空会话聊天记录 ---- */
    let before = st2.read_history(&peer_key, 500).len();
    let removed = st2.clear_history(&peer_key);
    log.check(
        &format!("清空聊天记录（{before} 条 → 删除 {removed} 条）"),
        before > 0 && removed == before && st2.read_history(&peer_key, 500).is_empty(),
    );
    // 清空之后会话仍然可用：新消息照常落库
    st2.log_record(
        &peer_key,
        &serde_json::json!({"dir": "out", "kind": "text", "text": "清空后的新消息", "pkt": 1, "ts": 1}),
    );
    log.check(
        "清空后会话仍可继续记录新消息",
        st2.read_history(&peer_key, 10).len() == 1,
    );

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
    img_pkt_no: u32,
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

    /* -- 主动发送一条「要求送达确认」的消息（真实对端每条都带 SENDCHECKOPT） -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        let shared = shared.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let pkt = proto::Packet {
                pkt_no: 777456,
                user: "假对端".into(),
                host: "fake-host".into(),
                command: cmd::SENDMSG | opt::SENDCHECKOPT,
                extra: "要求送达确认的消息".as_bytes().to_vec(),
            };
            shared.lock().unwrap().queued_ack = true;
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;

            // 再发一条：这条的"对端"只接受十六进制书写的送达确认
            let pkt = proto::Packet {
                pkt_no: 777789,
                user: "假对端".into(),
                host: "fake-host".into(),
                command: cmd::SENDMSG | opt::SENDCHECKOPT,
                extra: "只认十六进制确认的消息".as_bytes().to_vec(),
            };
            shared.lock().unwrap().queued_hex = true;
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;
        }
    }));

    /* -- 主动公告一张图片（验证「小图片自动接收 + 内联预览」路径） -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let entry = proto::FileEntry {
                id: 12,
                raw_id: String::new(),
                name: "截图.png".into(),
                size: content_len,
                mtime: 123,
                attr: fileattr::REGULAR,
            };
            let mut extra = "看这张图".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize().as_bytes());
            let pkt = proto::Packet {
                pkt_no: img_pkt_no,
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
                        // 未收到送达确认的消息：对方一上线就再投一次
                        if shared.lock().unwrap().queued_ack {
                            let pkt = proto::Packet {
                                pkt_no: 777456,
                                user: "假对端".into(),
                                host: "fake-host".into(),
                                command: cmd::SENDMSG | opt::SENDCHECKOPT,
                                extra: "要求送达确认的消息\n----\n(IPMsg Delayed Send: 20:00 )"
                                    .as_bytes()
                                    .to_vec(),
                            };
                            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), from).await;
                        }
                        if shared.lock().unwrap().queued_hex {
                            let pkt = proto::Packet {
                                pkt_no: 777789,
                                user: "假对端".into(),
                                host: "fake-host".into(),
                                command: cmd::SENDMSG | opt::SENDCHECKOPT,
                                extra: "只认十六进制确认的消息".as_bytes().to_vec(),
                            };
                            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), from).await;
                        }
                        // 对方（重新）上线：把没确认过的延迟消息按原包号再投一次
                        if shared.lock().unwrap().delayed_armed {
                            let pkt = proto::Packet {
                                pkt_no: 777123,
                                user: "假对端".into(),
                                host: "fake-host".into(),
                                command: cmd::SENDMSG | opt::READCHECKOPT,
                                // 重发副本正文带尾注，与首投并不逐字相同
                                extra: "带回执的消息\n----\n(IPMsg Delayed Send: 19:18 )"
                                    .as_bytes()
                                    .to_vec(),
                            };
                            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), from).await;
                        }
                    }
                    cmd::SENDMSG => {
                        let text = proto::text_of(&pkt);
                        shared.lock().unwrap().texts.push(text);
                        if pkt.command & opt::FILEATTACHOPT != 0 {
                            for f in proto::parse_file_entries(&pkt.extra) {
                                if f.attr & 0xFF == fileattr::DIR {
                                    match fetch_dir(port_app, pkt.pkt_no, f.id).await {
                                        Ok(tree) => shared.lock().unwrap().fetched_dirs.push(tree),
                                        Err(e) => eprintln!(
                                            "[fake-peer] 取目录失败 pkt={} id={}: {e}",
                                            pkt.pkt_no, f.id
                                        ),
                                    }
                                    continue;
                                }
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
                    cmd::RECVMSG => {
                        // 送达确认：包编号可能是十进制或十六进制书写，两种都认
                        let raw = String::from_utf8_lossy(&pkt.extra);
                        let t = raw.split(':').next().unwrap_or("").trim();
                        // 这个"对端"对 777789 只接受十六进制写法
                        if t.eq_ignore_ascii_case(&format!("{:x}", 777789u32)) {
                            shared.lock().unwrap().queued_hex = false;
                        }
                        let no = t
                            .parse::<u32>()
                            .ok()
                            .or_else(|| u32::from_str_radix(t, 16).ok());
                        if let Some(no) = no {
                            let mut sh = shared.lock().unwrap();
                            sh.recv_acks.push(no);
                            // 送达已确认：从待发队列里移除，不再重投
                            if no == 777456 {
                                sh.queued_ack = false;
                            }
                        }
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
                            let mut sh = shared.lock().unwrap();
                            sh.receipts.push(no);
                            if no == 777123 {
                                sh.delayed_armed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }));

    /* -- TCP 服务：向应用的下载请求回传内容（严格按官方十六进制方言校验） -- */
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
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let Some(req) = proto::parse(&buf[..n]) else { return };
                let extra = String::from_utf8_lossy(&req.extra);
                let mut fields = extra.split(':');
                // 按官方 IP Messenger 约定解析：包编号是十六进制。
                // 真实的飞秋类客户端就是这么解析的 —— 请求方若发十进制，
                // 这里对不上号，于是接受连接后一个字节都不回。
                let pkt_ok = fields
                    .next()
                    .and_then(|f| u32::from_str_radix(f.trim(), 16).ok())
                    .map(|p| p == offer_pkt_no || p == img_pkt_no)
                    .unwrap_or(false);
                if !pkt_ok {
                    let _ = stream.shutdown().await;
                    return;
                }
                let body = if req.command & 0xFF == cmd::GETDIRFILES {
                    fake_dir_stream()
                } else {
                    content
                };
                if stream.write_all(&body).await.is_err() {
                    return;
                }
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            });
        }
    }));

    tasks
}


/* ---------------- 目录流工具（自检内独立实现，与被测代码不共享逻辑） ---------------- */

fn dir_head(name: &str, size: u64, attr: u32) -> Vec<u8> {
    let body = format!(":{name}:{size:x}:{attr:x}:");
    let total = 4 + body.len();
    format!("{total:04x}{body}").into_bytes()
}

/// 假对端提供的目录树：dirA/{hello.txt, sub/{data.bin}}
fn fake_dir_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("hello.txt", "目录里的文本内容".as_bytes().to_vec()),
        ("sub/data.bin", (0..5000u32).map(|i| (i % 256) as u8).collect()),
    ]
}

fn fake_dir_stream() -> Vec<u8> {
    let files = fake_dir_files();
    let mut out = dir_head("假对端目录", 0, 2);
    let (_, ref f0) = files[0];
    out.extend_from_slice(&dir_head("hello.txt", f0.len() as u64, 1));
    out.extend_from_slice(f0);
    out.extend_from_slice(&dir_head("sub", 0, 2));
    let (_, ref f1) = files[1];
    out.extend_from_slice(&dir_head("data.bin", f1.len() as u64, 1));
    out.extend_from_slice(f1);
    out.extend_from_slice(&dir_head(".", 0, 3)); // 退出 sub
    out.extend_from_slice(&dir_head(".", 0, 3)); // 退出根
    out
}

/// 假对端作为 TCP 客户端，用 GETDIRFILES 取回整棵目录树（相对路径 → 内容）
async fn fetch_dir(
    port_app: u16,
    pkt_no: u32,
    file_id: u32,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port_app)))
            .await
            .map_err(|_| "连接超时".to_string())?
            .map_err(|e| format!("connect: {e}"))?;
    let req = format!(
        "1:{}:fake:fake-host:{}:{}:{}:0\n",
        proto::next_packet_no(),
        cmd::GETDIRFILES,
        pkt_no,
        file_id
    );
    stream.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| "读取目录流超时".to_string())?
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < raw.len() {
        let colon = raw[i..].iter().position(|&b| b == b':').ok_or("头部缺少冒号")? + i;
        let head_len = usize::from_str_radix(
            String::from_utf8_lossy(&raw[i..colon]).trim(),
            16,
        )
        .map_err(|e| format!("头部长度: {e}"))?;
        if i + head_len > raw.len() {
            return Err("头部越界".into());
        }
        let head = String::from_utf8_lossy(&raw[i..i + head_len]).into_owned();
        i += head_len;
        let fields: Vec<&str> = head.trim_end_matches(':').split(':').collect();
        if fields.len() < 4 {
            return Err(format!("头部字段不足: {head:?}"));
        }
        let name = fields[1].to_string();
        let size = u64::from_str_radix(fields[2], 16).map_err(|e| e.to_string())? as usize;
        let attr = u32::from_str_radix(fields[3], 16).map_err(|e| e.to_string())?;
        match attr {
            2 => stack.push(name),
            3 => {
                if stack.pop().is_none() {
                    return Err("多余的返回上级".into());
                }
                if stack.is_empty() {
                    break;
                }
            }
            _ => {
                if i + size > raw.len() {
                    return Err("内容越界".into());
                }
                // 相对路径不含根目录名，便于与源目录直接比对
                let rel = stack
                    .iter()
                    .skip(1)
                    .cloned()
                    .chain(std::iter::once(name))
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, raw[i..i + size].to_vec()));
                i += size;
            }
        }
    }
    out.sort();
    Ok(out)
}

/// 递归读取本地目录为 (相对路径, 内容) 列表，用于比对
fn read_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            match e.metadata() {
                Ok(m) if m.is_dir() => walk(&e.path(), &rel, out),
                Ok(m) if m.is_file() => {
                    out.push((rel, std::fs::read(e.path()).unwrap_or_default()))
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// 假对端作为 TCP 客户端，从应用侧 GETFILEDATA 取文件
async fn fetch_file(port_app: u16, pkt_no: u32, file_id: u32) -> Result<Vec<u8>, String> {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port_app)))
            .await
            .map_err(|_| "连接超时".to_string())?
            .map_err(|e| format!("connect: {e}"))?;
    let req = format!(
        "1:{}:fake:fake-host:{}:{}:{}:0\n",
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
