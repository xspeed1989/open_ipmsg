//! Tauri 应用层：命令注册、事件桥接、生命周期。

mod net;
mod protocol;
mod selftest;
mod state;

pub use state::{AppState, Config, PeerInfo};

use serde::Deserialize;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use tauri::{Manager, RunEvent, State};

type SharedState = Arc<AppState>;
type SharedCtx = Arc<net::NetCtx>;

static EXIT_INFO: OnceLock<(Arc<AppState>, u16)> = OnceLock::new();

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
) -> Result<(), String> {
    // 后台执行；进度与结果通过 file-progress 事件推送
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = net::download_file_task(&ctx, &key, pkt_no, file_id, &name).await {
            eprintln!("[download] {key} #{file_id} {name}: {e}");
        }
    });
    Ok(())
}

/* ================= 启动 ================= */

pub fn run() {
    if std::env::args().any(|a| a == "--selftest") {
        let ok = selftest::run();
        std::process::exit(if ok { 0 } else { 1 });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
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
            let ctx = tauri::async_runtime::block_on(net::start_network(
                st.clone(),
                protocol::DEFAULT_PORT,
            ))?;

            let _ = EXIT_INFO.set((st.clone(), protocol::DEFAULT_PORT));
            app.manage(st);
            app.manage(ctx);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_users,
            refresh_users,
            get_history,
            send_text,
            send_files,
            download_file
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
