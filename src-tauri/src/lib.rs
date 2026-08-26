//! Tauri 应用层：命令注册、事件桥接、托盘、通知与生命周期。

mod crypto;
mod ipmsg_import;
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

/// 界面文案二选一（config.lang 为空时按系统语言探测，与前端一致）：
/// 托盘菜单、最小化提示等原生 UI 字符串走这里。
fn ui_str(lang: &str, zh: &str, en: &str) -> String {
    let use_en = if lang.eq_ignore_ascii_case("en") {
        true
    } else if lang.is_empty() {
        // 未设置：跟随系统（对应前端 detectLocale：locale 以 zh 开头 → 简体中文）
        !std::env::var_os("LANG")
            .map(|l| l.to_string_lossy().to_lowercase().starts_with("zh"))
            .unwrap_or(false)
    } else {
        false
    };
    if use_en {
        en.into()
    } else {
        zh.into()
    }
}

/// 从托盘唤起主窗口：顺带通知前端「跳到最新的未读会话」。
/// SNI 的回调跑在 DBus 线程上，这里统一回主线程操作窗口。
///
/// 注意：Wayland/KWin 可能拒绝来自外部的 set_focus（防抢焦点）；前端收到
/// open-unread 事件后会自己再 show/unminimize/setFocus 一次 —— 窗口自我激活
/// 任何合成器都无条件允许，保证「窗口可见但被盖住」时点击托盘也能弹到最前面。
fn activate_from_tray(app: &tauri::AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        show_main_window(&app2);
        use tauri::Emitter;
        let _ = app2.emit("open-unread", ());
    });
}

/* ================= 命令 ================= */

