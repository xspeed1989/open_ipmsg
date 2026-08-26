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
    tray::TrayIconBuilder,
    Manager, RunEvent, State, WindowEvent,
};
// Linux 回退托盘（GTK）不投递任何点击事件，事件类型只在 Windows/macOS 用到
#[cfg(not(target_os = "linux"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

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

/// 把主窗口带到最前并聚焦（托盘/通知唤起统一走这里）。
///
/// 为什么需要「重映射」：Wayland 合成器只给带 xdg-activation token 的请求
/// 提层（协议规定 token 只能来自「用户与应用表面的交互」）；托盘/通知点击
/// 来自 DBus，应用拿不到 token，gtk present / set_focus 会被合成器静默忽略 ──
/// 表现就是「窗口隐藏时能显示、已可见被盖住时点托盘不置前」。解法：**可见但
/// 不在前台的**窗口先 hide 再 show 重映射，合成器把重映射的窗口当新窗口重新
/// 聚焦置前（KWin/GNOME 的 Wayland 实现行为一致，KDE Wayland 上实测有效），
/// 代价至多一帧闪烁。窗口若本来就在最前则跳过重映射，只让前端切会话，不闪。
/// 仅限 Wayland 会话（按 WAYLAND_DISPLAY 判定，对任何合成器生效）；
/// X11 的 present 提层、Windows/macOS 路径完全不受影响。
fn raise_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        if let Some(w) = app.get_webview_window("main") {
            // 本来就在最前：重映射只会无谓闪烁；不可见/被盖住才需要重映射
            // （判定用点击前的焦点采样，点托盘本身会抢走焦点，见 LAST_FOCUS_SAMPLE）
            let visible = w.is_visible().unwrap_or(false);
            let front = was_in_front_before_click();
            eprintln!("[raise] visible={visible} front={front} (本次是否重映射: {})", visible && !front);
            if visible && !front {
                let _ = w.hide();
            }
        }
    }
    show_main_window(app);
}

