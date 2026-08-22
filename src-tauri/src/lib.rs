//! Tauri 应用层：命令注册、事件桥接、托盘、通知与生命周期。

mod net;
mod protocol;
mod selftest;
mod state;

pub use state::{AppState, Config, PeerInfo};

use serde::Deserialize;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::{
    menu::{MenuBuilder, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, State, WindowEvent,
};

type SharedState = Arc<AppState>;
type SharedCtx = Arc<net::NetCtx>;

static EXIT_INFO: OnceLock<(Arc<AppState>, u16)> = OnceLock::new();
/// 首次隐藏到托盘时提示一次
static HIDE_NOTIFIED: AtomicBool = AtomicBool::new(false);

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/* ================= 命令 ================= */

#[derive(Deserialize)]
struct ConfigPatch {
    nickname: String,
    group: String,
    download_dir: String,
    encoding: String,
}

/// 配置 + 本机信息（前端设置页展示）
#[tauri::command]
async fn get_config(st: State<'_, SharedState>) -> Result<Value, String> {
    let cfg = st.config();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut ips: Vec<String> = Vec::new();
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifaces {
            if let IpAddr::V4(v4) = ip {
                if !v4.is_loopback() && !ips.contains(&v4.to_string()) {
                    ips.push(v4.to_string());
                }
            }
        }
    }
    Ok(json!({
        "nickname": cfg.nickname,
        "group": cfg.group,
        "download_dir": cfg.download_dir,
        "encoding": cfg.encoding,
        "hostname": hostname,
        "ips": ips,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[tauri::command]
async fn save_config(
    patch: ConfigPatch,
    st: State<'_, SharedState>,
    ctx: State<'_, SharedCtx>,
) -> Result<(), String> {
    if patch.nickname.trim().is_empty() {
        return Err("昵称不能为空".into());
    }
    let cfg = Config {
        nickname: patch.nickname.trim().to_string(),
        group: patch.group.trim().to_string(),
        download_dir: patch.download_dir.trim().to_string(),
        encoding: if patch.encoding.eq_ignore_ascii_case("gbk") {
            "gbk".into()
        } else {
            "utf8".into()
        },
    };
    st.set_config(cfg.clone());
    st.persist_config().map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&cfg.download_dir);
    // 身份变化，立即重新广播
    net::announce(&ctx).await;
    Ok(())
}

#[tauri::command]
async fn get_users(st: State<'_, SharedState>) -> Result<Vec<PeerInfo>, String> {
    let mut users: Vec<PeerInfo> = st.peers.lock().unwrap().values().cloned().collect();
    users.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.nickname.cmp(&b.nickname))
            .then_with(|| a.ip.cmp(&b.ip))
    });
    Ok(users)
}

#[tauri::command]
async fn refresh_users(ctx: State<'_, SharedCtx>) -> Result<(), String> {
    net::announce(&ctx).await;
    Ok(())
}

#[tauri::command]
async fn get_history(
    st: State<'_, SharedState>,
    key: String,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    Ok(st.read_history(&key, limit.unwrap_or(300)))
}

#[tauri::command]
async fn send_text(ctx: State<'_, SharedCtx>, key: String, text: String) -> Result<Value, String> {
    net::send_message(&ctx, &key, &text, vec![]).await
}

#[tauri::command]
async fn send_files(
    ctx: State<'_, SharedCtx>,
    key: String,
    paths: Vec<String>,
) -> Result<Value, String> {
    net::send_message(&ctx, &key, "", paths).await
}

#[tauri::command]
async fn download_file(
    ctx: State<'_, SharedCtx>,
    key: String,
    pkt_no: u32,
    file_id: u32,
    name: String,
    rid: Option<String>,
) -> Result<(), String> {
    // 后台执行；进度与结果通过 file-progress 事件推送
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            net::download_file_task(&ctx, &key, pkt_no, file_id, &name, rid.as_deref().unwrap_or(""))
                .await
        {
            eprintln!("[download] {key} #{file_id} {name}: {e}");
        }
    });
    Ok(())
}

/// 标记入站消息已读，并对要求回执的消息向对端发送 READMSG
#[tauri::command]
async fn mark_read(ctx: State<'_, SharedCtx>, key: String, pkts: Vec<u32>) -> Result<usize, String> {
    let ctx = ctx.inner().clone();
    net::mark_read_and_receipt(&ctx, &key, &pkts).await
}

/* ================= 启动 ================= */