#[derive(Deserialize)]
struct ConfigPatch {
    nickname: String,
    group: String,
    download_dir: String,
    encoding: String,
    #[serde(default)]
    theme: Option<String>,
    /// 界面语言：'zh-CN' / 'en'；省略时保留现值（旧前端兼容）
    #[serde(default)]
    lang: Option<String>,
    /// 加密开关；省略时保留现值（旧前端兼容）
    #[serde(default)]
    encrypt: Option<bool>,
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
        "theme": cfg.theme,
        "lang": cfg.lang,
        "encrypt": cfg.encrypt,
        "hostname": hostname,
        "ips": ips,
        "version": env!("CARGO_PKG_VERSION"),
        // 本机公钥指纹：设置页与对端核对密钥用（首次调用会触发生成并落盘）
        "key_fp": st.fingerprint(),
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
    let prev = st.config();
    let cfg = Config {
        nickname: patch.nickname.trim().to_string(),
        group: patch.group.trim().to_string(),
        download_dir: patch.download_dir.trim().to_string(),
        encoding: if patch.encoding.eq_ignore_ascii_case("gbk") {
            "gbk".into()
        } else {
            "utf8".into()
        },
        theme: match patch.theme.as_deref().unwrap_or("system") {
            "light" => "light".into(),
            "dark" => "dark".into(),
            _ => "system".into(),
        },
        // 界面语言白名单；补丁未携带或值非法时保留现值
        lang: match patch.lang.as_deref() {
            Some("zh-CN") => "zh-CN".into(),
            Some("en") => "en".into(),
            _ => prev.lang,
        },
        // 加密开关：补丁未携带时保留现值，避免旧前端保存配置时误关加密
        encrypt: patch.encrypt.unwrap_or(prev.encrypt),
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

/// 全文搜索聊天记录；key 省略则搜索全部会话
#[tauri::command]
async fn search_history(
    st: State<'_, SharedState>,
    query: String,
    key: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    Ok(st.search_history(&query, key.as_deref(), limit.unwrap_or(80)))
}

/// 清空某会话的聊天记录（仅本地，不影响对方）
#[tauri::command]
async fn clear_history(st: State<'_, SharedState>, key: String) -> Result<usize, String> {
    Ok(st.clear_history(&key))
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
    text: Option<String>,
) -> Result<Value, String> {
    // 正文与附件同发（IPMsg 一条消息可同时带正文和附件）；
    // 省略 text 时保持旧语义（纯附件），兼容旧前端
    net::send_message(&ctx, &key, text.as_deref().unwrap_or(""), paths).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn download_file(
    ctx: State<'_, SharedCtx>,
    key: String,
    pkt_no: u32,
    file_id: u32,
    name: String,
    rid: Option<String>,
    size: Option<u64>,
    is_dir: Option<bool>,
) -> Result<(), String> {
    // 后台执行；进度与结果通过 file-progress 事件推送
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            net::download_file_task(
                &ctx,
                &key,
                pkt_no,
                file_id,
                &name,
                rid.as_deref().unwrap_or(""),
                size.unwrap_or(0),
                is_dir.unwrap_or(false),
            )
            .await
        {
            eprintln!("[download] {key} #{file_id} {name}: {e}");
        }
    });
    Ok(())
}

/// 发送剪贴板图片：先落盘到数据目录下的缓存目录，再按普通附件公告出去。
/// IPMsg 协议没有独立的"图片"报文，图片就是一个附件；对端（含官方客户端）
/// 按文件接收，本客户端则会自动接收并在聊天里内联显示。
#[tauri::command]
async fn send_clipboard_image(
    ctx: State<'_, SharedCtx>,
    st: State<'_, SharedState>,
    key: String,
    text: String,
    b64: String,
    mime: String,
) -> Result<Value, String> {
    let path = net::stage_clipboard_image(&st.data_dir, &b64, &mime)?;
    net::send_message(&ctx, &key, &text, vec![path.to_string_lossy().into_owned()]).await
}

/// 读取本地图片并转为 base64 数据（供聊天内联预览）。
/// 仅允许常见位图扩展名，超过 32MB 拒绝预览。
#[tauri::command]
async fn read_image_data(path: String) -> Result<Value, String> {
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        _ => return Err("不支持的图片类型".into()),
    };
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("读取失败: {e}"))?;
    if meta.len() > 32 * 1024 * 1024 {
        return Err("图片过大，不做内联预览".into());
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("读取失败: {e}"))?;
    use base64::Engine as _;
    Ok(json!({
        "mime": mime,
        "b64": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

/* ================= Linux 托盘（自实现 StatusNotifierItem） =================

Tauri 在 Linux 用的是 libappindicator：它只有菜单，**不投递任何点击事件**
（tray-icon 0.24 的 GTK 后端里连点击处理都没有，set_tooltip 也是空实现）。
微信之类的应用能做到「单击托盘打开主界面」，是因为它们自己注册
StatusNotifierItem —— KDE/XFCE 等宿主会对该对象调用 Activate。

这里在 Linux 上同样自己注册 SNI（ksni），于是拿到了：
  - 左键单击 → Activate → 唤起主窗口并跳到最新未读会话
  - 悬停提示（含未读条数），libappindicator 下本来是没有的
  - 图标闪烁（更新图标即可）
Windows/macOS 仍走 Tauri 自带托盘。 */
#[cfg(target_os = "linux")]
mod linux_tray {
    use super::{activate_from_tray, TRAY_IDLE, TRAY_SIZE};
    use ksni::{
        menu::{StandardItem, MenuItem},
        Icon, ToolTip, Tray, TrayMethods,
    };
    use std::sync::OnceLock;

    static HANDLE: OnceLock<ksni::Handle<OimTray>> = OnceLock::new();

    pub struct OimTray {
        pub app: tauri::AppHandle,
        /// 界面语言（config.lang，空视为 zh-CN），决定菜单文案
        pub lang: String,
        /// 闪烁时显示透明帧
        pub blank: bool,
        pub tip: String,
    }

    /// RGBA → SNI 要求的 ARGB32（网络字节序）
    fn to_argb(rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
        }
        out
    }

    impl Tray for OimTray {
        fn id(&self) -> String {
            "open-ipmsg".into()
        }
        fn title(&self) -> String {
            "Open IPMsg".into()
        }
        fn category(&self) -> ksni::Category {
            ksni::Category::Communications
        }
        fn icon_pixmap(&self) -> Vec<Icon> {
            let data = if self.blank {
                vec![0u8; (TRAY_SIZE * TRAY_SIZE * 4) as usize]
            } else {
                to_argb(TRAY_IDLE)
            };
            vec![Icon {
                width: TRAY_SIZE as i32,
                height: TRAY_SIZE as i32,
                data,
            }]
        }
        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: self.tip.clone(),
                ..Default::default()
            }
        }
        /// 左键单击：这正是 libappindicator 给不了的能力
        fn activate(&mut self, _x: i32, _y: i32) {
            activate_from_tray(&self.app);
        }
        fn secondary_activate(&mut self, _x: i32, _y: i32) {
            activate_from_tray(&self.app);
        }
        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: super::ui_str(&self.lang, "显示主窗口", "Show main window"),
                    activate: Box::new(|t: &mut Self| activate_from_tray(&t.app)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: super::ui_str(&self.lang, "刷新在线用户", "Refresh online users"),
                    activate: Box::new(|t: &mut Self| {
                        let app = t.app.clone();
                        tauri::async_runtime::spawn(async move {
                            use tauri::Manager;
                            if let Some(ctx) = app.try_state::<crate::SharedCtx>() {
                                crate::net::announce(&ctx).await;
                            }
                        });
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: super::ui_str(&self.lang, "退出", "Quit"),
                    activate: Box::new(|t: &mut Self| t.app.exit(0)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// 启动 SNI 托盘（失败时返回 false，调用方回退到 Tauri 自带托盘）
    pub async fn spawn(app: tauri::AppHandle, tip: String, lang: String) -> bool {
        match (OimTray {
            app,
            lang,
            blank: false,
            tip,
        })
        .spawn()
        .await
        {
            Ok(h) => {
                let _ = HANDLE.set(h);
                true
            }
            Err(e) => {
                eprintln!("[tray] SNI 注册失败，回退到默认托盘: {e}");
                false
            }
        }
    }

    /// 更新图标（闪烁）与悬停提示
    pub fn update(blank: bool, tip: String) {
        if let Some(h) = HANDLE.get() {
            let h = h.clone();
            tauri::async_runtime::spawn(async move {
                h.update(move |t: &mut OimTray| {
                    t.blank = blank;
                    t.tip = tip;
                })
                .await;
            });
        }
    }

    pub fn is_active() -> bool {
        HANDLE.get().is_some()
    }
}

/* ---------- 托盘未读提示（闪烁） ---------- */

/// 托盘两态图标：直接打包原始 RGBA 像素（64×64），
/// 免去运行时 PNG 解码，也不必为此拉一个图像解码依赖。由 scripts/gen_icons.py 生成。
const TRAY_SIZE: u32 = 64;
const TRAY_IDLE: &[u8] = include_bytes!("../icons/tray.rgba");

/// 全透明帧：与正常图标交替 = 微信那种「图标一闪一闪」。
///
/// 为什么 Windows 上全 0 像素也能透明（曾在此踩过两次坑，已根治）：
/// tray-icon 0.24.x 原本在 Windows 用 CreateIcon 做成「单色 AND mask + 32bpp XOR」
/// 的经典图标，没有真 alpha 通道 —— 任务栏按 mask 绘制，全 0 像素的帧被画成
/// 黑块/马赛克（alpha 全 0 走经典路径；alpha=254 会走合成路径变成近纯黑）。
/// 现已在 vendor/tray-icon 里把 Windows 图标改成 CreateIconIndirect + DIB section
/// 的 32 位 alpha 图标（Electron nativeImage 同款做法，见 Cargo.toml 的 patch 注释），
/// 透明帧由 alpha 通道真正合成，全 0 像素即全透明。macOS / Linux 一直走真 alpha
/// 合成（NSImage / SNI 的 ARGB pixbuf），同样全 0 即可。
static TRAY_BLANK: [u8; (TRAY_SIZE * TRAY_SIZE * 4) as usize] =
    [0u8; (TRAY_SIZE * TRAY_SIZE * 4) as usize];
/// 闪烁间隔：与微信节奏接近
const FLASH_INTERVAL_MS: u64 = 600;
/// 当前是否处于闪烁状态（未读 > 0）
static FLASHING: AtomicBool = AtomicBool::new(false);

/// 切换托盘图标（正常帧 / 透明帧）
fn set_tray_frame(app: &tauri::AppHandle, blank: bool, tip: &str) {
    #[cfg(target_os = "linux")]
    if linux_tray::is_active() {
        linux_tray::update(blank, tip.to_string());
        return;
    }
    let _ = tip;
    set_tray_icon(app, if blank { &TRAY_BLANK } else { TRAY_IDLE });
}

fn set_tray_icon(app: &tauri::AppHandle, rgba: &'static [u8]) {
    // 闪烁循环跑在后台任务里，而托盘底层是 GTK/状态栏对象：
    // 一律回主线程改图标，避免跨线程操作 UI
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = app2.tray_by_id("main-tray") {
            let _ = tray.set_icon(Some(tauri::image::Image::new(rgba, TRAY_SIZE, TRAY_SIZE)));
        }
    });
}

/// 按未读总数更新托盘：有未读就让图标闪烁（图标 ↔ 透明），读完立刻停。
#[tauri::command]
async fn set_unread(app: tauri::AppHandle, total: u32) -> Result<(), String> {
    let title = format!("Open IPMsg v{}", env!("CARGO_PKG_VERSION"));
    let tip = if total > 0 {
        format!("{title} · {total} 条未读")
    } else {
        title
    };
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(&tip));
    }
    #[cfg(target_os = "linux")]
    if linux_tray::is_active() {
        linux_tray::update(false, tip.clone());
    }
    // 窗口标题也带上未读数：最小化后在任务栏/窗口列表里一眼能看到
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_title(&tip);
    }

    if total > 0 {
        // 已经在闪就不再起第二个任务
        if !FLASHING.swap(true, Ordering::SeqCst) {
            let app2 = app.clone();
            let tip2 = tip.clone();
            tauri::async_runtime::spawn(async move {
                let mut blank = false;
                while FLASHING.load(Ordering::SeqCst) {
                    blank = !blank;
                    set_tray_frame(&app2, blank, &tip2);
                    tokio::time::sleep(std::time::Duration::from_millis(FLASH_INTERVAL_MS)).await;
                }
                // 停止时一定回到正常图标，避免停在透明帧上（图标像消失了）
                set_tray_frame(&app2, false, "");
            });
        }
    } else {
        FLASHING.store(false, Ordering::SeqCst);
        set_tray_frame(&app, false, &tip);
    }
    Ok(())
}