/// 焦点采样历史（Linux 下主线程每 250ms 更新一次，见 setup 里的采样循环）。
/// 供「托盘点击前窗口是否本来就在最前」判定：**点托盘那一下，面板会把键盘
/// 焦点从本窗口抢走**（Wayland wl_keyboard 离开应用表面）——无论用事件还是
/// 点击后再查 is_focused，到双击处理器执行时窗口都已显示为“无焦点”，明明
/// 在最前也被误判成「被盖住」，于是又 hide/show 重映射闪一下。用点击前
/// 最近一次采样就不会被点击本身影响：采到 true 就是本来在最前。
#[cfg(target_os = "linux")]
static LAST_FOCUS_SAMPLE: std::sync::Mutex<Option<(bool, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// 「托盘点击前窗口是否本来就在最前」：最近一次采样为聚焦且不超过 2 秒
/// （采样循环健在）即认为在最前；采不到/过期按「不在最前」处理（重映射，
/// 安全方向——宁可多闪一次也不能点了不置前）。
#[cfg(target_os = "linux")]
fn was_in_front_before_click() -> bool {
    let g = LAST_FOCUS_SAMPLE.lock().unwrap();
    matches!(
        *g,
        Some((true, at)) if at.elapsed() < std::time::Duration::from_secs(2)
    )
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
/// 提层靠 raise_main_window 的重映射（Wayland）；前端收到 open-unread 后仍会
/// 自己 show/unminimize/setFocus 一次作为第二道保险（X11 下 present 直接有效）。
fn activate_from_tray(app: &tauri::AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        raise_main_window(&app2);
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

/* ================= 托盘「双击」判定（SNI / macOS 没有双击事件） ================= */

/// 两次单击判定为一次双击的最大间隔（毫秒）。
const DOUBLE_CLICK_WINDOW_MS: u128 = 350;

/// 单击 → 双击判定器：`feed(now)` 返回这次单击是否构成一次双击。
///
/// 需要模拟双击的平台：Linux SNI（StatusNotifierItem 协议只有 Activate
/// 单击信号，没有双击）与 macOS 托盘（tray-icon 0.24 不投递 DoubleClick）。
/// Windows 有原生 WM_LBUTTONDBLCLK，不走这里。
///
/// 语义（微信式）：单击本身没有任何动作，350ms 内连续两次单击才算双击。
/// 判定成功后清空状态，避免三连击拆成「双击 + 单击」后残留旧时间戳。
#[cfg(not(target_os = "windows"))]
pub(crate) struct DoubleClickGate {
    last: Option<std::time::Instant>,
}

#[cfg(not(target_os = "windows"))]
impl DoubleClickGate {
    pub(crate) const fn new() -> Self {
        Self { last: None }
    }

    /// 送入一次单击的时间点。返回是否构成双击（是则重置状态，可作为
    /// 下一组双击的第一次单击）。
    pub(crate) fn feed(&mut self, now: std::time::Instant) -> bool {
        let double = match self.last {
            Some(prev) => {
                now.saturating_duration_since(prev).as_millis() <= DOUBLE_CLICK_WINDOW_MS
            }
            None => false,
        };
        self.last = if double { None } else { Some(now) };
        double
    }
}

/// 全局双击判定器：SNI 的回调跑在 DBus 线程、macOS 托盘事件跑在主线程，
/// 用 Mutex 串行化（判定不依赖顺序，只依赖时间差）。
#[cfg(not(target_os = "windows"))]
static DOUBLE_CLICK_GATE: std::sync::Mutex<DoubleClickGate> =
    std::sync::Mutex::new(DoubleClickGate::new());

#[cfg(all(test, not(target_os = "windows")))]
mod gate_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn single_clicks_never_open() {
        let mut g = DoubleClickGate::new();
        let t = Instant::now();
        assert!(!g.feed(t), "第一次单击不构成双击");
        assert!(!g.feed(t + Duration::from_millis(400)), "间隔超窗的单击不算双击");
        // 400ms 后的那次单击已成为新的「第一次」，再来一次相隔 400ms 的仍是单击
        let t2 = t + Duration::from_millis(800);
        assert!(!g.feed(t2));
    }

    #[test]
    fn double_click_detected_and_resets() {
        let mut g = DoubleClickGate::new();
        let t = Instant::now();
        assert!(!g.feed(t));
        assert!(g.feed(t + Duration::from_millis(200)), "350ms 内第二次单击 = 双击");
        // 判定成功后状态清空：1s 后的单击是下一组的第一下
        assert!(!g.feed(t + Duration::from_millis(1000)));
        assert!(g.feed(t + Duration::from_millis(1200)), "第二组双击仍能识别");
    }

    #[test]
    fn window_boundary_is_inclusive() {
        let mut g = DoubleClickGate::new();
        let t = Instant::now();
        assert!(!g.feed(t));
        assert!(g.feed(t + Duration::from_millis(350)), "恰好在窗口内算双击");

        let mut g2 = DoubleClickGate::new();
        assert!(!g2.feed(t));
        assert!(!g2.feed(t + Duration::from_millis(351)), "超过窗口 1ms 不算");
    }
}

/* ================= Linux 托盘（自实现 StatusNotifierItem） =================

Tauri 在 Linux 用的是 libappindicator：它只有菜单，**不投递任何点击事件**
（tray-icon 0.24 的 GTK 后端里连点击处理都没有，set_tooltip 也是空实现）。
微信之类的应用能做到「单击托盘打开主界面」，是因为它们自己注册
StatusNotifierItem —— KDE/XFCE 等宿主会对该对象调用 Activate。

这里在 Linux 上同样自己注册 SNI（ksni），于是拿到了：
  - 左键单击 → Activate：协议里没有双击信号，用两次单击的间隔自己判定
    （≤350ms 视为双击，唤起主窗口并跳到最新未读会话；单击无动作，微信式）
  - 悬停提示（含未读条数），libappindicator 下本来是没有的
  - 图标闪烁（更新图标即可）
Windows 走 tray-icon 的原生 DoubleClick 事件；macOS 用单击间隔模拟（同上）。
SNI 注册失败（桌面没有 SNI 宿主，如部分 X11 轻量桌面）时回退到 Tauri 自带
托盘：GTK 托盘连点击事件都没有，只能靠右键菜单。 */
#[cfg(target_os = "linux")]
mod linux_tray {
    use super::{activate_from_tray, tray_idle_image, TRAY_SIZE};
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
                to_argb(tray_idle_image().rgba())
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
        /// 左键单击：SNI 协议里没有双击信号，用两次 activate 的间隔自己判定
        /// （双击判定器见 DOUBLE_CLICK_GATE）。微信式：单击无动作，双击才唤起。
        fn activate(&mut self, _x: i32, _y: i32) {
            if super::DOUBLE_CLICK_GATE
                .lock()
                .unwrap()
                .feed(std::time::Instant::now())
            {
                activate_from_tray(&self.app);
            }
        }
        fn secondary_activate(&mut self, _x: i32, _y: i32) {
            // 中键单击：快捷唤起（不属于左键连击，不参与双击判定）
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

    /// 只切换图标帧（闪烁循环用）：不写 tip，避免旧未读数覆盖新提示
    pub fn set_blank(blank: bool) {
        if let Some(h) = HANDLE.get() {
            let h = h.clone();
            tauri::async_runtime::spawn(async move {
                h.update(move |t: &mut OimTray| {
                    t.blank = blank;
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

/// 托盘两态图标（PNG 资产，首次使用时解码一次并缓存）：
/// - `tray.png`：正常图标（scripts/gen_icons.py 生成，64×64）；
/// - `tray_blank.png`：全透明帧，与正常图标交替 = 微信那种「图标一闪一闪」。
///
/// 为什么 Windows 上现在能靠全 0 像素的 PNG 帧透明（曾在此踩过两次坑）：
/// tray-icon 0.24.x 原本在 Windows 用 CreateIcon 做成「单色 AND mask + 32bpp XOR」
/// 的经典图标，没有真 alpha 通道 —— 全 0 像素的帧被画成黑块/马赛克（alpha
/// 全 0 走经典路径；调成 alpha=254 又走合成路径变成近纯黑）。现已在
/// vendor/tray-icon 把 Windows 图标改成 CreateIconIndirect + DIB section 的
/// 32 位 alpha 图标（Electron nativeImage 同款做法，见 Cargo.toml 的
/// [patch.crates-io] 注释），透明帧由 alpha 通道真正合成；PNG 资产解码出的
/// RGBA 与原生像素完全等价。macOS / Linux 一直走真 alpha 合成，同样全 0 即可。
const TRAY_SIZE: u32 = 64;

fn tray_idle_image() -> &'static tauri::image::Image<'static> {
    static IMG: OnceLock<tauri::image::Image<'static>> = OnceLock::new();
    IMG.get_or_init(|| {
        let img = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
            .expect("内置托盘图标 tray.png 解码失败");
        debug_assert_eq!((img.width(), img.height()), (TRAY_SIZE, TRAY_SIZE));
        img
    })
}

fn tray_blank_image() -> &'static tauri::image::Image<'static> {
    static IMG: OnceLock<tauri::image::Image<'static>> = OnceLock::new();
    IMG.get_or_init(|| {
        let img = tauri::image::Image::from_bytes(include_bytes!("../icons/tray_blank.png"))
            .expect("内置托盘空白帧 tray_blank.png 解码失败");
        debug_assert_eq!((img.width(), img.height()), (TRAY_SIZE, TRAY_SIZE));
        img
    })
}
/// 闪烁间隔：与微信节奏接近
const FLASH_INTERVAL_MS: u64 = 600;
/// 当前是否处于闪烁状态（未读 > 0）
static FLASHING: AtomicBool = AtomicBool::new(false);

/// 切换托盘图标帧（正常帧 / 透明帧），**不改悬停提示**。
/// 闪烁循环里每 600ms 用旧 tip 覆盖会把未读数卡在循环启动时的值
/// （如一直显示「1 条未读」）——提示只由 set_unread 直接刷新。
fn set_tray_frame(app: &tauri::AppHandle, blank: bool) {
    #[cfg(target_os = "linux")]
    if linux_tray::is_active() {
        linux_tray::set_blank(blank);
        return;
    }
    let img = if blank { tray_blank_image() } else { tray_idle_image() };
    set_tray_icon(app, img);
}

fn set_tray_icon(app: &tauri::AppHandle, img: &'static tauri::image::Image<'static>) {
    // 闪烁循环跑在后台任务里，而托盘底层是 GTK/状态栏对象：
    // 一律回主线程改图标，避免跨线程操作 UI
    let app2 = app.clone();
    let img = img.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = app2.tray_by_id("main-tray") {
            let _ = tray.set_icon(Some(img));
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
            tauri::async_runtime::spawn(async move {
                let mut blank = false;
                while FLASHING.load(Ordering::SeqCst) {
                    blank = !blank;
                    set_tray_frame(&app2, blank);
                    tokio::time::sleep(std::time::Duration::from_millis(FLASH_INTERVAL_MS)).await;
                }
                // 停止时一定回到正常图标，避免停在透明帧上（图标像消失了）
                set_tray_frame(&app2, false);
            });
        }
    } else {
        FLASHING.store(false, Ordering::SeqCst);
        set_tray_frame(&app, false);
    }
    Ok(())
}

/// Linux 原生「可点击」消息通知：点击通知（正文或「打开」按钮）→ 前端收到
/// open-chat 事件 → 弹出主窗口并切到对应会话。
///
/// 为什么不用 tauri-plugin-notification：它的 JS/Rust API 都没有「点击通知」
/// 回调，点了没反应。这里直接用 notify-rust 发 DBus 通知 —— 通知守护进程
/// 会把点击作为 ActionInvoked 送回（KDE/XFCE 点正文触发 default 动作；
/// GNOME 不支持 default 动作，但「打开」按钮同样可用）。
/// 仅 Linux 提供点击能力；Windows/macOS 的通知仍由前端走插件，不去抢
/// 各自系统的通知注册（如 Windows 的 toast AUMID）。
#[tauri::command]
fn notify_message(
    app: tauri::AppHandle,
    key: String,
    title: String,
    body: String,
    lang: String,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let mut n = notify_rust::Notification::new();
        n.appname("Open IPMsg")
            .icon("open-ipmsg")
            .summary(&title)
            .body(&body)
            .action("open", &ui_str(&lang, "打开", "Open"));
        let handle = n.show().map_err(|e| format!("通知发送失败: {e}"))?;
        eprintln!("[notify] shown id={} key={}", handle.id(), key);
        // wait_for_action 会阻塞到通知被点击或关闭（守护进程超时/手动关掉也会
        // 触发关闭信号），放独立线程等，避免占住 tokio 的 blocking 线程池。
        // 点击通知 = raise_main_window（Wayland 重映射置前）+ 通知前端切会话。
        std::thread::spawn(move || {
            handle.wait_for_action(|action| {
                eprintln!("[notify] clicked action={action} key={key}");
                if action == "default" || action == "open" {
                    let app2 = app.clone();
                    let key2 = key.clone();
                    let _ = app.run_on_main_thread(move || {
                        raise_main_window(&app2);
                        use tauri::Emitter;
                        // payload 用 JSON 对象（与 msg-in 等事件一致）：字符串裸
                        // payload 在事件桥里的序列化路径未经受验证，对象模式最稳
                        let _ = app2.emit("open-chat", json!({"key": key2}));
                    });
                }
            });
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (&app, &key, &title, &body, &lang);
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

    /// 托盘两态 PNG 资产回归测试：
    /// - `tray.png` / `tray_blank.png` 必须能解码且是 64×64（阻止资产损坏/尺寸漂移）；
    /// - 空白帧必须保持全 0（真透明像素），防止再改成 alpha=254 之类
    ///   «接近黑» 的「假透明」。
    ///   Windows 上全 0 像素的透明度由 vendor 修复后的 tray-icon DIB alpha
    ///   图标保证（见 Cargo.toml [patch.crates-io] 注释与 vendor/tray-icon）。
    #[test]
    fn tray_png_assets_decode_and_blank_is_transparent() {
        let idle = super::tray_idle_image();
        let blank = super::tray_blank_image();
        assert_eq!(
            (idle.width(), idle.height()),
            (super::TRAY_SIZE, super::TRAY_SIZE),
            "tray.png 必须是 64×64"
        );
        assert_eq!(
            (blank.width(), blank.height()),
            (super::TRAY_SIZE, super::TRAY_SIZE),
            "tray_blank.png 必须是 64×64"
        );
        // 全 0：R、G、B、A 每个字节都必须为 0
        for px in blank.rgba().chunks_exact(4) {
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
                let rgba = tray_idle_image().rgba();
                let mut data = Vec::with_capacity(rgba.len());
                for px in rgba.chunks_exact(4) {
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

            // Wayland：托盘点击会抢走窗口焦点，需要「点击前的焦点采样历史」
            // 来判断窗口是否本来就在最前（见 LAST_FOCUS_SAMPLE），每 250ms 采样
            #[cfg(target_os = "linux")]
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                let h2 = handle.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let h3 = h2.clone();
                        let _ = h2.run_on_main_thread(move || {
                            if let Some(w) = h3.get_webview_window("main") {
                                if let Ok(f) = w.is_focused() {
                                    *LAST_FOCUS_SAMPLE.lock().unwrap() =
                                        Some((f, std::time::Instant::now()));
                                }
                            }
                        });
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                });
            }

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
                .icon(tray_idle_image().clone())
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
                .on_tray_icon_event(|_tray, event| {
                    // 双击唤起主窗口（并跳到最新的未读会话），微信式：单击无动作。
                    // Windows 有原生 DoubleClick 事件；macOS 的 tray-icon 只投递
                    // 单击，用两次单击的间隔自己判定（见 DOUBLE_CLICK_GATE）。
                    // Linux 回退托盘（GTK）不投递任何点击事件，双击判定在 SNI 里做。
                    #[cfg(target_os = "windows")]
                    if let TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        activate_from_tray(_tray.app_handle());
                    }
                    #[cfg(target_os = "macos")]
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if DOUBLE_CLICK_GATE
                            .lock()
                            .unwrap()
                            .feed(std::time::Instant::now())
                        {
                            activate_from_tray(_tray.app_handle());
                        }
                    }
                    #[cfg(target_os = "linux")]
                    let _ = event;
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
                            "已最小化到托盘：双击托盘图标可打开主窗口，右键菜单可退出",
                            "Minimized to tray: double-click the tray icon to open, right-click to quit",
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
            notify_message,
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