pub fn run() {
    if std::env::args().any(|a| a == "--selftest") {
        let ok = selftest::run();
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 诊断模式：--dump-peers [秒数] 启动网络栈等待后打印用户表（不写用户数据目录）
    if std::env::args().any(|a| a == "--dump-peers") {
        let secs: u64 = std::env::args()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(6);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let dir = std::env::temp_dir()
                .join(format!("open-ipmsg-dumppeers-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let st = Arc::new(AppState::new(dir));
            st.set_config(Config {
                nickname: "诊断节点".into(),
                group: "诊断组".into(),
                encoding: "utf8".into(),
                download_dir: String::new(),
            });
            let _ctx = net::start_network(st.clone(), protocol::DEFAULT_PORT)
                .await
                .expect("start network");
            println!(
                "listening on udp/{}, waiting {secs}s ...",
                protocol::DEFAULT_PORT
            );
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            let mut users: Vec<PeerInfo> = st.peers.lock().unwrap().values().cloned().collect();
            users.sort_by(|a, b| a.key.cmp(&b.key));
            println!("{}", serde_json::to_string_pretty(&users).unwrap());
        });
        std::process::exit(0);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let st = Arc::new(AppState::new(data_dir));
            st.load_config();
            let _ = std::fs::create_dir_all(st.config().download_dir);

            // Rust → 前端 事件桥
            {
                let h = handle.clone();
                st.set_event(Box::new(move |event, value| {
                    use tauri::Emitter;
                    let _ = h.emit(event, value);
                }));
            }

            // 启动网络栈（UDP 发现/消息 + TCP 文件服务）
            // 端口被占用（多半是托盘里还挂着旧实例）时明确报错退出，避免静默失败
            let ctx = match tauri::async_runtime::block_on(net::start_network(
                st.clone(),
                protocol::DEFAULT_PORT,
            )) {
                Ok(c) => c,
                Err(e) => {
                    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                    app.dialog()
                        .message(format!(
                            "端口 {} 被占用（{}）。\n很可能是旧的 Open IPMsg 还在托盘中运行，\
                             请从托盘菜单退出后再启动。",
                            protocol::DEFAULT_PORT, e
                        ))
                        .kind(MessageDialogKind::Error)
                        .title("Open IPMsg 启动失败")
                        .blocking_show();
                    std::process::exit(1);
                }
            };

            // 窗口标题与托盘提示带上版本号和编码模式，便于确认当前运行的构建
            let run_title = {
                let cfg = st.config();
                format!(
                    "Open IPMsg v{} · {}",
                    env!("CARGO_PKG_VERSION"),
                    if crate::protocol::is_utf8_mode(&cfg.encoding) { "UTF-8" } else { "GBK" }
                )
            };
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&run_title);
            }

            // 退出前广播 BR_EXIT（托盘退出 / 进程退出都会走到这里）
            let _ = EXIT_INFO.set((st.clone(), protocol::DEFAULT_PORT));

            app.manage(st);
            app.manage(ctx.clone());

            /* ---------- 系统托盘 ---------- */
            let show_item =
                MenuItem::with_id(&handle, "show", "显示主窗口", true, None::<&str>)?;
            let refresh_item =
                MenuItem::with_id(&handle, "refresh", "刷新在线用户", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(&handle, "quit", "退出", true, None::<&str>)?;
            let menu = MenuBuilder::new(&handle)
                .items(&[&show_item, &refresh_item, &quit_item])
                .build()?;

            let refresh_ctx = ctx.clone();
            TrayIconBuilder::with_id("main-tray")
                .icon(handle.default_window_icon().expect("missing icon").clone())
                .tooltip(&run_title)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "refresh" => {
                        let c = refresh_ctx.clone();
                        tauri::async_runtime::spawn(async move {
                            net::announce(&c).await;
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Windows/macOS：左键单击显示主窗口
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(&handle)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口 → 最小化到托盘（微信式）；真正退出走托盘菜单
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
                if !HIDE_NOTIFIED.swap(true, Ordering::Relaxed) {
                    use tauri_plugin_notification::NotificationExt;
                    let _ = window
                        .notification()
                        .builder()
                        .title("Open IPMsg")
                        .body("已最小化到托盘，右键托盘图标可退出")
                        .show();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_users,
            refresh_users,
            get_history,
            send_text,
            send_files,
            download_file,
            mark_read
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let RunEvent::Exit = event {
                if let Some((st, port)) = EXIT_INFO.get() {
                    net::announce_exit_blocking(st, *port);
                }
            }
        });
}