/// 读系统剪贴板里的位图（截图后粘贴用）。
///
/// Linux 的 WebKitGTK 不把剪贴板图片暴露给网页（paste 事件拿不到图），
/// 和文件 URI 是同一类缺口 —— 只能直接读 GTK 剪贴板。
/// 返回 PNG 编码的 base64 与字节数；剪贴板里没有图片时返回 null。
#[tauri::command]
async fn clipboard_image(app: tauri::AppHandle) -> Result<Option<Value>, String> {
    #[cfg(target_os = "linux")]
    {
        use base64::Engine as _;
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        // GTK 调用必须在主线程上做
        app.run_on_main_thread(move || {
            let png = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
                .wait_for_image()
                .and_then(|pix| pix.save_to_bufferv("png", &[]).ok());
            let _ = tx.send(png);
        })
        .map_err(|e| format!("读取剪贴板失败: {e}"))?;
        let png = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|e| format!("读取剪贴板超时: {e}"))?;
        Ok(png.map(|bytes| {
            json!({
                "mime": "image/png",
                "size": bytes.len(),
                "b64": base64::engine::general_purpose::STANDARD.encode(&bytes),
            })
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Ok(None)
    }
}

/// 读系统剪贴板里的文件列表（复制文件后粘贴用）。
///
/// Linux 的 WebKitGTK 不会把 `text/uri-list` / `x-special/gnome-copied-files`
/// 暴露给网页，所以在文件管理器里复制文件后，webview 的 clipboardData 是空的
/// —— 只能直接读 GTK 剪贴板。Windows/macOS 的 webview 能自己拿到文件，
/// 这里返回空列表，由前端走 clipboardData 分支。
#[tauri::command]
async fn clipboard_file_paths(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<Vec<String>>();
        // GTK 调用必须在主线程上做
        app.run_on_main_thread(move || {
            let uris = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
                .wait_for_uris()
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<String>>();
            let _ = tx.send(uris);
        })
        .map_err(|e| format!("读取剪贴板失败: {e}"))?;
        let uris = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|e| format!("读取剪贴板超时: {e}"))?;
        Ok(uris
            .iter()
            .filter_map(|u| file_uri_to_path(u))
            .filter(|p| std::path::Path::new(p).exists())
            .collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Ok(Vec::new())
    }
}

/// `file:///home/a%20b.txt` → `/home/a b.txt`；非 file 协议返回 None
fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("file://localhost")
        .or_else(|| uri.strip_prefix("file://"))?;
    let mut out: Vec<u8> = Vec::with_capacity(rest.len());
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let p = String::from_utf8(out).ok()?;
    let p = p.trim_end_matches(['\r', '\n']).to_string();
    if p.is_empty() {
        return None;
    }
    // Windows 形式 file:///C:/x → /C:/x，需要去掉前导斜杠
    #[cfg(target_os = "windows")]
    let p = if p.len() > 2 && p.starts_with('/') && p.as_bytes()[2] == b':' {
        p[1..].to_string()
    } else {
        p
    };
    Some(p)
}

