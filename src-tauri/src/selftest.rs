//! 无头自检模式 (`--selftest`)：
//! 在本机回环地址上启动完整网络栈，并内置一个「假对端」完成互通验证：
//!   1. 上线发现（BR_ENTRY → ANSENTRY 注册）
//!   2. 文本消息收发（双向，含 READMSG 回执与陌生来源注册）
//!   3. 发送附件 —— 假对端作为 TCP 客户端把文件完整取回并逐字节比对
//!   4. 接收附件 —— 假对端作为 TCP 服务端供我们下载并逐字节比对
//!   5. 下线广播（BR_EXIT）
//! 第二个场景（双实例加密全链路）拉起两套完整网络栈 A/B：
//!   1. 单播发现 → 双向预握手 → 双方 peer_keys.json 各缓存对方公钥
//!   2. A→B 文本密文送达，B 落库 enc=true 且 sig_ok=true
//!   3. B→A 反向同样断言
//!   4. 加密文件传输：双方互发附件，取回请求带 ENCRYPTOPT|ENCFILEOPT、
//!      正文双向过 AES-CTR，逐字节一致；服务端 diag.log 留 tcp-hit enc=1 标记
//!      （Task 10）；另以非零偏移（777，十进制）直调下载路径验证断点续传
//!      密钥流对齐与偏移的十进制编码
//!   5. 能力撤回：encrypt=false 的实例 C 以同一 IP 上线 → A 丢弃缓存 →
//!      后续发送回退明文，C 落库 enc=false
//! 第三个场景（离线文件投递）：实例 A 对不在线的 B 发送附件 → 入队（含路径）；
//! B 上线后 A 经上线应答自动重投（带延迟尾注，重投时重新登记文件槽）→
//! B 取回附件逐字节一致 → RECVMSG 确认后 A 清空待投递队列。
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
    let plain_ok = rt.block_on(async_run());
    let crypto_ok = rt.block_on(crypto_roundtrip());
    let offline_ok = rt.block_on(offline_file_delivery());
    let extended_ok = rt.block_on(extended_protocols());
    plain_ok && crypto_ok && offline_ok && extended_ok
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

    let ctx = net::start_network_quiet(st.clone(), port_app)
        .await
        .expect("start network");
    println!("[..] 网络栈已启动 (UDP/TCP 端口 {port_app})");

    /* ---------------- 假对端 ---------------- */
    let fake_content: Vec<u8> = b"FAKE-PEER-FILE-CONTENT-".repeat(4000); // ~92KB
    let offer_pkt_no = proto::next_packet_no();
    let shared = Arc::new(Mutex::new(PeerShared::default()));
    let img_pkt_no = proto::next_packet_no();
    let empty_pkt_no = proto::next_packet_no();
    let clip_pkt_no = proto::next_packet_no();
    let peer_tasks = spawn_fake_peer(
        port_peer,
        port_app,
        shared.clone(),
        fake_content.clone(),
        offer_pkt_no,
        img_pkt_no,
        empty_pkt_no,
        clip_pkt_no,
    );
    println!("[..] 假对端已就绪 (端口 {port_peer})");
    // 假对端只绑定在 127.0.0.1，广播到不了它 —— 按真实场景做单播发现
    tokio::time::sleep(Duration::from_millis(60)).await;
    let peer_addr: SocketAddr = format!("127.0.0.1:{port_peer}").parse().unwrap();
    net::announce_unicast(&ctx, &[peer_addr]).await;

    let mut log = Log(vec![]);
    // 会话身份 = 对端 IP（源端口不再参与去重，见 upsert_peer 注释）
    let peer_key = "127.0.0.1".to_string();

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
    std::fs::create_dir_all(send_dir.join("空子目录")).unwrap(); // 空目录也要能传过去
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
        p.fetched_dirs
            .last()
            .map(|t| {
                let files: Vec<_> = t.iter().filter(|(n, _)| !n.ends_with('/')).cloned().collect();
                files == want_tree
            })
            .unwrap_or(false)
    })
    .await;
    log.check("假对端经 GETDIRFILES 取回整棵目录树且逐字节一致", dir_fetch_ok);
    log.check(
        "发送目录：空子目录也在流里（对端能原样重建）",
        shared
            .lock()
            .unwrap()
            .fetched_dirs
            .last()
            .map(|t| t.iter().any(|(n, _)| n == "空子目录/"))
            .unwrap_or(false),
    );

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

    /* ---- 4a-3. 空文件（0 字节）下载：0 字节 = 完整传输，必须记为成功 ---- */
    {
        let offered = wait_for(3000, || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(e, v)| e == "msg-in" && v["msg"]["text"] == "请收空文件")
        })
        .await;
        log.check("收到空文件的入站公告", offered);
        let dl = if offered {
            net::download_file_task(
                &ctx,
                &peer_key,
                empty_pkt_no,
                13,
                "空文件.txt",
                "",
                0,
                false,
            )
            .await
        } else {
            Err("空文件公告未到达，跳过下载".into())
        };
        match dl {
            Ok(path) => {
                let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(u64::MAX);
                log.check("空文件下载成功且落盘为 0 字节", len == 0);
                let hist = st.read_history(&peer_key, 30);
                let state_ok = hist
                    .iter()
                    .rev()
                    .find(|r| r["pkt"].as_u64() == Some(empty_pkt_no as u64))
                    .and_then(|r| r["files"][0]["state"].as_str())
                    .map(|s| s == "done")
                    .unwrap_or(false);
                log.check("空文件历史记录状态记为 done（而非 failed）", state_ok);
            }
            Err(e) => {
                log.check(&format!("空文件下载失败: {e}"), false);
            }
        }
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
                "接收目录：空子目录也被建出来",
                path.join("空目录").is_dir(),
            );
            log.check(
                "接收目录：符号链接等非常规条目不落盘（内容被正确跳过）",
                !path.join("链接").exists(),
            );
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

    /* ---- 4c2. 官方「粘贴图片」（FILEATTACHOPT+attr=0x20+ipmsgclip_s_*.png）自动接收 ---- */
    let clip_seen = wait_for(8000, || {
        st.read_history(&peer_key, 80).iter().any(|r| {
            r["pkt"].as_u64() == Some(clip_pkt_no as u64)
                && r["files"].as_array().is_some_and(|a| a.len() == 1)
                && r["files"][0]["state"] == "done"
        })
    })
    .await;
    log.check(
        "官方粘贴图片：attr=0x20(IPMSG_FILE_CLIPBOARD) 附件自动接收为已完成",
        clip_seen,
    );
    if clip_seen {
        let r = st
            .read_history(&peer_key, 80)
            .into_iter()
            .find(|r| r["pkt"].as_u64() == Some(clip_pkt_no as u64))
            .unwrap_or_default();
        let p = r["files"][0]["path"].as_str().unwrap_or("");
        let bytes = std::fs::read(p).unwrap_or_default();
        log.check(
            "官方粘贴图片字节与公告一致",
            bytes.starts_with(b"\xFF\xD8\xFF\xE0clip-jpeg-content") && bytes.len() == 85,
        );
        log.check(
            "官方粘贴图片保留 ipmsgclip 文件名与 .png 扩展名（内联预览的前提）",
            p.to_lowercase().ends_with(".png")
                && r["files"][0]["name"]
                    .as_str()
                    .map(|n| n.starts_with("ipmsgclip_s_"))
                    .unwrap_or(false),
        );
        log.check(
            "官方粘贴图片消息正文与附件同屏",
            r["text"] == "粘贴的图片" && r["kind"] == "file",
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
    // 出站方向：我们发的消息带 READCHECKOPT，假对端回复 READMSG 后应标记为已读。
    // 注意：read_history 只保留末尾 limit 条，前面的新增记录（如空文件公告）会
    // 把最早的消息挤出窗口 —— 这里按文本找最早的出站消息，limit 必须足够大
    let out_read = wait_for(3000, || {
        st.read_history(&peer_key, 50)
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
    let ctx2 = net::start_network_quiet(st2.clone(), port_app2)
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

    /* ---- 5b-4. 会话摘要的未读计数与历史 read 标志逐条一致 ---- */
    // 前端启动时用 list_sessions 的未读摘要补回「WebView 就绪前被丢弃的
    // msg-in 事件」（对端离线留言在我方上线瞬间重投即属此列），摘要必须与
    // 历史记录里的持久化 read 标志一致：重投继承已读的 777123 不得再计。
    let mut sess_ok = false;
    let mut sess_snap = String::new();
    if let Some(s) = st2.list_sessions().into_iter().find(|s| s.key == peer_key) {
        let hist = st2.read_history(&peer_key, 500);
        let expect = hist
            .iter()
            .filter(|r| r["dir"] == "in" && !r["read"].as_bool().unwrap_or(false))
            .count() as u32;
        let expect_ts = hist
            .iter()
            .filter(|r| r["dir"] == "in" && !r["read"].as_bool().unwrap_or(false))
            .filter_map(|r| r["ts"].as_u64())
            .max()
            .unwrap_or(0);
        sess_ok = s.unread == expect && s.unread_ts == expect_ts;
        sess_snap = format!("摘要 unread={}（期望 {expect}）unread_ts={}（期望 {expect_ts}）", s.unread, s.unread_ts);
    }
    log.check(&format!("会话摘要未读数与会话内未读记录一致（{sess_snap}）"), sess_ok);

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

/* ==================== 场景：双实例端到端加密全链路 ==================== */

/// peer_keys.json 里是否已缓存该 IP 的公钥（读盘断言，重启口径；
    /// 文件为 {rev, keys:{ip:{...}}} 版本化结构）
fn peer_keys_cached(dir: &Path, ip: &str) -> bool {
    std::fs::read_to_string(dir.join("peer_keys.json"))
        .ok()
        .and_then(|txt| serde_json::from_str::<Value>(&txt).ok())
        .and_then(|v| v.get("keys").cloned())
        .map(|keys| keys.get(ip).is_some())
        .unwrap_or(false)
}

/// 双实例加密全链路：握手 → 密文互发 → 能力撤回。
///
/// A、B 是两套完整网络栈（独立 UDP 端口 + 数据目录），均默认 encrypt=true；
/// 回环上所有实例的会话键都是对端 IP「127.0.0.1」，各自读写独立数据目录互不
/// 干扰。撤回环节用第三实例 C（encrypt=false）扮演「同一对端重新上线却不再
/// 声明 ENCRYPTOPT」：A 必须丢弃其公钥缓存，之后的发送回退明文。
async fn crypto_roundtrip() -> bool {
    println!("== 场景：双实例加密全链路 ==");
    let base =
        std::env::temp_dir().join(format!("open-ipmsg-selftest-crypto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (dir_a, dir_b, dir_c) = (base.join("a"), base.join("b"), base.join("c"));
    let port_a = free_udp_port().await;
    let port_b = free_udp_port().await;
    let port_c = free_udp_port().await;
    let key = "127.0.0.1";
    // B 独占 127.0.0.2：A↔B 会话的会话键是 127.0.0.2，与 127.0.0.1 上任何
    // 外来流量（本机真实客户端应答等）完全隔离，预握手断言才确定。
    // C 也绑 127.0.0.2 并接管这个会话键 —— 撤回场景的语义就是
    // 「同一 IP 换成新实例重新上线」。
    let key_b = "127.0.0.2";
    let mut log = Log(vec![]);

    /* -- 实例 A / B：Config::default 的 encrypt 即为 true -- */
    let st_a = Arc::new(AppState::new(dir_a.clone()));
    let mut cfg_a = Config::default();
    cfg_a.nickname = "加密实例A".into();
    cfg_a.group = "测试组".into();
    cfg_a.encoding = "utf8".into();
    cfg_a.download_dir = dir_a.join("dl").to_string_lossy().into_owned();
    st_a.set_config(cfg_a);
    let ctx_a = net::start_network_quiet(st_a.clone(), port_a).await.expect("start A");

    let st_b = Arc::new(AppState::new(dir_b.clone()));
    let mut cfg_b = Config::default();
    cfg_b.nickname = "加密实例B".into();
    cfg_b.group = "测试组".into();
    cfg_b.encoding = "utf8".into();
    cfg_b.download_dir = dir_b.join("dl").to_string_lossy().into_owned();
    st_b.set_config(cfg_b);
    let ctx_b =
        net::start_network_loopback(st_b.clone(), key_b.parse().unwrap(), port_b)
            .await
            .expect("start B");

    /* ---- 1. A 单播发现 B；双方各走一遍 GETPUBKEY 预握手 ---- */
    tokio::time::sleep(Duration::from_millis(60)).await;
    let addr_b: SocketAddr = format!("{key_b}:{port_b}").parse().unwrap();
    net::announce_unicast(&ctx_a, &[addr_b]).await;

    let discovered = wait_for(2500, || st_a.peers.lock().unwrap().contains_key(key_b)).await;
    log.check("发现实例 B（BR_ENTRY→ANSENTRY 注册）", discovered);
    // B 缓存了 A 的公钥 = B 侧预握手已完成（peer_keys.json 非空且含回环键）
    let b_cached_a = wait_for(5000, || peer_keys_cached(&dir_b, key)).await;
    log.check("A 广播声明 ENCRYPTOPT → B 完成预握手并持久化 A 的公钥", b_cached_a);
    let a_cached_b = wait_for(5000, || peer_keys_cached(&dir_a, key_b)).await;
    log.check("B 应答声明 ENCRYPTOPT → A 完成预握手并持久化 B 的公钥", a_cached_b);
    if !b_cached_a || !a_cached_b {
        return finish_crypto(log, &base);
    }

    /* ---- 2. A→B 文本：密封发出，B 解密落库 ---- */
    let text_ab = "密文互发：A 到 B";
    match net::send_message(&ctx_a, key_b, text_ab, vec![]).await {
        Ok(rec) => log.check(
            "A 发送返回 out 记录且实际密文发出（enc=true）",
            rec["dir"] == "out" && rec["enc"] == true,
        ),
        Err(e) => {
            eprintln!("      A 发送错误: {e}");
            log.check("A 发送返回 out 记录且实际密文发出（enc=true）", false);
        }
    }
    let got_in_b = wait_for(4000, || {
        st_b.read_history(key, 50).iter().any(|r| {
            r["dir"] == "in" && r["text"] == text_ab && r["enc"] == true && r["sig_ok"] == true
        })
    })
    .await;
    log.check("B 解密落库：文本一致且 enc=true、sig_ok=true", got_in_b);

    /* ---- 2b. 长文本自动分段：整段超出密封明文预算（3400B）时自动拆条
           发送，每条独立包号且全部加密；B 收到后能按序拼接还原原文 ---- */
    let text_long = format!("长文本分段测试：{}结尾", "密".repeat(1800)); // ≈5400B+
    match net::send_message_multi(&ctx_a, key_b, &text_long, vec![]).await {
        Ok(recs) => {
            // 在线直发记录无 queued 字段；用 as_bool 判“非离线入队”
            let all_enc = !recs.is_empty()
                && recs
                    .iter()
                    .all(|r| r["dir"] == "out" && r["enc"] == true
                        && r.get("queued").and_then(|v| v.as_bool()).unwrap_or(false) == false);
            log.check(
                "A 长文本自动分段：返回 ≥2 条 out 记录且全部密文发出",
                recs.len() >= 2 && all_enc,
            );
        }
        Err(e) => {
            eprintln!("      A 长文本发送错误: {e}");
            log.check("A 长文本自动分段：返回 ≥2 条 out 记录且全部密文发出", false);
        }
    }
    let got_long_b = wait_for(4000, || {
        let incoming: Vec<String> = st_b
            .read_history(key, 100)
            .iter()
            .filter(|r| r["dir"] == "in" && r["enc"] == true)
            .filter_map(|r| r["text"].as_str().map(String::from))
            .collect();
        incoming.join("").contains(&text_long)
    })
    .await;
    log.check(
        "B 收到全部分段并还原长文本（每段独立解密 enc=true）",
        got_long_b,
    );

    /* ---- 2c. 附件 + 长文本：首段带附件、其余纯文本分段，全部密文 ---- */
    let attach_path = dir_a.join("long_attach.bin");
    std::fs::write(&attach_path, vec![0x5Au8; 12345]).unwrap();
    let text_attach = format!("附件长文本：{}结尾", "密".repeat(1800));
    let recs_attach = match net::send_message_multi(
        &ctx_a,
        key_b,
        &text_attach,
        vec![attach_path.to_string_lossy().into_owned()],
    )
    .await
    {
        Ok(recs) => recs,
        Err(e) => {
            eprintln!("      A 附件+长文本发送错误: {e}");
            vec![]
        }
    };
    let ok_attach = recs_attach.len() >= 2
        && recs_attach[0]["files"].as_array().is_some_and(|f| !f.is_empty())
        && recs_attach.iter().all(|r| {
            r["dir"] == "out"
                && r["enc"] == true
                && r.get("queued").and_then(|v| v.as_bool()).unwrap_or(false) == false
        });
    log.check(
        "A 附件+长文本：首段带附件、共 ≥2 条且全部密文发出",
        ok_attach,
    );
    let attach_pkt = recs_attach
        .first()
        .and_then(|r| r["pkt"].as_u64())
        .unwrap_or(0) as u32;
    let got_attach_b = wait_for(4000, || {
        let hist = st_b.read_history(key, 100);
        let has_announce = hist.iter().any(|r| {
            r["dir"] == "in"
                && r["pkt"].as_u64() == Some(attach_pkt as u64)
                && r["enc"] == true
                && r["files"].as_array().is_some_and(|f| !f.is_empty())
        });
        let texts: String = hist
            .iter()
            .filter(|r| r["dir"] == "in" && r["enc"] == true)
            .filter_map(|r| r["text"].as_str().map(String::from))
            .collect::<Vec<_>>()
            .join("");
        has_announce && texts.contains(&text_attach)
    })
    .await;
    log.check(
        "B 解密附件公告（带文件条目）并收到全部分段还原长文本",
        got_attach_b,
    );

    /* ---- 3. 反向 B→A 同样断言 ---- */
    let text_ba = "密文互发：B 回 A";
    match net::send_message(&ctx_b, key, text_ba, vec![]).await {
        Ok(rec) => log.check(
            "B 发送返回 out 记录且实际密文发出（enc=true）",
            rec["dir"] == "out" && rec["enc"] == true,
        ),
        Err(e) => {
            eprintln!("      B 发送错误: {e}");
            log.check("B 发送返回 out 记录且实际密文发出（enc=true）", false);
        }
    }
    let got_in_a = wait_for(4000, || {
        st_a.read_history(key_b, 50).iter().any(|r| {
            r["dir"] == "in" && r["text"] == text_ba && r["enc"] == true && r["sig_ok"] == true
        })
    })
    .await;
    log.check("A 解密落库：文本一致且 enc=true、sig_ok=true", got_in_a);

    /* ---- 3b. A 公告文件，B 以加密取回请求下载（正文过 AES-CTR） ---- */
    let content_ab: Vec<u8> = (0..150_000u32).map(|i| ((i * 17 + 5) % 253) as u8).collect();
    let path_ab = dir_a.join("enc_upload.bin");
    std::fs::write(&path_ab, &content_ab).unwrap();
    let rec_file_ab = match net::send_message(
        &ctx_a,
        key_b,
        "加密文件给你",
        vec![path_ab.to_string_lossy().into_owned()],
    )
    .await
    {
        Ok(rec) => {
            log.check(
                "A 的文件公告以密文发出（enc=true）",
                rec["dir"] == "out" && rec["enc"] == true,
            );
            Some(rec)
        }
        Err(e) => {
            eprintln!("      A 发送文件公告错误: {e}");
            log.check("A 的文件公告以密文发出（enc=true）", false);
            None
        }
    };
    // 公告包号/文件 ID 直接取自 out 记录；B 解密登记后按同值提供下载槽位
    let ab_pkt = rec_file_ab
        .as_ref()
        .and_then(|r| r["pkt"].as_u64())
        .unwrap_or(0) as u32;
    let ab_id = rec_file_ab
        .as_ref()
        .and_then(|r| r["files"][0]["id"].as_u64())
        .unwrap_or(0) as u32;
    let registered_at_b = wait_for(4000, || {
        st_b
            .read_history(key, 50)
            .iter()
            .any(|r| r["dir"] == "in" && r["pkt"].as_u64() == Some(ab_pkt as u64))
    })
    .await;
    log.check("B 解密并登记 A 的文件公告", registered_at_b);
    match net::download_file_task(
        &ctx_b,
        key,
        ab_pkt,
        ab_id,
        "enc_upload.bin",
        "",
        content_ab.len() as u64,
        false,
    )
    .await
    {
        Ok(path) => {
            let saved = std::fs::read(&path).unwrap_or_default();
            log.check("B 经加密流取回 A 的文件且逐字节一致", saved == content_ab);
        }
        Err(e) => {
            eprintln!("      B 加密下载错误: {e}");
            log.check("B 经加密流取回 A 的文件且逐字节一致", false);
        }
    }

    /* ---- 3b-2. 断点续传腿：非零偏移的加密取回（终审修复回归钉） ---- */
    // 直接走下载路径 open_transfer 以偏移 777 续传：777 的十六进制是全数字
    // 的 309 —— 若密封内层把偏移误编码成十六进制，A 的服务端按十进制优先
    // 解析（num_flex_dec_first）会读成 309：不仅密钥流错位，还会从错误的
    // 文件位置续传。断言从 777 起读到的字节与源文件尾部逐字节一致，且
    // A 的 diag 里 offset 按十进制落为 777。
    {
        let target_a: SocketAddr = format!("127.0.0.1:{port_a}").parse().unwrap();
        const RESUME_OFF: u64 = 777;
        match net::open_transfer(
            &ctx_b,
            target_a,
            ab_pkt,
            ab_id,
            "",
            cmd::GETFILEDATA,
            net::DIALECTS[1],
            RESUME_OFF,
        )
        .await
        {
            Ok((mut stream, enc)) => {
                let mut rest = Vec::new();
                let got =
                    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut rest))
                        .await;
                log.check("续传腿确认加密流（enc=true）", enc);
                match got {
                    Ok(Ok(_)) => log.check(
                        &format!("断点续传：从偏移 {RESUME_OFF} 续取加密流且逐字节一致"),
                        rest == content_ab[RESUME_OFF as usize..],
                    ),
                    Ok(Err(e)) => {
                        eprintln!("      续传读取错误: {e}");
                        log.check("断点续传：从非零偏移续取加密流", false);
                    }
                    Err(_) => log.check("断点续传：读取超时", false),
                }
            }
            Err(e) => {
                eprintln!("      续传请求错误: {e}");
                log.check(&format!("断点续传：请求失败: {e}"), false);
            }
        }
        // 服务端解析出的续传偏移必须就是十进制 777（若收到十六进制 "309" 并
        // 误读，这里会是 offset=309）
        let diag_a = std::fs::read_to_string(dir_a.join("diag.log")).unwrap_or_default();
        log.check(
            &format!("服务端按十进制解析续传偏移（diag 记录 offset={RESUME_OFF}）"),
            diag_a.contains(&format!("offset={RESUME_OFF}")),
        );
    }

    /* ---- 3c. 反向：B 公告文件，A 加密下载；加密请求标记必须落在 B 的日志里 ---- */
    let content_ba: Vec<u8> = (0..90_000u32).map(|i| ((i * 23 + 11) % 251) as u8).collect();
    let path_ba = dir_b.join("enc_reply.bin");
    std::fs::write(&path_ba, &content_ba).unwrap();
    let rec_file_ba = match net::send_message(
        &ctx_b,
        key,
        "回赠加密文件",
        vec![path_ba.to_string_lossy().into_owned()],
    )
    .await
    {
        Ok(rec) => Some(rec),
        Err(e) => {
            eprintln!("      B 发送文件公告错误: {e}");
            log.check("B 的文件公告以密文发出（enc=true）", false);
            None
        }
    };
    if let Some(rec) = rec_file_ba.as_ref() {
        log.check(
            "B 的文件公告以密文发出（enc=true）",
            rec["dir"] == "out" && rec["enc"] == true,
        );
        let ba_pkt = rec["pkt"].as_u64().unwrap_or(0) as u32;
        let ba_id = rec["files"][0]["id"].as_u64().unwrap_or(0) as u32;
        let registered_at_a = wait_for(4000, || {
            st_a
                .read_history(key_b, 50)
                .iter()
                .any(|r| r["dir"] == "in" && r["pkt"].as_u64() == Some(ba_pkt as u64))
        })
        .await;
        log.check("A 解密并登记 B 的文件公告", registered_at_a);
        match net::download_file_task(
            &ctx_a,
            key_b,
            ba_pkt,
            ba_id,
            "enc_reply.bin",
            "",
            content_ba.len() as u64,
            false,
        )
        .await
        {
            Ok(path) => {
                let saved = std::fs::read(&path).unwrap_or_default();
                log.check("A 经加密流取回 B 的文件且逐字节一致", saved == content_ba);
            }
            Err(e) => {
                eprintln!("      A 加密下载错误: {e}");
                log.check("A 经加密流取回 B 的文件且逐字节一致", false);
            }
        }
    }
    {
        // 服务端收到 ENCRYPTOPT 取文件请求才会写这个标记 —— 有它才能证明
        // 走的是加密路径而不是明文回退。注意内层含会话钥，绝不落日志。
        let diag_b = std::fs::read_to_string(dir_b.join("diag.log")).unwrap_or_default();
        log.check(
            "B 的 diag.log 含 tcp-hit enc=1（加密取文件请求被服务端真实解封）",
            diag_b.contains("tcp-hit enc=1"),
        );
    }

    /* ---- 4. 能力撤回：encrypt=false 的实例 C 以 B 的 IP 上线 ---- */
    // 与真实场景同构：老对手关掉加密后重新上线，广播里不再有 ENCRYPTOPT。
    // C 抢占 B 的会话键（127.0.0.2）：A 视角下会话键不变，只是投递端口随
    // 最新报文刷新到 C —— 撤回语义与 B 在不在线无关，只认「同 IP 不再声明
    // 加密能力」。
    let st_c = Arc::new(AppState::new(dir_c.clone()));
    let mut cfg_c = Config::default();
    cfg_c.nickname = "明文实例C".into();
    cfg_c.group = "测试组".into();
    cfg_c.encoding = "utf8".into();
    cfg_c.encrypt = false; // 撤回方：上线通告不再携带 ENCRYPTOPT
    cfg_c.download_dir = dir_c.join("dl").to_string_lossy().into_owned();
    st_c.set_config(cfg_c);
    let ctx_c = net::start_network_loopback(st_c.clone(), key_b.parse().unwrap(), port_c)
        .await
        .expect("start C");
    tokio::time::sleep(Duration::from_millis(60)).await;
    let addr_a: SocketAddr = format!("127.0.0.1:{port_a}").parse().unwrap();
    net::announce_unicast(&ctx_c, &[addr_a]).await;

    let forgotten = wait_for(4000, || !peer_keys_cached(&dir_a, key_b)).await;
    log.check(
        "重新上线却未声明 ENCRYPTOPT → A 撤回该对端公钥缓存（写盘生效）",
        forgotten,
    );

    /* ---- 5. 撤回后 A 只能明文发送：C 收到的记录 enc=false ---- */
    let text_plain = "撤回后的明文消息";
    match net::send_message(&ctx_a, key_b, text_plain, vec![]).await {
        Ok(rec) => log.check(
            "撤回后发送回退明文（out 记录 enc=false）",
            rec["dir"] == "out" && rec["enc"] == false,
        ),
        Err(e) => {
            eprintln!("      A 发送错误: {e}");
            log.check("撤回后发送回退明文（out 记录 enc=false）", false);
        }
    }
    let plain_at_c = wait_for(4000, || {
        // C 的会话键 = A 的 IP（127.0.0.1），与 C 自己绑在哪个地址无关
        st_c.read_history(key, 50).iter().any(|r| {
            r["dir"] == "in" && r["text"] == text_plain && r["enc"] == false
        })
    })
    .await;
    log.check("明文实例 C 收到该消息且记录 enc=false", plain_at_c);

    finish_crypto(log, &base)
}

fn finish_crypto(log: Log, base: &Path) -> bool {
    let ok = log.all_ok();
    let _ = std::fs::remove_dir_all(base);
    println!(
        "== 加密场景{} ==",
        if ok { "全部通过 ✔" } else { "存在失败项 ✘" }
    );
    ok
}

fn finish_offline_file(log: Log, base: &Path) -> bool {
    let ok = log.all_ok();
    let _ = std::fs::remove_dir_all(base);
    println!(
        "== 离线文件投递场景{} ==",
        if ok { "全部通过 ✔" } else { "存在失败项 ✘" }
    );
    ok
}

/// 离线文件投递：对方不在线时附件消息入队，对方上线后带延迟尾注重投，
/// 文件槽在重投时登记，对端可正常取回；RECVMSG 确认后队列清空。
async fn offline_file_delivery() -> bool {
    let base =
        std::env::temp_dir().join(format!("open-ipmsg-offline-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir_a = base.join("a");
    let dir_b = base.join("b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    let port_a = free_udp_port().await;
    let port_b = free_udp_port().await;
    // A 绑 127.0.0.1，B 绑 127.0.0.2：A 眼里 B 是 127.0.0.2，B 眼里 A 是 127.0.0.1
    let key_b = "127.0.0.2";
    let key_a = "127.0.0.1";
    let mut log = Log(vec![]);

    // 实例 A 先启动，B 不在线
    let st_a = Arc::new(AppState::new(dir_a.clone()));
    let mut cfg_a = Config::default();
    cfg_a.nickname = "离线A".into();
    cfg_a.group = "测试组".into();
    cfg_a.encoding = "utf8".into();
    cfg_a.download_dir = dir_a.join("dl").to_string_lossy().into_owned();
    st_a.set_config(cfg_a);
    let ctx_a = net::start_network_quiet(st_a.clone(), port_a)
        .await
        .expect("start A");

    /* ---- 1. 对方离线：带附件消息入队（不再报错） ---- */
    let content: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();
    let path = dir_a.join("offline_upload.bin");
    std::fs::write(&path, &content).unwrap();
    let rec = match net::send_message(
        &ctx_a,
        key_b,
        "离线文件给你",
        vec![path.to_string_lossy().into_owned()],
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("      A 离线发送错误: {e}");
            log.check("对方离线时附件消息入队", false);
            return finish_offline_file(log, &base);
        }
    };
    log.check(
        "对方离线：附件消息返回 queued 的 file 记录",
        rec["queued"] == true && rec["kind"] == "file",
    );
    log.check(
        "离线附件记录带文件条目（气泡可渲染）",
        rec["files"].as_array().map(|a| a.len()) == Some(1)
            && rec["files"][0]["name"] == "offline_upload.bin",
    );
    let queued = wait_for(1500, || st_a.pending_for(key_b).len() == 1).await;
    log.check(
        "离线附件进入待投递队列（含本地路径）",
        queued && st_a.pending_for(key_b)[0].paths.len() == 1,
    );

    /* ---- 2. 附件路径不可读：报错而不是入队 ---- */
    let bogus = dir_a.join("ghost.bin").to_string_lossy().into_owned();
    match net::send_message(&ctx_a, key_b, "不存在的文件", vec![bogus]).await {
        Err(e) if e.contains("无法读取文件") => log.check("离线附件：路径不可读直接报错", true),
        other => log.check(
            &format!("离线附件：路径不可读直接报错（实际 {other:?}）"),
            false,
        ),
    }

    /* ---- 3. B 上线：A 收到应答后自动重投 ---- */
    let st_b = Arc::new(AppState::new(dir_b.clone()));
    let mut cfg_b = Config::default();
    cfg_b.nickname = "离线B".into();
    cfg_b.group = "测试组".into();
    cfg_b.encoding = "utf8".into();
    cfg_b.download_dir = dir_b.join("dl").to_string_lossy().into_owned();
    st_b.set_config(cfg_b);
    let ctx_b = net::start_network_loopback(st_b.clone(), key_b.parse().unwrap(), port_b)
        .await
        .expect("start B");
    let addr_b: SocketAddr = format!("{key_b}:{port_b}").parse().unwrap();
    net::announce_unicast(&ctx_a, &[addr_b]).await;

    // 对方在线的瞬间由上线应答触发重投（flush_pending_for），
    // B 落库的记录应带延迟尾注与文件条目
    let delivered = wait_for(5000, || {
        st_b.read_history(key_a, 50).iter().any(|r| {
            r["dir"] == "in"
                && r["kind"] == "file"
                && r["text"]
                    .as_str()
                    .map(|t| t.contains("Delayed Send"))
                    .unwrap_or(false)
                && r["files"].as_array().map(|a| a.len()) == Some(1)
        })
    })
    .await;
    log.check("B 上线后收到离线附件公告（延迟尾注 + 文件条目）", delivered);

    /* ---- 4. B 从重投公告取回附件（验证重投时登记的文件槽可用） ---- */
    let mut fetched_ok = false;
    if let Some(rec) = st_b.read_history(key_a, 50).into_iter().find(|r| {
        r["dir"] == "in"
            && r["kind"] == "file"
            && r["text"]
                .as_str()
                .map(|t| t.contains("Delayed Send"))
                .unwrap_or(false)
    }) {
        let pkt = rec["pkt"].as_u64().unwrap_or(0) as u32;
        let id = rec["files"][0]["id"].as_u64().unwrap_or(0) as u32;
        let name = rec["files"][0]["name"]
            .as_str()
            .unwrap_or("offline_upload.bin")
            .to_string();
        match net::download_file_task(&ctx_b, key_a, pkt, id, &name, "", content.len() as u64, false)
            .await
        {
            Ok(p) => fetched_ok = std::fs::read(&p).unwrap_or_default() == content,
            Err(e) => {
                eprintln!("      B 取回离线附件错误: {e}");
            }
        }
    }
    log.check("B 从重投公告取回附件且逐字节一致", fetched_ok);

    /* ---- 5. 送达确认后队列清空 ---- */
    let drained = wait_for(4000, || st_a.pending_for(key_b).is_empty()).await;
    log.check("B 回 RECVMSG 确认后 A 清空待投递队列", drained);

    finish_offline_file(log, &base)
}

/* ==================== 假对端实现 ==================== */

fn spawn_fake_peer(
    port_peer: u16,
    port_app: u16,
    shared: Arc<Mutex<PeerShared>>,
    serve_content: Vec<u8>,
    offer_pkt_no: u32,
    img_pkt_no: u32,
    empty_pkt_no: u32,
    clip_pkt_no: u32,
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
                ext_attrs: vec![],
        };
            let mut extra = "请收文件".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize("utf8").as_bytes());
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
                ext_attrs: vec![],
        };
            let mut extra = "看这张图".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize("utf8").as_bytes());
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

    /* -- 官方「粘贴图片」：FILEATTACHOPT + attr=0x20(IPMSG_FILE_CLIPBOARD) + ipmsgclip_s_*.png
            官方 5.8.6 senddlg.cpp：粘贴图片按普通附件公告（clipCnt 项），文件名
            ipmsgclip_s_<id>_<pos>.png、attr=IPMSG_FILE_CLIPBOARD(0x20)、公告尾附
            IPMSG_FILE_CLIPBOARDPOS=pos 扩展段。前提是对方 Entry 声明 CLIPBOARDOPT。 -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        let clip_bytes = {
            let mut v = b"\xFF\xD8\xFF\xE0clip-jpeg-content".to_vec();
            v.extend_from_slice(&(0..64u8).collect::<Vec<_>>());
            v
        };
        async move {
            tokio::time::sleep(Duration::from_millis(570)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let entry = proto::FileEntry {
                id: 14,
                raw_id: String::new(),
                name: "ipmsgclip_s_14_0.png".into(),
                size: clip_bytes.len() as u64,
                mtime: 123,
                // 官方 IPMSG_FILE_CLIPBOARD=0x20；公告格式 id:name:size:mtime:attr:8=pos:
                attr: 0x20,
                ext_attrs: vec![],
        };
            let mut extra = "粘贴的图片".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize("utf8").as_bytes());
            extra.extend_from_slice(b":8=0"); // IPMSG_FILE_CLIPBOARDPOS 扩展段
            let pkt = proto::Packet {
                pkt_no: clip_pkt_no,
                user: "假对端".into(),
                host: "fake-host".into(),
                command: cmd::SENDMSG | opt::FILEATTACHOPT,
                extra,
            };
            let _ = ps.send_to(&pkt.encode("假对端", "fake-host"), target).await;
        }
    }));

    /* -- 主动提供空文件（0 字节）下载：传输应以成功收尾，而非误判失败 -- */
    tasks.push(tokio::spawn({
        let ps = ps.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(550)).await;
            let target: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
            let entry = proto::FileEntry {
                id: 13,
                raw_id: String::new(),
                name: "空文件.txt".into(),
                size: 0,
                mtime: 123,
                attr: fileattr::REGULAR,
                ext_attrs: vec![],
        };
            let mut extra = "请收空文件".as_bytes().to_vec();
            extra.push(0);
            extra.extend_from_slice(entry.serialize("utf8").as_bytes());
            let pkt = proto::Packet {
                pkt_no: empty_pkt_no,
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
                let pkt_val = fields
                    .next()
                    .and_then(|f| u32::from_str_radix(f.trim(), 16).ok())
                    .filter(|p| {
                        *p == offer_pkt_no || *p == img_pkt_no || *p == empty_pkt_no || *p == clip_pkt_no
                    });
                if pkt_val.is_none() {
                    let _ = stream.shutdown().await;
                    return;
                }
                let clip_content: Vec<u8> = {
                    let mut v = b"\xFF\xD8\xFF\xE0clip-jpeg-content".to_vec();
                    v.extend_from_slice(&(0..64u8).collect::<Vec<_>>());
                    v
                };
                let body = if req.command & 0xFF == cmd::GETDIRFILES {
                    fake_dir_stream()
                } else if pkt_val == Some(empty_pkt_no) {
                    Vec::new() // 空文件：0 字节内容
                } else if pkt_val == Some(clip_pkt_no) {
                    clip_content
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

    // 符号链接条目（官方类型 4）：不该落盘，但其内容必须被跳过，
    // 否则后面的条目会全部错位
    let link_target = b"/etc/passwd";
    out.extend_from_slice(&dir_head("链接", link_target.len() as u64, 4));
    out.extend_from_slice(link_target);

    // 空子目录：进入后立刻返回，接收端也应该把它建出来
    out.extend_from_slice(&dir_head("空目录", 0, 2));
    out.extend_from_slice(&dir_head(".", 0, 3));

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
            2 => {
                stack.push(name);
                // 根目录之外的每一层都记一笔（空目录只能靠这个验证）
                if stack.len() > 1 {
                    out.push((stack[1..].join("/") + "/", Vec::new()));
                }
            }
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

/* ================= 场景四：协议补齐全链路（撤回/不在/广播/封书/密码/重发/主机列表/成员主/代理） ================= */

/// 扩展协议场景：专用假对端 + 多实例，覆盖 docs/ipmsg-protocol-gap.md 的
/// P0/P1/P2 各协议面（DIR_MASTER/AGENT 用本机不同回环 IP 隔离会话键）。
/// 全部通过返回 true；任一失败打 FAIL 行并返回 false。
async fn extended_protocols() -> bool {
    let port_app = free_udp_port().await;
    let port_fake = free_udp_port().await;

    let data_dir =
        std::env::temp_dir().join(format!("open-ipmsg-selftest-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    /* ---- 应用侧实例 A（被测试主体） ---- */
    let st = Arc::new(AppState::new(data_dir.clone()));
    let mut cfg = Config::default();
    cfg.nickname = "扩展自检".into();
    cfg.group = "扩展组".into();
    cfg.encoding = "utf8".into();
    cfg.password_use = true;
    cfg.password = "secret123".into();
    st.set_config(cfg.clone());
    let ctx = net::start_network_quiet(st.clone(), port_app)
        .await
        .expect("start network");
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let ev = events.clone();
        st.set_event(Box::new(move |e, v| ev.lock().unwrap().push((e.to_string(), v))));
    }

    /* ---- 假对端（回显能力） ---- */
    #[derive(Default)]
    struct ExtShared {
        texts: Vec<String>,
        delmsg: Vec<u32>,
        absence_flags: Vec<bool>,
        absence_replies: Vec<String>,
        got_absence_info: bool,
        secret_flags: Vec<bool>,
        passwd_flags: Vec<bool>,
        receipts: Vec<u32>,
        broad_flags: Vec<bool>,
        dup_count: std::collections::HashMap<u32, u32>,
        drop_ack: bool,
        ok_getlist: bool,
        anslists_sent: u32,
    }
    let ext = Arc::new(Mutex::new(ExtShared::default()));
    let std_sock =
        std::net::UdpSocket::bind(("127.0.0.1", port_fake)).expect("ext fake bind");
    std_sock.set_nonblocking(true).unwrap();
    let ps = Arc::new(UdpSocket::from_std(std_sock).expect("ext fake convert"));
    let target_app: SocketAddr = format!("127.0.0.1:{port_app}").parse().unwrap();
    let fake_task = tokio::spawn({
        let ps = ps.clone();
        let ext = ext.clone();
        async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let (n, from) = match ps.recv_from(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                let Some(pkt) = proto::parse(&buf[..n]) else { continue };
                let base = pkt.command & 0xFF;
                match base {
                    cmd::BR_ENTRY => {
                        // 记录不在模式位（ABSENCEOPT）
                        ext.lock().unwrap().absence_flags.push(pkt.command & opt::ABSENCEOPT != 0);
                        let mut a = proto::Packet::new(cmd::ANSENTRY);
                        a.extra = proto::build_entry_extra("扩展假对端", "扩展组", "utf8");
                        let _ = ps.send_to(&a.encode("扩展假对端", "fake-ext"), from).await;
                    }
                    cmd::BR_ABSENCE => {
                        ext.lock().unwrap().absence_flags.push(pkt.command & opt::ABSENCEOPT != 0);
                    }
                    cmd::SENDMSG => {
                        let (need_ack, dup_n) = {
                            let mut sh = ext.lock().unwrap();
                            sh.texts.push(proto::text_of(&pkt));
                            // 重发次数统计：同一包号第二次出现意味着在线重发生效
                            *sh.dup_count.entry(pkt.pkt_no).or_insert(0) += 1;
                            (pkt.command & opt::SENDCHECKOPT != 0 && !sh.drop_ack, sh.dup_count.get(&pkt.pkt_no).copied().unwrap_or(0))
                        }; // 守卫在此释放
                        if need_ack {
                            let mut r = proto::Packet::new(cmd::RECVMSG | opt::AUTORETOPT);
                            r.extra = pkt.pkt_no.to_string().into_bytes();
                            let _ = ps.send_to(&r.encode("扩展假对端", "fake-ext"), from).await;
                        }
                        let _ = dup_n;
                        if pkt.command & opt::BROADCASTOPT != 0 {
                            ext.lock().unwrap().broad_flags.push(true);
                        }
                        if pkt.command & (opt::SECRETOPT | opt::SECRETEXOPT) != 0 {
                            ext.lock().unwrap().secret_flags.push(true);
                        }
                        if pkt.command & opt::PASSWORDOPT != 0 {
                            ext.lock().unwrap().passwd_flags.push(true);
                        }
                        if pkt.command & opt::AUTORETOPT != 0 {
                            // 我们收到的自动应答（不在模式自动回复）
                            ext.lock().unwrap().absence_replies.push(proto::text_of(&pkt));
                        }
                    }
                    cmd::DELMSG => {
                        let no = String::from_utf8_lossy(&pkt.extra).trim().parse().unwrap_or(0);
                        ext.lock().unwrap().delmsg.push(no);
                    }
                    cmd::READMSG => {
                        let no = String::from_utf8_lossy(&pkt.extra).trim().parse().unwrap_or(0);
                        ext.lock().unwrap().receipts.push(no);
                    }
                    cmd::GETABSENCEINFO => {
                        ext.lock().unwrap().got_absence_info = true;
                    }
                    cmd::BR_ISGETLIST => {
                        ext.lock().unwrap().ok_getlist = true;
                        let r = proto::Packet::new(cmd::OKGETLIST);
                        let _ = ps
                            .send_to(
                                &r.encode("扩展假对端", "fake-ext"),
                                from,
                            )
                            .await;
                    }
                    cmd::GETLIST => {
                        // 回一份含「第三方主机」的 ANSLIST（分页续传为 0）
                        let hosts = vec![
                            proto::HostListEntry {
                                user: "假对端".into(),
                                host: "fake-ext".into(),
                                status: cmd::BR_ENTRY,
                                ip: "127.0.0.1".into(),
                                port: port_fake,
                                nick: "扩展假对端".into(),
                                group: "扩展组".into(),
                            },
                            proto::HostListEntry {
                                user: "third".into(),
                                host: "third-host".into(),
                                status: cmd::BR_ENTRY,
                                ip: "127.0.0.9".into(),
                                port: 2425,
                                nick: "第三方".into(),
                                group: String::new(),
                            },
                        ];
                        let (wire, _) = proto::build_anslist(&hosts, 0, 4000, "utf8");
                        let mut r = proto::Packet::new(cmd::ANSLIST | opt::UTF8OPT | opt::AUTORETOPT);
                        r.extra = wire;
                        let _ = ps
                            .send_to(&r.encode("扩展假对端", "fake-ext"), from)
                            .await;
                        ext.lock().unwrap().anslists_sent += 1;
                    }
                    _ => {}
                }
            }
        }
    });
    println!("[..] 扩展场景假对端已就绪 (端口 {port_fake})");

    tokio::time::sleep(Duration::from_millis(60)).await;
    let peer_addr: SocketAddr = format!("127.0.0.1:{port_fake}").parse().unwrap();
    net::announce_unicast(&ctx, &[peer_addr]).await;
    let peer_key = "127.0.0.1".to_string();
    let mut log = Log(vec![]);

    let discovered = wait_for(2500, || st.peers.lock().unwrap().contains_key(&peer_key)).await;
    log.check("E: 扩展假对端发现", discovered);
    if !discovered {
        // 收尾：杀任务
        fake_task.abort();
        return finish(log, vec![], &data_dir);
    }

    /* ---- E1. DELMSG 撤回 ---- */
    let rec = net::send_message(&ctx, &peer_key, "撤回测试消息", vec![])
        .await
        .expect("send");
    let pkt_no = rec["pkt"].as_u64().unwrap() as u32;
    let got = wait_for(2000, || {
        ext.lock()
            .unwrap()
            .texts
            .iter()
            .any(|t| t == "撤回测试消息")
    })
    .await;
    log.check("E: 撤回前的消息已送达", got);
    net::recall_message(&ctx, &peer_key, pkt_no)
        .await
        .expect("recall");
    let recalled = wait_for(2000, || ext.lock().unwrap().delmsg.contains(&pkt_no)).await;
    log.check("E: 对方收到 DELMSG（撤回通知）", recalled);
    // 本地记录标记已撤回
    log.check(
        "E: 本地记录标记 recalled",
        st.find_history_any(&peer_key, pkt_no)
            .map(|r| r["recalled"].as_bool().unwrap_or(false))
            .unwrap_or(false),
    );
    // 对端撤回我们的入站消息
    let in_pkt = proto::next_packet_no();
    let mut p = proto::Packet::new(cmd::SENDMSG).with_pkt_no(in_pkt);
    p.extra = "将被撤回的入站消息".as_bytes().to_vec();
    let _ = ps
        .send_to(&p.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let seen = wait_for(2000, || {
        events.lock().unwrap().iter().any(|(e, v)| {
            e == "msg-in" && v["msg"]["pkt"].as_u64() == Some(in_pkt as u64)
        })
    })
    .await;
    log.check("E: 入站消息就绪", seen);
    let mut d = proto::Packet::new(cmd::DELMSG);
    d.extra = in_pkt.to_string().into_bytes();
    let _ = ps
        .send_to(&d.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let ev_recalled = wait_for(2000, || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|(e, v)| e == "msg-recalled" && v["pkt"].as_u64() == Some(in_pkt as u64))
    })
    .await;
    log.check("E: 对端撤回 → msg-recalled 事件", ev_recalled);

    /* ---- E2. 不在模式（ABSENCE） ---- */
    st.set_absence(true, "开会中，稍后回复");
    net::announce_absence(&ctx).await;
    let abs_ann = wait_for(2000, || ext.lock().unwrap().absence_flags.contains(&true)).await;
    log.check("E: 广播 BR_ABSENCE 且带 ABSENCEOPT", abs_ann);
    // 收到普通消息 → 自动应答不在通知文（AUTORETOPT）
    let mut p2 = proto::Packet::new(cmd::SENDMSG | opt::SENDCHECKOPT);
    p2.extra = "发给不在用户的消息".as_bytes().to_vec();
    let _ = ps
        .send_to(&p2.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let auto_replied = wait_for(2500, || {
        ext.lock()
            .unwrap()
            .absence_replies
            .iter()
            .any(|t| t.contains("开会中，稍后回复"))
    })
    .await;
    log.check("E: 不在模式自动应答（AUTORETOPT）", auto_replied);
    // 主动索取不在通知文
    net::request_absence_info(&ctx, &peer_key)
        .await
        .expect("req absence");
    let probed = wait_for(2000, || ext.lock().unwrap().got_absence_info).await;
    log.check("E: 对端收到 GETABSENCEINFO", probed);
    // 关闭不在模式
    st.set_absence(false, "");
    net::announce_absence(&ctx).await;
    log.check(
        "E: 关闭不在模式后收到消息不再自动应答",
        !wait_for(1200, || {
            ext.lock()
                .unwrap()
                .absence_replies
                .iter()
                .filter(|t| t.contains("开会中，稍后回复"))
                .count()
                >= 2
        })
        .await,
    );

    /* ---- E3. 广播群发（BROADCASTOPT） ---- */
    net::broadcast_message(&ctx, "大家好，这是广播")
        .await
        .expect("broadcast");
    let brod = wait_for(2000, || ext.lock().unwrap().broad_flags.contains(&true)).await;
    log.check("E: 广播群发送达且带 BROADCASTOPT", brod);
    drop(ps);

    /* ---- E4. 封书（SECRETOPT） ---- */
    use crate::net::MsgSendOpts;
    net::send_message_multi_opts(
        &ctx,
        &peer_key,
        "封书消息",
        vec![],
        MsgSendOpts { secret: true, ..Default::default() },
    )
    .await
    .expect("send secret");
    let secret_sent = wait_for(2000, || ext.lock().unwrap().secret_flags.contains(&true)).await;
    log.check("E: 封书发出带 SECRETOPT", secret_sent);

    // 对端发来无密码封书（SECRETEXOPT = SECRET|READCHECK）：正文自动展示但仍未读，
    // 只有会话可见并走现有 mark-read 路径后才回 READMSG。
    let secret_pkt = proto::next_packet_no();
    let mut s = proto::Packet::new(cmd::SENDMSG | opt::SECRETEXOPT).with_pkt_no(secret_pkt);
    s.extra = "请开封查看的封书".as_bytes().to_vec();
    let _ = tokio::net::UdpSocket::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .send_to(&s.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let secret_in = wait_for(2000, || {
        events.lock().unwrap().iter().any(|(e, v)| {
            e == "msg-in" && v["msg"]["pkt"].as_u64() == Some(secret_pkt as u64)
                && v["msg"]["secret"].as_bool() == Some(true)
                && v["msg"]["locked"].as_bool() == Some(false)
                && v["msg"]["unlocked"].as_bool() == Some(true)
                && v["msg"]["read"].as_bool() == Some(false)
        })
    })
    .await;
    log.check("E: 无密码封书入站自动展示但仍未读", secret_in);
    tokio::time::sleep(Duration::from_millis(300)).await;
    log.check(
        "E: 无密码封书在显式标记前不发 READMSG",
        !ext.lock().unwrap().receipts.contains(&secret_pkt),
    );
    let sent1 = net::mark_read_and_receipt(&ctx, &peer_key, &[secret_pkt])
        .await
        .expect("mark secret");
    log.check("E: 显式标记无密码封书后发送 READMSG", sent1 == 1);
    let receipt_seen = wait_for(2000, || {
        ext.lock()
            .unwrap()
            .receipts
            .iter()
            .filter(|&&pkt| pkt == secret_pkt)
            .count()
            == 1
    })
    .await;
    log.check("E: 显式标记后恰好收到一条 READMSG", receipt_seen);
    let sent_again = net::mark_read_and_receipt(&ctx, &peer_key, &[secret_pkt])
        .await
        .expect("mark secret again");
    log.check("E: 重复标记无密码封书不重复发送回执", sent_again == 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    log.check(
        "E: 重复标记后仍只有一条 READMSG",
        ext.lock()
            .unwrap()
            .receipts
            .iter()
            .filter(|&&pkt| pkt == secret_pkt)
            .count()
            == 1,
    );
    log.check(
        "E: 无密码封书记录保持 unlocked",
        st.find_history_pkt(&peer_key, secret_pkt)
            .map(|r| {
                r["secret"].as_bool() == Some(true)
                    && r["locked"].as_bool() == Some(false)
                    && r["unlocked"].as_bool() == Some(true)
                    && r["read"].as_bool() == Some(true)
            })
            .unwrap_or(false),
    );

    /* ---- E5. 密码锁（PASSWORDOPT） ---- */
    let pass_pkt = proto::next_packet_no();
    let mut s2 = proto::Packet::new(cmd::SENDMSG | opt::PASSWORDOPT | opt::READCHECKOPT)
        .with_pkt_no(pass_pkt);
    s2.extra = "密码保护的消息".as_bytes().to_vec();
    let _ = tokio::net::UdpSocket::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .send_to(&s2.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let locked_in = wait_for(2000, || {
        events.lock().unwrap().iter().any(|(e, v)| {
            e == "msg-in" && v["msg"]["pkt"].as_u64() == Some(pass_pkt as u64)
                && v["msg"]["locked"].as_bool() == Some(true)
                && v["msg"]["unlocked"].as_bool() == Some(false)
                && v["msg"]["read"].as_bool() == Some(false)
        })
    })
    .await;
    log.check("E: 密码消息入站保持 locked 且未解锁", locked_in);
    let bad = net::unlock_message(&ctx, &peer_key, pass_pkt, Some("wrong".into())).await;
    log.check("E: 错误密码拒绝开封", bad.is_err());
    log.check(
        "E: 错误密码后消息仍锁定",
        st.find_history_pkt(&peer_key, pass_pkt)
            .map(|r| r["locked"].as_bool() == Some(true) && r["unlocked"].as_bool() == Some(false))
            .unwrap_or(false),
    );
    let good = net::unlock_message(&ctx, &peer_key, pass_pkt, Some("secret123".into())).await;
    log.check("E: 正确密码开封成功", good.is_ok());
    log.check(
        "E: 正确密码后消息解锁",
        st.find_history_pkt(&peer_key, pass_pkt)
            .map(|r| r["locked"].as_bool() == Some(false) && r["unlocked"].as_bool() == Some(true))
            .unwrap_or(false),
    );

    // 线上 PASSWORDOPT 的保护语义不取决于本机是否启用了密码功能。关闭本机密码
    // 功能后，不能把密码封书降级成可见正文，更不能绕过验证直接解锁。
    let mut cfg_password_disabled = st.config();
    cfg_password_disabled.password_use = false;
    cfg_password_disabled.password.clear();
    st.set_config(cfg_password_disabled);
    let disabled_pass_pkt = proto::next_packet_no();
    let mut s3 = proto::Packet::new(
        cmd::SENDMSG | opt::SECRETOPT | opt::PASSWORDOPT | opt::READCHECKOPT,
    )
    .with_pkt_no(disabled_pass_pkt);
    s3.extra = "关闭本机密码时仍需保护的封书".as_bytes().to_vec();
    let _ = tokio::net::UdpSocket::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .send_to(&s3.encode("扩展假对端", "fake-ext"), target_app)
        .await;
    let disabled_pass_locked = wait_for(2000, || {
        events.lock().unwrap().iter().any(|(e, v)| {
            e == "msg-in" && v["msg"]["pkt"].as_u64() == Some(disabled_pass_pkt as u64)
                && v["msg"]["secret"].as_bool() == Some(true)
                && v["msg"]["locked"].as_bool() == Some(true)
                && v["msg"]["unlocked"].as_bool() == Some(false)
                && v["msg"]["read"].as_bool() == Some(false)
        })
    })
    .await;
    log.check(
        "E: 本机关闭密码功能时 PASSWORDOPT 仍保持锁定",
        disabled_pass_locked,
    );
    let disabled_unlock =
        net::unlock_message(&ctx, &peer_key, disabled_pass_pkt, Some("secret123".into())).await;
    log.check(
        "E: 本机关闭密码功能时拒绝解锁 PASSWORDOPT",
        matches!(disabled_unlock, Err(ref e) if e.contains("未启用密码验证")),
    );
    log.check(
        "E: 被拒绝后 PASSWORDOPT 记录仍锁定",
        st.find_history_pkt(&peer_key, disabled_pass_pkt)
            .map(|r| r["locked"].as_bool() == Some(true) && r["unlocked"].as_bool() == Some(false))
            .unwrap_or(false),
    );

    /* ---- E6. 在线重发（SENDCHECKOPT 未确认 → 同包号重发） ---- */
    ext.lock().unwrap().drop_ack = true;
    let drop_pkt_no = {
        let rec = net::send_message(&ctx, &peer_key, "不确认的消息", vec![])
            .await
            .expect("send unacked");
        rec["pkt"].as_u64().unwrap() as u32
    };
    // 重发间隔 4s：7.5s 内应看到同包号 ≥2 次
    let resent = wait_for(7500, || {
        ext.lock().unwrap().dup_count.get(&drop_pkt_no).copied().unwrap_or(0) >= 2
    })
    .await;
    log.check("E: 未确认消息按同包号自动重发", resent);
    ext.lock().unwrap().drop_ack = false;

    /* ---- E7. 主机列表（BR_ISGETLIST/OKGETLIST/GETLIST/ANSLIST） ---- */
    // 手动单播 BR_ISGETLIST 到假对端（广播到不了 127.0.0.1 之外的自检对端）；
    // 先开获取窗口（启动时由 start_network 打开过，但已超时）
    st.open_hostlist_window(30);
    let cfg_now = st.config();
    let probe = proto::Packet::new(cmd::BR_ISGETLIST);
    let _ = ctx
        .sock
        .send_to(
            &probe.encode(&cfg_now.nickname, "ext-self"),
            peer_addr,
        )
        .await;
    let third = wait_for(3000, || {
        st.peers
            .lock()
            .unwrap()
            .contains_key("127.0.0.9")
    })
    .await;
    log.check("E: ANSLIST 第三方主机并入用户表", third);

    /* ---- E8. 成员主目录服务（DIR_MASTER 三实例：主/成员/普通） ---- */
    // M：成员主（127.0.0.7）；U：成员（127.0.0.8，POLL master）。
    // 不同回环 IP 隔离会话键，同时让 DIR_PACKET 的「过滤自身条目」逻辑不会
    // 把测试目标误杀。
    let port_m = free_udp_port().await;
    let port_u = free_udp_port().await;
    let st_m = Arc::new(AppState::new(
        std::env::temp_dir().join(format!("oim-ext-master-{}", std::process::id())),
    ));
    let mut cfg_m = Config::default();
    cfg_m.nickname = "成员主".into();
    cfg_m.dir_mode = "master".into();
    st_m.set_config(cfg_m.clone());
    let ctx_m = net::start_network_loopback(st_m.clone(), "127.0.0.7".parse().unwrap(), port_m)
        .await
        .expect("master net");
    let st_u = Arc::new(AppState::new(
        std::env::temp_dir().join(format!("oim-ext-user-{}", std::process::id())),
    ));
    let mut cfg_u = Config::default();
    cfg_u.nickname = "成员".into();
    cfg_u.dir_mode = "user".into();
    cfg_u.master_addr = format!("127.0.0.7:{port_m}");
    st_u.set_config(cfg_u.clone());
    let ctx_u = net::start_network_loopback(st_u.clone(), "127.0.0.8".parse().unwrap(), port_u)
        .await
        .expect("user net");
    net::dir_tick(&ctx_u).await; // 成员 POLL
    let member_reg = wait_for(3000, || {
        st_m
            .dir_members_snapshot()
            .iter()
            .any(|m| m.key == "127.0.0.8")
    })
    .await;
    log.check("E: 成员主登记 POLL 成员", member_reg);
    let got_packet = wait_for(5000, || {
        st_u.peers.lock().unwrap().contains_key("127.0.0.7")
    })
    .await;
    log.check("E: 成员收到 DIR_PACKET 全网列表", got_packet);
    // 代理任命（POLLAGENT+BROADCAST）与 AGS 窗口
    let agent_win = wait_for(3000, || ctx_u.st.agent_active("127.0.0.7")).await;
    log.check("E: 成员被任命代理（AGS 窗口生效）", agent_win);
    net::dir_tick(&ctx_m).await;
    log.check(
        "E: 主侧周期分发不崩",
        st_m.dir_members_snapshot().len() >= 1,
    );

    /* ---- E9. NAT 中继代理（AGENT 协议：X → AG → Y） ---- */
    let port_ag = free_udp_port().await;
    let port_x = free_udp_port().await;
    let port_y = free_udp_port().await;
    // 三实例各绑独立回环 IP，避免中继会话键混淆
    #[allow(clippy::redundant_clone)]
    let cfg_ag = Config::default();
    let st_ag = Arc::new(AppState::new(
        std::env::temp_dir().join(format!("oim-ext-agent-{}", std::process::id())),
    ));
    st_ag.set_config(cfg_ag);
    let ctx_ag = net::start_network_loopback(st_ag.clone(), "127.0.0.3".parse().unwrap(), port_ag)
        .await
        .expect("agent net");
    let mut cfg_x = Config::default();
    cfg_x.nickname = "中继X".into();
    cfg_x.agent_addr = format!("127.0.0.3:{port_ag}");
    let st_x = Arc::new(AppState::new(
        std::env::temp_dir().join(format!("oim-ext-x-{}", std::process::id())),
    ));
    st_x.set_config(cfg_x);
    let ctx_x = net::start_network_loopback(st_x.clone(), "127.0.0.5".parse().unwrap(), port_x)
        .await
        .expect("x net");
    let mut cfg_y = Config::default();
    cfg_y.nickname = "中继Y".into();
    let st_y = Arc::new(AppState::new(
        std::env::temp_dir().join(format!("oim-ext-y-{}", std::process::id())),
    ));
    st_y.set_config(cfg_y);
    let ctx_y = net::start_network_loopback(st_y.clone(), "127.0.0.6".parse().unwrap(), port_y)
        .await
        .expect("y net");

    // Y 先向代理注册自己（回环下代理不知道 Y 的 2425 之外的真实端口）
    {
        let cfg_y = st_y.config();
        let mut hand = proto::Packet::new(cmd::AGENT_PACKET);
        hand.extra = format!("127.0.0.6:127.0.0.6:1:1:fake:fake:1:").into_bytes();
        let ag_addr: SocketAddr = format!("127.0.0.3:{port_ag}").parse().unwrap();
        let _ = ctx_y
            .sock
            .send_to(&hand.encode(&cfg_y.nickname, "y-host"), ag_addr)
            .await;
        // 直连注册完成后，代理应把 127.0.0.6 → Y 的真实地址入库
        let registered = wait_for(2000, || {
            ctx_ag.st.relay_peer("127.0.0.6").is_some()
        })
        .await;
        log.check("E: 代理登记 Y 的真实地址", registered);
    }
    // X 的会话表里手工登记 Y（回环下没有广播发现）
    {
        let mut peers = st_x.peers.lock().unwrap();
        peers.insert(
            "127.0.0.6".into(),
            crate::state::PeerInfo {
                key: "127.0.0.6".into(),
                ip: "127.0.0.6".into(),
                port: port_y,
                nickname: "中继Y".into(),
                group: String::new(),
                host: "y-host".into(),
                user: "中继Y".into(),
                last_seen: crate::state::now_secs(),
                absence: false,
                absence_text: None,
                vs: None,
            },
        );
    }
    let rec_x = net::send_message(&ctx_x, "127.0.0.6", "经代理的消息", vec![])
        .await
        .expect("send via agent");
    let mut y_got = wait_for(4000, || {
        st_y
            .history_contains_text("127.0.0.5", "经代理的消息")
    })
    .await;
    if !y_got {
        // 与真实客户端同款重试语义：未达则再发一次（同包号，后端去重）
        let _ = net::send_message(&ctx_x, "127.0.0.6", "经代理的消息", vec![]).await;
        y_got = wait_for(4000, || {
            st_y
                .history_contains_text("127.0.0.5", "经代理的消息")
        })
        .await;
    }
    log.check("E: 目标经代理收到消息", y_got);
    let _ = rec_x;
    // X 的重发队列应被 Y 的 RECVMSG（经代理回流）确认清空
    let acked = wait_for(6000, || {
        ctx_x.st.retry_for("127.0.0.6").is_empty()
    })
    .await;
    log.check("E: 代理中继的送达确认回流（重发队列清空）", acked);

    /* ---- E10. 官方 v5 密文消息（EncIPDict）端到端 ---- */
    {
        let sender = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let enc_pkt = 665500;
        let kp_sender = crate::crypto::KeyPair::generate().unwrap();
        let mut inner = crate::ipdict::Dict::new();
        inner
            .put_int(crate::ipdict::DICT_VER, 3)
            .put_int(crate::ipdict::DICT_PKT, enc_pkt)
            .put_str(crate::ipdict::DICT_UID, "官方假对端")
            .put_str(crate::ipdict::DICT_HID, "win-host")
            .put_int(crate::ipdict::DICT_CMD, cmd::SENDMSG as i64)
            .put_int(
                crate::ipdict::DICT_FLG,
                (opt::SENDCHECKOPT | opt::UTF8OPT | opt::ENCRYPTOPT) as i64,
            )
            .put_str(crate::ipdict::DICT_BODY, "1111111");
        let outer =
            crate::crypto::seal_encipdict(&ctx.st.own_keypair().public_key(), &kp_sender, &inner)
                .unwrap();
        let wire = outer.pack();
        sender.send_to(&wire, target_app).await.unwrap();

        let mut ack_buf = [0u8; 1024];
        let (ack_len, _) =
            tokio::time::timeout(Duration::from_secs(2), sender.recv_from(&mut ack_buf))
                .await
                .expect("RECVMSG timeout")
                .expect("RECVMSG recv");
        let ack = proto::parse(&ack_buf[..ack_len]).expect("classic RECVMSG");
        log.check(
            "E: 完整官方 IP2 EncIPDict 解密并通过签名校验",
            ack.command & 0xff == cmd::RECVMSG
                && events.lock().unwrap().iter().any(|(event, value)| {
                    event == "msg-in"
                        && value["msg"]["pkt"].as_u64() == Some(enc_pkt as u64)
                        && value["msg"]["enc"].as_bool() == Some(true)
                        && value["msg"]["sig_ok"].as_bool() == Some(true)
                }),
        );
        log.check(
            "E: EncIPDict RECVMSG 使用 inner PKT",
            proto::text_of(&ack).trim_end_matches('\0') == enc_pkt.to_string(),
        );

        let got_numeric_body = wait_for(3000, || {
            events.lock().unwrap().iter().any(|(e, v)| {
                e == "msg-in"
                    && v["msg"]["pkt"].as_u64() == Some(enc_pkt as u64)
                    && v["msg"]["text"].as_str() == Some("1111111")
                    && v["msg"]["secret"].as_bool() == Some(false)
            })
        })
        .await;
        log.check("E: 普通纯数字 BODY 逐字保留并上屏", got_numeric_body);

        let mut retry_wire = wire.clone();
        retry_wire.extend_from_slice(&[0u8; 64]);
        sender.send_to(&retry_wire, target_app).await.unwrap();

        let (retry_ack_len, _) =
            tokio::time::timeout(Duration::from_secs(2), sender.recv_from(&mut ack_buf))
                .await
                .expect("duplicate RECVMSG timeout")
                .expect("duplicate RECVMSG recv");
        let retry_ack = proto::parse(&ack_buf[..retry_ack_len]).expect("duplicate classic RECVMSG");
        log.check(
            "E: 精确 64-NUL 重投仍回 RECVMSG（inner PKT）",
            retry_ack.command & 0xff == cmd::RECVMSG
                && proto::text_of(&retry_ack).trim_end_matches('\0') == enc_pkt.to_string(),
        );
        let matching_events = events
            .lock()
            .unwrap()
            .iter()
            .filter(|(event, value)| {
                event == "msg-in" && value["msg"]["pkt"].as_u64() == Some(enc_pkt as u64)
            })
            .count();
        log.check("E: 64-NUL 重投不重复 emit", matching_events == 1);
        let matching_records = st
            .read_history(&peer_key, 200)
            .iter()
            .filter(|record| {
                record["dir"] == "in" && record["pkt"].as_u64() == Some(enc_pkt as u64)
            })
            .count();
        log.check("E: 64-NUL 重投历史只落一条", matching_records == 1);

        let secret_pkt = 665501;
        let mut secret_inner = crate::ipdict::Dict::new();
        secret_inner
            .put_int(crate::ipdict::DICT_VER, 3)
            .put_int(crate::ipdict::DICT_PKT, secret_pkt)
            .put_str(crate::ipdict::DICT_UID, "官方假对端")
            .put_str(crate::ipdict::DICT_HID, "win-host")
            .put_int(crate::ipdict::DICT_CMD, cmd::SENDMSG as i64)
            .put_int(
                crate::ipdict::DICT_FLG,
                (opt::SECRETOPT | opt::READCHECKOPT | opt::ENCRYPTOPT | opt::UTF8OPT) as i64,
            )
            .put_str(crate::ipdict::DICT_BODY, "官方封书正文");
        let secret_outer = crate::crypto::seal_encipdict(
            &ctx.st.own_keypair().public_key(),
            &kp_sender,
            &secret_inner,
        )
        .unwrap();
        sender
            .send_to(&secret_outer.pack(), target_app)
            .await
            .unwrap();
        let secret_emitted = wait_for(3000, || {
            events.lock().unwrap().iter().any(|(event, value)| {
                event == "msg-in"
                    && value["msg"]["pkt"].as_u64() == Some(secret_pkt as u64)
                    && value["msg"]["secret"].as_bool() == Some(true)
                    && value["msg"]["unlocked"].as_bool() == Some(true)
                    && value["msg"]["sig_ok"].as_bool() == Some(true)
            })
        })
        .await;
        log.check("E: EncIPDict 保留官方 SECRETOPT 封书语义", secret_emitted);
        log.check(
            "E: EncIPDict 封书历史保留 secret=true 且自动解锁",
            st.read_history(&peer_key, 200).iter().any(|record| {
                record["dir"] == "in"
                    && record["pkt"].as_u64() == Some(secret_pkt as u64)
                    && record["secret"].as_bool() == Some(true)
                    && record["unlocked"].as_bool() == Some(true)
            }),
        );
    }

    /* ---- 收尾 ---- */
    fake_task.abort();
    let all = log.all_ok();
    println!(
        "== 扩展协议场景{} ==",
        if all { "全部通过 ✔" } else { "存在失败项 ✘" }
    );
    all
}