/// 把剪贴板里粘贴进来的文件（只有内容、没有路径）落盘，返回可发送的本地路径。
/// 前端拿到路径后走和拖放一样的 send_files 通道。
#[tauri::command]
async fn stage_pasted_file(
    st: State<'_, SharedState>,
    name: String,
    b64: String,
) -> Result<String, String> {
    let path = net::stage_clipboard_file(&st.data_dir, &name, &b64)?;
    Ok(path.to_string_lossy().into_owned())
}

/// 在独立窗口里打开一张本地图片（仿微信：双击/单击图片弹出查看器窗口）。
///
/// 窗口标签用递增序号，允许同时开多张；图片内容由查看器自己通过
/// `read_image_data` 读取，路径经查询串传入（只允许已存在的本地图片文件）。
#[tauri::command]
async fn open_image_viewer(
    app: tauri::AppHandle,
    path: String,
    name: Option<String>,
) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp") {
        return Err("不支持的图片类型".into());
    }
    if !p.is_file() {
        return Err("图片文件不存在（可能已被移动或删除）".into());
    }
    // 同一张图已经开着就直接聚焦，不重复开窗
    let label_key: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let label = format!(
        "image-viewer-{:x}",
        label_key.bytes().fold(0u64, |a, b| a
            .wrapping_mul(1099511628211)
            .wrapping_add(b as u64))
    );
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    let title = name.unwrap_or_else(|| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "图片".into())
    });
    let url = format!("index.html?viewer=image&path={}", urlencode(&path));
    // 查询串之外再注入一份参数：不依赖前端框架对查询串的处理，路径原样送达
    let boot = format!(
        "window.__OIM_VIEWER__ = {};",
        json!({"path": path, "name": title}),
    );
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
        .initialization_script(boot)
        .title(&title)
        .inner_size(1000.0, 720.0)
        .min_inner_size(360.0, 280.0)
        .center()
        .resizable(true)
        .decorations(false)
        .build()
        .map_err(|e| format!("打开图片窗口失败: {e}"))?;
    Ok(())
}

/// 查询串百分号编码（路径里可能有空格、中文、#、? 等）
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::urlencode;

    #[test]
    fn file_uri_to_path_decodes() {
        use super::file_uri_to_path;
        assert_eq!(
            file_uri_to_path("file:///home/allen/a%20b.txt").as_deref(),
            Some("/home/allen/a b.txt")
        );
        assert_eq!(
            file_uri_to_path("file:///tmp/%E5%9B%BE.png").as_deref(),
            Some("/tmp/图.png")
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/x").as_deref(),
            Some("/tmp/x")
        );
        // 结尾的换行（uri-list 分隔符残留）要去掉
        assert_eq!(file_uri_to_path("file:///tmp/x\r\n").as_deref(), Some("/tmp/x"));
        assert_eq!(file_uri_to_path("http://example.com/a"), None);
        assert_eq!(file_uri_to_path("file://"), None);
    }

    #[test]
    fn urlencode_escapes_path_specials() {
        assert_eq!(urlencode("/tmp/a b.png"), "%2Ftmp%2Fa%20b.png");
        assert_eq!(urlencode("a-_.~"), "a-_.~");
        // 中文与 ?# 等会破坏查询串的字符必须转义
        assert_eq!(urlencode("图"), "%E5%9B%BE");
        assert!(!urlencode("x?y#z&w=1").contains(['?', '#', '&', '=']));
    }

    /// Windows 闪现帧的回归测试（两轮实测后根治的结论）：
    /// 托盘图标在 Windows 必须由「32 位 alpha 图标」（DIB section + alpha 通道）
    /// 承载，透明帧才能真正透明。已在 vendor/tray-icon 的 windows/icon.rs 把
    /// CreateIcon 经典图标改成 CreateIconIndirect + DIB section；这里守护
    /// 空白帧本身必须保持全 0（真透明像素），防止再改成 alpha=254 之类
    /// «接近黑» 的「假透明」。
    #[test]
    fn tray_blank_pixel_is_fully_transparent() {
        assert_eq!(super::TRAY_BLANK.len(), (super::TRAY_SIZE * super::TRAY_SIZE * 4) as usize);
        // 全 0：R、G、B、A 每个字节都必须为 0
        for px in super::TRAY_BLANK.chunks_exact(4) {
            assert_eq!(px, [0, 0, 0, 0], "空白帧必须是全透明像素");
        }
    }
}

/// 标记入站消息已读，并对要求回执的消息向对端发送 READMSG
#[tauri::command]
async fn mark_read(ctx: State<'_, SharedCtx>, key: String, pkts: Vec<u32>) -> Result<usize, String> {
    let ctx = ctx.inner().clone();
    net::mark_read_and_receipt(&ctx, &key, &pkts).await
}

/// 本地标记出站消息已被对端阅读（不发包）。
/// 用于「对方回话即视为已读」的兜底：飞秋等实现不会回 READMSG。
#[tauri::command]
fn mark_out_read(st: State<'_, SharedState>, key: String, pkts: Vec<u32>) -> Result<usize, String> {
    let st = st.inner().clone();
    let mut changed = 0;
    for p in pkts {
        if st.mark_out_read(&key, p) {
            changed += 1;
        }
    }
    Ok(changed)
}

/// 全部历史会话摘要（含离线的，中栏展示用）
#[tauri::command]
fn list_sessions(st: State<'_, SharedState>) -> Result<Vec<state::SessionInfo>, String> {
    Ok(st.inner().list_sessions())
}

/// 从官方 IP Messenger 的日志库（v4.5+ 的 ipmsg.db，SQLite）导入聊天记录。
/// 可一次传多个文件；返回汇总与逐文件明细，前端弹结果并刷新会话列表。
#[tauri::command]
async fn import_ipmsg_log(
    st: State<'_, SharedState>,
    paths: Vec<String>,
) -> Result<Value, String> {
    let st = st.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut total = 0usize;
        let mut skipped = 0usize;
        let mut sessions_new = 0usize;
        let mut merged_sessions = 0usize;
        let mut files: Vec<Value> = Vec::new();
        for p in &paths {
            let path = std::path::PathBuf::from(p);
            match ipmsg_import::import_ipmsg_db(&st, &path) {
                Ok(rep) => {
                    total += rep.imported;
                    skipped += rep.skipped;
                    sessions_new += rep.sessions_new;
                    merged_sessions += rep.merged_sessions;
                    files.push(json!({
                        "path": p,
                        "ok": true,
                        "imported": rep.imported,
                        "skipped": rep.skipped,
                        "sessions_new": rep.sessions_new,
                        "sessions_merged": rep.merged_sessions,
                    }));
                }
                Err(e) => {
                    files.push(json!({ "path": p, "ok": false, "error": e }));
                }
            }
        }
        Ok(json!({
            "total": total,
            "skipped": skipped,
            "sessionsNew": sessions_new,
            "mergedSessions": merged_sessions,
            "files": files,
            "failed": files.iter().filter(|f| f["ok"] == json!(false)).count(),
        }))
    })
    .await
    .map_err(|e| format!("导入任务失败：{e}"))?
}

/* ================= 启动 ================= */

pub fn run() {
    if std::env::args().any(|a| a == "--selftest") {
        let ok = selftest::run();
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 诊断模式：--tray-test 注册 SNI 托盘并打印收到的激活事件
    // （验证 Linux 下「单击托盘」这条链路是否真的通）
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == "--tray-test") {
        use ksni::{menu::StandardItem, Icon, Tray, TrayMethods};
        struct T;
        impl Tray for T {
            fn id(&self) -> String {
                "open-ipmsg-traytest".into()
            }
            fn title(&self) -> String {
                "Open IPMsg 托盘检测".into()
            }
            fn icon_pixmap(&self) -> Vec<Icon> {
                let mut data = Vec::with_capacity(TRAY_IDLE.len());
                for px in TRAY_IDLE.chunks_exact(4) {
                    data.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
                }
                vec![Icon {
                    width: TRAY_SIZE as i32,
                    height: TRAY_SIZE as i32,
                    data,
                }]
            }
            fn activate(&mut self, x: i32, y: i32) {
                println!("[tray-test] 收到 Activate（左键单击） at ({x},{y})");
            }
            fn secondary_activate(&mut self, x: i32, y: i32) {
                println!("[tray-test] 收到 SecondaryActivate（中键） at ({x},{y})");
            }
            fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
                vec![StandardItem {
                    label: "菜单项测试".into(),
                    activate: Box::new(|_: &mut T| println!("[tray-test] 菜单项被点击")),
                    ..Default::default()
                }
                .into()]
            }
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            match T.spawn().await {
                Ok(_h) => {
                    println!("[tray-test] SNI 注册成功，等待 25 秒接收事件…");
                    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
                }
                Err(e) => println!("[tray-test] SNI 注册失败: {e}"),
            }
        });
        std::process::exit(0);
    }

    // 诊断模式：--clipboard-test 读一次系统剪贴板里的文件列表并打印
    // （不启动界面，用于验证 Linux 下 GTK 剪贴板读取是否真的可用）
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == "--clipboard-test") {
        use gtk::prelude::*;
        if gtk::init().is_err() {
            eprintln!("GTK 初始化失败（需要图形会话）");
            std::process::exit(1);
        }
        // Wayland 下没有焦点窗口的客户端读不到剪贴板（合成器只把选区交给
        // 有焦点的客户端），所以必须先亮一个窗口拿到焦点再读
        let win = gtk::Window::new(gtk::WindowType::Toplevel);
        win.set_title("剪贴板检测（自动关闭）");
        win.set_default_size(360, 110);
        win.add(&gtk::Label::new(Some("正在读取剪贴板…")));
        win.show_all();
        win.present();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(900), || {
            let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
            println!(
                "GDK 后端: {}",
                gdk::Display::default()
                    .map(|d| d.name().to_string())
                    .unwrap_or_else(|| "<none>".into())
            );
            println!("剪贴板文本: {:?}", cb.wait_for_text().map(|t| t.to_string()));
            match cb.wait_for_targets() {
                Some(t) => println!(
                    "剪贴板可用目标: {:?}",
                    t.iter().map(|x| x.name().to_string()).collect::<Vec<_>>()
                ),
                None => println!("剪贴板可用目标: <取不到>"),
            }
            let uris: Vec<String> = cb.wait_for_uris().iter().map(|u| u.to_string()).collect();
            println!("剪贴板 URI 数量: {}", uris.len());
            for u in &uris {
                println!("  {u}  ->  {:?}", file_uri_to_path(u));
            }
            gtk::main_quit();
            gtk::glib::ControlFlow::Break
        });
        gtk::main();
        std::process::exit(0);
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
                theme: "system".into(),
                lang: String::new(),
                encrypt: true,
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
        // 单实例互斥：再次启动时不再抢端口，而是把已运行的那个窗口唤到前台
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            eprintln!("[single-instance] 已有实例在运行，唤起既有窗口");
            activate_from_tray(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let st = Arc::new(AppState::new(data_dir));
            st.load_config();
            // 界面语言：原生 UI（托盘菜单等）按 config.lang 取简中/英文
            let ui_lang = st.config().lang.clone();
            // 恢复离线消息待投递队列（对方上线后自动重投）
            st.load_pending();
            // 恢复对端公钥缓存（加密会话重启后无需重新交换公钥）
            st.load_peer_keys();
            // 会话键从 ip:port 归一化为纯 IP：把旧命名的 `<ip>_<端口>.jsonl`
            // 迁移成 `<ip>.jsonl`，并改写记录内的 peer.key 快照
            let migrated = st.migrate_legacy_history_keys();
            if migrated > 0 {
                st.diag(&format!("history-migrate 迁移旧会话文件 {migrated} 个"));
            }
            // 一次性修复旧版本遗留的历史文件（重投副本堆积 / 并发写入的坏行）
            let (merged, dropped) = st.compact_histories();
            if merged > 0 || dropped > 0 {
                st.diag(&format!("history-compact 合并重复 {merged} 条，丢弃坏行 {dropped} 行"));
            }
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
                        .message(ui_str(
                            &ui_lang,
                            &format!(
                                "端口 {} 被占用（{}）。\n本程序已做单实例互斥，若非本程序重复启动，\
                                 多半是机器上还运行着别的 IPMsg/飞秋类客户端，请先退出它。",
                                protocol::DEFAULT_PORT, e
                            ),
                            &format!(
                                "Port {} is in use ({}).\nThis app already guards against duplicate \
                                 instances; if this is not a second copy of the app, another IPMsg \
                                 client is probably running — please quit it first.",
                                protocol::DEFAULT_PORT, e
                            ),
                        ))
                        .kind(MessageDialogKind::Error)
                        .title(ui_str(&ui_lang, "Open IPMsg 启动失败", "Open IPMsg failed to start"))
                        .blocking_show();
                    std::process::exit(1);
                }
            };

            // 窗口标题与托盘提示带上版本号，便于确认当前运行的构建
            let run_title = format!("Open IPMsg v{}", env!("CARGO_PKG_VERSION"));
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&run_title);
            }

            // 退出前广播 BR_EXIT（托盘退出 / 进程退出都会走到这里）
            let _ = EXIT_INFO.set((st.clone(), protocol::DEFAULT_PORT));

            app.manage(st);
            app.manage(ctx.clone());

            /* ---------- 系统托盘 ---------- */
            // Linux：先尝试自己注册 StatusNotifierItem（能拿到单击事件与悬停提示），
            // 注册失败（没有 SNI 宿主，如部分 X11 轻量桌面）再回退到 Tauri ��带托盘
            #[cfg(target_os = "linux")]
            let sni_ok = tauri::async_runtime::block_on(linux_tray::spawn(
                handle.clone(),
                run_title.clone(),
                ui_lang.clone(),
            ));
            #[cfg(not(target_os = "linux"))]
            let sni_ok = false;

            let show_item = MenuItem::with_id(
                &handle,
                "show",
                ui_str(&ui_lang, "显示主窗口", "Show main window"),
                true,
                None::<&str>,
            )?;
            let refresh_item = MenuItem::with_id(
                &handle,
                "refresh",
                ui_str(&ui_lang, "刷新在线用户", "Refresh online users"),
                true,
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(
                &handle,
                "quit",
                ui_str(&ui_lang, "退出", "Quit"),
                true,
                None::<&str>,
            )?;
            let menu = MenuBuilder::new(&handle)
                .items(&[&show_item, &refresh_item, &quit_item])
                .build()?;

            let refresh_ctx = ctx.clone();
            if !sni_ok {
            TrayIconBuilder::with_id("main-tray")
                .icon(tauri::image::Image::new(TRAY_IDLE, TRAY_SIZE, TRAY_SIZE))
                .tooltip(&run_title)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => activate_from_tray(app),
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
                    // 双击（以及 Windows/macOS 上的左键单击）唤起主窗口，
                    // 并跳到最新的未读会话
                    let hit = matches!(
                        event,
                        TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        } | TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    );
                    if hit {
                        activate_from_tray(tray.app_handle());
                    }
                })
                .build(&handle)?;
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 只有主窗口才「关闭 = 最小化到托盘」（微信式）；
            // 图片查看器等附属窗口必须能真正关掉
            if window.label() != "main" {
                return;
            }
            // 关闭窗口 → 最小化到托盘；真正退出走托盘菜单
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
                if !HIDE_NOTIFIED.swap(true, Ordering::Relaxed) {
                    use tauri_plugin_notification::NotificationExt;
                    let lang = window
                        .app_handle()
                        .state::<SharedState>()
                        .config()
                        .lang;
                    let _ = window
                        .notification()
                        .builder()
                        .title("Open IPMsg")
                        .body(ui_str(
                            &lang,
                            "已最小化到托盘，右键托盘图标可退出",
                            "Minimized to tray; use the tray icon menu to quit",
                        ))
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
            clear_history,
            search_history,
            send_text,
            send_files,
            send_clipboard_image,
            stage_pasted_file,
            clipboard_file_paths,
            clipboard_image,
            set_unread,
            download_file,
            read_image_data,
            open_image_viewer,
            mark_read,
            mark_out_read,
            list_sessions,
            import_ipmsg_log
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
