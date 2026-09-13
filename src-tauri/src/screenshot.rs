//! 截图子系统：抓屏后端 / 会话缓存 / 遮罩窗口 / 命令。
//!
//! 坐标约定：`logical` 是窗口系统逻辑像素（开窗与遮罩定位用），
//! `px` 是整幅抓屏图像里的物理像素（裁剪用）。两者的换算只在这里做一次，
//! 前端拿到的永远是物理像素矩形。

use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;

/// 矩形（x/y 允许为负 —— 多屏时副屏可以排在主屏左侧或上方）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 所有显示的并集（虚拟桌面），坐标为逻辑像素
pub fn virtual_bounds(rects: &[Rect]) -> Rect {
    if rects.is_empty() {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    }
    let x1 = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.y).min().unwrap_or(0);
    let x2 = rects.iter().map(|r| r.x + r.w as i32).max().unwrap_or(0);
    let y2 = rects.iter().map(|r| r.y + r.h as i32).max().unwrap_or(0);
    Rect { x: x1, y: y1, w: (x2 - x1).max(0) as u32, h: (y2 - y1).max(0) as u32 }
}

/// 全局比例 k = 整幅图像宽 ÷ 逻辑总宽。
///
/// 这是唯一可信的换算来源：tao 在 Linux 上给的 `scale_factor` 是 GDK 的整数
/// 缩放（本机报 2），与真实比例（本机 1.25）不符，用它换算必然错位。
pub fn scale_for(image_w: u32, logical_w: u32) -> f64 {
    if logical_w == 0 {
        return 1.0;
    }
    image_w as f64 / logical_w as f64
}

/// 某块屏在整幅图像里的物理像素矩形
pub fn slice_for_monitor(logical: Rect, bounds: Rect, k: f64) -> Rect {
    let x = ((logical.x - bounds.x) as f64 * k).round() as i32;
    let y = ((logical.y - bounds.y) as f64 * k).round() as i32;
    let w = (logical.w as f64 * k).round().max(1.0) as u32;
    let h = (logical.h as f64 * k).round().max(1.0) as u32;
    Rect { x, y, w, h }
}

/// Windows BitBlt 的 32 位 BGRA 缓冲 → RGBA（stride 可能大于行宽）
pub fn bgra_to_rgba(src: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let s = y * stride + x * 4;
            let d = (y * w as usize + x) * 4;
            if s + 3 < src.len() {
                out[d] = src[s + 2];
                out[d + 1] = src[s + 1];
                out[d + 2] = src[s];
                out[d + 3] = src[s + 3];
            }
        }
    }
    out
}

/// 抓屏失败分类：错误码给前端做分支，文案给用户看
#[derive(Debug)]
pub enum ShotErr {
    /// 系统没有可用的截图服务（未安装/未运行 xdg-desktop-portal）
    PortalMissing(String),
    /// portal 返回了非 0 响应码（用户拒绝 / 后端出错）
    PortalDenied(u32),
    /// portal 在规定时间内没有回响应
    Timeout,
    /// 图像解码失败
    Decode(String),
    /// macOS 未授予「屏幕录制」权限
    MacPermission,
    /// 平台抓屏 API 失败
    CaptureFailed(String),
}

impl ShotErr {
    pub fn code(&self) -> &'static str {
        match self {
            ShotErr::PortalMissing(_) => "PORTAL_MISSING",
            ShotErr::PortalDenied(_) => "PORTAL_DENIED",
            ShotErr::Timeout => "PORTAL_TIMEOUT",
            ShotErr::Decode(_) => "DECODE_FAILED",
            ShotErr::MacPermission => "MAC_PERMISSION",
            ShotErr::CaptureFailed(_) => "CAPTURE_FAILED",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ShotErr::PortalMissing(e) => format!("系统未提供截图服务（xdg-desktop-portal）：{e}"),
            ShotErr::PortalDenied(c) => format!("截图请求被系统拒绝（响应码 {c}）"),
            ShotErr::Timeout => "截图超时：系统未在 15 秒内响应".into(),
            ShotErr::Decode(e) => format!("截图数据解码失败：{e}"),
            ShotErr::MacPermission => {
                "需要「屏幕录制」权限：系统设置 → 隐私与安全性 → 屏幕录制".into()
            }
            ShotErr::CaptureFailed(e) => format!("抓屏失败：{e}"),
        }
    }
}

/// 一次抓屏的结果
pub struct Captured {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// PNG 字节 → 尺寸（Linux 下 portal 直接给 PNG，无需再编码）
pub fn decode_captured(png: Vec<u8>) -> Result<Captured, ShotErr> {
    let img = image::load_from_memory(&png).map_err(|e| ShotErr::Decode(e.to_string()))?;
    let (width, height) = (img.width(), img.height());
    Ok(Captured { png, width, height })
}

/// 抓取整个工作区（原生物理像素）。
///
/// 放到独立线程并带超时：portal 的 Response 信号是阻塞等待的，不能占住
/// Tauri 命令所在的 tokio worker，也不能无限期挂起。
pub fn capture_png(timeout: Duration) -> Result<Captured, ShotErr> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(capture_png_inner());
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(ShotErr::Timeout),
    }
}

#[cfg(target_os = "linux")]
fn capture_png_inner() -> Result<Captured, ShotErr> {
    let raw = portal::screenshot_png()?;
    decode_captured(raw)
}

#[cfg(not(target_os = "linux"))]
fn capture_png_inner() -> Result<Captured, ShotErr> {
    platform::capture()
}

/// xdg-desktop-portal 客户端（Linux：X11 与 Wayland 同一条路径）
#[cfg(target_os = "linux")]
mod portal {
    use super::ShotErr;
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";

    /// 非交互抓屏 → PNG 字节
    pub fn screenshot_png() -> Result<Vec<u8>, ShotErr> {
        let conn = Connection::session()
            .map_err(|e| ShotErr::PortalMissing(format!("无法连接会话总线: {e}")))?;
        // handle 路径可预测：/org/freedesktop/portal/desktop/request/<sender>/<token>
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .ok_or_else(|| ShotErr::PortalMissing("会话总线没有唯一名".into()))?;
        let token = format!("oimshot{}", std::process::id());
        let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

        // 必须先订阅再调用：portal 的响应可能早于调用返回
        let req = Proxy::new(&conn, DEST, handle.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        let mut signals = req
            .receive_signal("Response")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;

        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("interactive", Value::from(false));
        options.insert("modal", Value::from(false));

        let shot = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.Screenshot")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        let returned: OwnedObjectPath = shot
            .call("Screenshot", &("", options))
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        if returned.as_str() != handle {
            // portal 用了别的 handle（罕见）：改挂到实际路径上再等
            let req2 = Proxy::new(&conn, DEST, returned.as_str(), "org.freedesktop.portal.Request")
                .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
            signals = req2
                .receive_signal("Response")
                .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        }

        let msg = signals
            .next()
            .ok_or_else(|| ShotErr::PortalMissing("portal 未返回响应".into()))?;
        let (code, results): (u32, HashMap<String, OwnedValue>) = msg
            .body()
            .deserialize()
            .map_err(|e| ShotErr::Decode(e.to_string()))?;
        if code != 0 {
            return Err(ShotErr::PortalDenied(code));
        }
        let uri: &str = results
            .get("uri")
            .ok_or_else(|| ShotErr::Decode("响应里没有 uri".into()))?
            .try_into()
            .map_err(|_| ShotErr::Decode("uri 不是字符串".into()))?;
        let path = crate::file_uri_to_path(uri)
            .ok_or_else(|| ShotErr::Decode(format!("无法解析 uri: {uri}")))?;
        let bytes = std::fs::read(&path)
            .map_err(|e| ShotErr::CaptureFailed(format!("读取截图文件失败: {e}")))?;
        // portal 把 PNG 落在用户图片目录：读完即删，不留垃圾
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }
}

/// 非 Linux 平台的抓屏后端（Windows / macOS，见 Task 12 / Task 13）
#[cfg(not(target_os = "linux"))]
mod platform {
    use super::ShotErr;

    pub fn capture() -> Result<super::Captured, ShotErr> {
        Err(ShotErr::CaptureFailed("当前平台尚未实现抓屏".into()))
    }
}

/// 单块显示器（index 是主键：Linux 上两块同型号屏的 name 会重名）
#[derive(Clone, Debug, Serialize)]
pub struct ShotMonitor {
    pub index: usize,
    pub name: String,
    /// 在整幅图像里的物理像素矩形（前端裁剪用）
    pub px: Rect,
    /// 逻辑像素矩形（开窗定位用）
    pub logical: Rect,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShotCapture {
    pub session: String,
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<ShotMonitor>,
}

pub struct CachedShot {
    pub session: String,
    pub png_b64: String,
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<ShotMonitor>,
}

/// 同一时刻只保留一个截图会话：遮罩窗口凭 session 取图，旧会话立即失效
#[derive(Default)]
pub struct ShotState(Mutex<Option<CachedShot>>);

impl ShotState {
    pub fn put(&self, shot: CachedShot) {
        *self.0.lock().unwrap() = Some(shot);
    }

    pub fn get(&self, session: &str) -> Result<CachedShot, ShotErr> {
        let guard = self.0.lock().unwrap();
        match guard.as_ref() {
            Some(s) if s.session == session => Ok(CachedShot {
                session: s.session.clone(),
                png_b64: s.png_b64.clone(),
                width: s.width,
                height: s.height,
                monitors: s.monitors.clone(),
            }),
            _ => Err(ShotErr::CaptureFailed("截图会话已失效，请重新截图".into())),
        }
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }

    pub fn active_session(&self) -> Option<String> {
        self.0.lock().unwrap().as_ref().map(|s| s.session.clone())
    }
}

pub fn monitor_at(monitors: &[ShotMonitor], index: usize) -> Result<ShotMonitor, ShotErr> {
    monitors
        .get(index)
        .cloned()
        .ok_or_else(|| ShotErr::CaptureFailed(format!("显示器序号越界: {index}")))
}

fn session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms:x}-{n}")
}

/// 抓屏并写缓存（**阻塞**，最长 15s）—— 只做重活，不开窗。
///
/// 必须从「非 tokio worker」的上下文调用（命令用 `spawn_blocking`，热键/命令行
/// 用自己的线程）：portal 的 Response 是阻塞等待，直接放在 async 命令里会占住
/// 一个 tokio worker 最长 15 秒。
pub fn capture_and_cache(
    app: &tauri::AppHandle,
    state: &ShotState,
) -> Result<ShotCapture, ShotErr> {
    // 已有一个会话：直接把现有会话还回去（不重复抓屏）
    if let Some(session) = state.active_session() {
        if let Ok(cached) = state.get(&session) {
            return Ok(ShotCapture {
                session: cached.session,
                width: cached.width,
                height: cached.height,
                monitors: cached.monitors,
            });
        }
        state.clear();
    }

    let cap = capture_png(Duration::from_secs(15))?;
    let monitors = collect_monitors(app, cap.width)?;
    let capture = ShotCapture {
        session: session_id(),
        width: cap.width,
        height: cap.height,
        monitors,
    };
    state.put(CachedShot {
        session: capture.session.clone(),
        png_b64: base64::engine::general_purpose::STANDARD.encode(&cap.png),
        width: capture.width,
        height: capture.height,
        monitors: capture.monitors.clone(),
    });
    Ok(capture)
}

/// 真正的入口（工具栏 / 热键 / 命令行都汇到这里）：抓屏 + 建遮罩窗口。
///
/// 抓屏在调用线程上阻塞完成（调用方保证这不是 tokio worker）；开窗沿用
/// `open_image_viewer` 已验证的写法 —— 直接在命令/事件线程上 build。
/// 会话已存在时 `capture_and_cache` 会直接返回旧会话，`open_overlays` 发现
/// 对应 label 的窗口已在，只做聚焦 —— 两层遮罩不会叠加。
pub fn begin_blocking(app: &tauri::AppHandle, state: &ShotState) -> Result<ShotCapture, ShotErr> {
    let capture = capture_and_cache(app, state)?;
    open_overlays(app, &capture)?;
    Ok(capture)
}

/// 显示器清单：逻辑矩形来自窗口系统的真实布局，物理矩形由 k 推得
fn collect_monitors(app: &tauri::AppHandle, image_w: u32) -> Result<Vec<ShotMonitor>, ShotErr> {
    let logical = logical_monitors(app)?;
    let bounds = virtual_bounds(&logical.iter().map(|(_, r)| *r).collect::<Vec<_>>());
    let k = scale_for(image_w, bounds.w.max(1));
    Ok(logical
        .into_iter()
        .enumerate()
        .map(|(i, (name, r))| ShotMonitor {
            index: i,
            name,
            px: slice_for_monitor(r, bounds, k),
            logical: r,
        })
        .collect())
}

/// Linux：逻辑几何必须直接问 GDK。
///
/// tao 的 `Monitor::position()/size()` 是「GDK 逻辑 × GDK 整数缩放」，本机
/// 真实比例 1.25 却按 2 乘，会得到第三套坐标，遮罩必然错位。
#[cfg(target_os = "linux")]
fn logical_monitors(app: &tauri::AppHandle) -> Result<Vec<(String, Rect)>, ShotErr> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        use gtk::prelude::*;
        let list = gtk::gdk::Display::default()
            .map(|d| {
                // GDK3（gtk 0.18）没有 `display.monitors()`：按序号逐个取，
                // 这样 index 与 `fullscreen_on_monitor` 用的序号同源
                (0..d.n_monitors())
                    .filter_map(|i| {
                        let m = d.monitor(i)?;
                        let g = m.geometry();
                        let name = m
                            .model()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("monitor-{i}"));
                        Some((
                            name,
                            Rect { x: g.x(), y: g.y(), w: g.width() as u32, h: g.height() as u32 },
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = tx.send(list);
    })
    .map_err(|e| ShotErr::CaptureFailed(format!("枚举显示器失败: {e}")))?;
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| ShotErr::CaptureFailed("枚举显示器超时".into()))
}

#[cfg(not(target_os = "linux"))]
fn logical_monitors(app: &tauri::AppHandle) -> Result<Vec<(String, Rect)>, ShotErr> {
    let mons = app
        .available_monitors()
        .map_err(|e| ShotErr::CaptureFailed(format!("枚举显示器失败: {e}")))?;
    Ok(mons
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let scale = m.scale_factor().max(0.1);
            let pos = m.position();
            let size = m.size();
            (
                m.name().cloned().unwrap_or_else(|| format!("monitor-{i}")),
                Rect {
                    x: (pos.x as f64 / scale).round() as i32,
                    y: (pos.y as f64 / scale).round() as i32,
                    w: (size.width as f64 / scale).round() as u32,
                    h: (size.height as f64 / scale).round() as u32,
                },
            )
        })
        .collect())
}

/// 是否 Wayland 会话（决定遮罩窗口是「每屏一个全屏」还是「一个跨虚拟桌面」）
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false)
        || std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
}

/// 建立遮罩窗口
fn open_overlays(app: &tauri::AppHandle, cap: &ShotCapture) -> Result<(), ShotErr> {
    let wayland = is_wayland();
    let count = if wayland { cap.monitors.len().max(1) } else { 1 };
    for i in 0..count {
        let label = format!("shot-overlay-{i}");
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.set_focus();
            continue;
        }
        let url = format!("index.html?viewer=shot&session={}&i={i}", cap.session);
        let boot = format!(
            "window.__OIM_SHOT__ = {};",
            json!({ "session": cap.session, "index": i })
        );
        let mut builder =
            tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App(url.into()))
                .initialization_script(boot)
                .title("Screenshot")
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .shadow(false)
                .focused(true);
        if wayland {
            // Wayland 不允许客户端定位窗口：先建小窗，再在 GTK 主线程上指定显示器全屏。
            //
            // **这里绝不能设 resizable(false)**：GDK 会把「不可缩放」翻译成
            // xdg_toplevel 的 min/max 尺寸一起钉死（实测 set_min_size/set_max_size
            // 都发 410×290，即当时的占位尺寸）。全屏请求虽然被 KWin 接受
            // （configure(2048, 1152, [FULLSCREEN])），但 max_size 随后把窗口打回
            // 410×290 —— 屏幕上只剩一块小方块。全屏窗口本来也不该让用户拖大小。
            builder = builder.inner_size(320.0, 200.0);
        } else {
            let bounds = virtual_bounds(&cap.monitors.iter().map(|m| m.logical).collect::<Vec<_>>());
            builder = builder
                .resizable(false)
                .position(bounds.x as f64, bounds.y as f64)
                .inner_size(bounds.w.max(1) as f64, bounds.h.max(1) as f64);
        }
        let win = builder
            .build()
            .map_err(|e| ShotErr::CaptureFailed(format!("创建遮罩窗口失败: {e}")))?;
        // 用户 Alt+F4 关掉遮罩时也要释放会话缓存，否则下次触发会拿到陈旧会话
        let watcher = app.clone();
        win.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                let remaining = watcher
                    .webview_windows()
                    .keys()
                    .any(|l| l.starts_with("shot-overlay-"));
                if !remaining {
                    watcher.state::<ShotState>().clear();
                    oim_log!("[shot] 遮罩全部关闭，会话缓存已释放");
                }
            }
        });
        if wayland {
            fullscreen_on_monitor(&win, i)?;
        } else {
            let _ = win.set_focus();
        }
    }
    Ok(())
}

/// Wayland：请求在指定显示器上全屏（xdg-shell 的 set_fullscreen 支持 output）。
///
/// 时序：**先映射、后请求**。GDK 的 xdg_toplevel 是窗口映射时才建出来的，未映射
/// 就 `fullscreen_on_monitor` 会被丢掉；而 `show_all()` 之后未必立刻映射，所以
/// 已映射时走 idle、未映射时等 map 信号再进 idle —— 两条路都保证请求发生在
/// 映射完成之后（也顺带排在 tao/wry 排队的尺寸请求之后）。
#[cfg(target_os = "linux")]
fn fullscreen_on_monitor(win: &tauri::WebviewWindow, index: usize) -> Result<(), ShotErr> {
    let w = win.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    win.app_handle()
        .run_on_main_thread(move || {
            use gtk::prelude::*;
            if let Ok(gw) = w.gtk_window() {
                gw.show_all();
                if gw.is_mapped() {
                    let gw = gw.clone();
                    gtk::glib::idle_add_local_once(move || request_fullscreen(&gw, index));
                } else {
                    gw.connect_map(move |gw| {
                        let gw = gw.clone();
                        gtk::glib::idle_add_local_once(move || request_fullscreen(&gw, index));
                    });
                }
            }
            let _ = tx.send(());
        })
        .map_err(|e| ShotErr::CaptureFailed(format!("请求全屏失败: {e}")))?;
    let _ = rx.recv_timeout(Duration::from_secs(3));
    Ok(())
}

/// 发出全屏请求。**只能在窗口映射之后调用**（见 `fullscreen_on_monitor` 的注释），
/// 单独拆出来是为了能同时被「已映射」与「map 信号」两条路径复用。
#[cfg(target_os = "linux")]
fn request_fullscreen(gw: &gtk::ApplicationWindow, index: usize) {
    use gtk::prelude::*;
    // GDK3 的签名是 `fullscreen_on_monitor(&Screen, monitor 序号)`，
    // 屏幕取窗口自身所在的那块（取不到再退默认屏）；
    // `screen` 在 GtkWindowExt 与 WidgetExt 上都有，必须写全路径
    let screen = gtk::prelude::GtkWindowExt::screen(gw).or_else(gtk::gdk::Screen::default);
    let exists = gtk::gdk::Display::default()
        .and_then(|d| d.monitor(index as i32))
        .is_some();
    match (screen, exists) {
        (Some(s), true) => {
            gw.fullscreen_on_monitor(&s, index as i32);
            oim_log!("[shot] 遮罩已请求在 {index} 号屏全屏");
        }
        // 取不到该显示器就退化为普通全屏（落在窗口当前所在屏）
        _ => {
            gw.fullscreen();
            oim_log!("[shot] 遮罩未找到 {index} 号屏，退化为普通全屏");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn fullscreen_on_monitor(win: &tauri::WebviewWindow, _index: usize) -> Result<(), ShotErr> {
    win.set_fullscreen(true)
        .map_err(|e| ShotErr::CaptureFailed(format!("请求全屏失败: {e}")))
}

/* ---------------- Tauri 命令 ---------------- */

#[tauri::command]
pub async fn start_screenshot(app: tauri::AppHandle) -> Result<ShotCapture, String> {
    // 抓屏最长阻塞 15s：放到 blocking 线程，绝不占住 tokio worker
    let app2 = app.clone();
    let capture = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<ShotState>();
        capture_and_cache(&app2, &state)
    })
    .await
    .map_err(|e| format!("CAPTURE_FAILED|抓屏任务失败: {e}"))?
    .map_err(|e| format!("{}|{}", e.code(), e.message()))?;

    // 开窗回到命令线程：与 open_image_viewer 同一写法（已在本仓库验证过）
    if let Err(e) = open_overlays(&app, &capture) {
        return Err(format!("{}|{}", e.code(), e.message()));
    }
    oim_log!(
        "[shot] 抓屏成功 {}x{}，{} 块屏，会话 {}",
        capture.width,
        capture.height,
        capture.monitors.len(),
        capture.session
    );
    Ok(capture)
}

#[tauri::command]
pub async fn shot_image(
    session: String,
    index: usize,
    state: tauri::State<'_, ShotState>,
) -> Result<Value, String> {
    let cached = state.get(&session).map_err(|e| e.message())?;
    let mon = monitor_at(&cached.monitors, index).map_err(|e| e.message())?;
    // Wayland：每块屏一个全屏窗口 → 返回该屏在整幅图里的切片
    // 其他平台：只有一个覆盖整个虚拟桌面的窗口 → 必须返回整幅图，
    //           否则双屏下窗口里只会画出 0 号屏的内容（错位 / 缺半屏）
    let (slice, logical_w) = if is_wayland() {
        (mon.px, mon.logical.w.max(1))
    } else {
        let bounds =
            virtual_bounds(&cached.monitors.iter().map(|m| m.logical).collect::<Vec<_>>());
        (
            Rect { x: 0, y: 0, w: cached.width, h: cached.height },
            bounds.w.max(1),
        )
    };
    let scale = slice.w as f64 / logical_w as f64;
    Ok(json!({
        "b64": cached.png_b64,
        "mime": "image/png",
        "slice": slice,
        "scale": scale,
        "total": { "w": cached.width, "h": cached.height },
    }))
}

#[tauri::command]
pub async fn close_shot_overlays(
    app: tauri::AppHandle,
    session: String,
    state: tauri::State<'_, ShotState>,
) -> Result<(), String> {
    for (label, w) in app.webview_windows() {
        if label.starts_with("shot-overlay-") {
            let _ = w.destroy();
        }
    }
    if state.active_session().as_deref() == Some(session.as_str()) {
        state.clear();
    }
    oim_log!("[shot] 遮罩已关闭 session={session}");
    Ok(())
}

#[tauri::command]
pub async fn save_shot_png(b64: String, path: String) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("图片数据非法: {e}"))?;
    std::fs::write(&path, bytes).map_err(|e| format!("保存失败: {e}"))
}

/// 把 PNG 写进系统剪贴板（Linux 走 GTK：与现有 clipboard_image 读路径对称）
#[tauri::command]
pub async fn copy_image_to_clipboard(app: tauri::AppHandle, b64: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| format!("图片数据非法: {e}"))?;
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        app.run_on_main_thread(move || {
            use gtk::prelude::*;
            let loader = gtk::gdk_pixbuf::PixbufLoader::new();
            let ok = loader.write(&bytes).is_ok()
                && loader.close().is_ok()
                && loader.pixbuf().is_some_and(|pb| {
                    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_image(&pb);
                    true
                });
            let _ = tx.send(ok);
        })
        .map_err(|e| format!("写剪贴板失败: {e}"))?;
        return match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(true) => Ok(()),
            Ok(false) => Err("剪贴板写入失败（图片解码失败）".into()),
            Err(_) => Err("写剪贴板超时".into()),
        };
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows/macOS 由前端用 clipboard-manager 插件写图片
        let _ = (app, b64);
        Err("PLUGIN".into())
    }
}

/// 供 shortcut.rs / 命令行调用：抓屏并建遮罩。
///
/// 这两个调用点都在主线程上（插件回调 / setup / 单实例回调），而抓屏要阻塞
/// 十几秒 —— 所以自己起线程做完整流程（Tauri 的建窗可以从任意线程发起，
/// 与 `open_image_viewer` 同一个机制），主线程立刻返回。
pub fn trigger(app: &tauri::AppHandle) -> Result<(), ShotErr> {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let state = app2.state::<ShotState>();
        if let Err(e) = begin_blocking(&app2, &state) {
            oim_log!("[shot] 触发失败 [{}]：{}", e.code(), e.message());
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_bounds_covers_negative_and_positive_positions() {
        let m = [
            Rect { x: -1920, y: 0, w: 1920, h: 1080 },
            Rect { x: 0, y: 0, w: 2560, h: 1440 },
        ];
        assert_eq!(virtual_bounds(&m), Rect { x: -1920, y: 0, w: 4480, h: 1440 });
        assert_eq!(virtual_bounds(&[]), Rect { x: 0, y: 0, w: 0, h: 0 });
    }

    #[test]
    fn scale_for_uses_image_width_over_logical_width() {
        // 本机实测：两屏逻辑宽 2048+2048=4096，整幅图 5120 → 1.25
        assert_eq!(scale_for(5120, 4096), 1.25);
        assert_eq!(scale_for(1920, 1920), 1.0);
        // 逻辑宽为 0 时退化为 1，绝不产生 inf/NaN
        assert_eq!(scale_for(5120, 0), 1.0);
    }

    #[test]
    fn slice_for_monitor_offsets_by_virtual_bounds() {
        let bounds = Rect { x: -1920, y: 0, w: 4480, h: 1440 };
        let k = 1.0;
        assert_eq!(
            slice_for_monitor(Rect { x: -1920, y: 0, w: 1920, h: 1080 }, bounds, k),
            Rect { x: 0, y: 0, w: 1920, h: 1080 },
        );
        assert_eq!(
            slice_for_monitor(Rect { x: 0, y: 0, w: 2560, h: 1440 }, bounds, k),
            Rect { x: 1920, y: 0, w: 2560, h: 1440 },
        );
        // 1.25 倍：副屏 2048 逻辑宽 → 2560 物理宽，起点 0 + (2048×1.25)
        assert_eq!(
            slice_for_monitor(
                Rect { x: 2048, y: 0, w: 2048, h: 1152 },
                Rect { x: 0, y: 0, w: 4096, h: 1152 },
                1.25
            ),
            Rect { x: 2560, y: 0, w: 2560, h: 1440 },
        );
    }

    #[test]
    fn converts_bgra_rows_honouring_stride() {
        // 2×2，stride 比行宽多 4 字节填充；源是 BGRA，目标是 RGBA
        let src: Vec<u8> = vec![
            1, 2, 3, 255, 4, 5, 6, 255, 9, 9, 9, 9, // 第一行 + 填充
            7, 8, 9, 255, 10, 11, 12, 255, 9, 9, 9, 9,
        ];
        assert_eq!(
            bgra_to_rgba(&src, 2, 2, 12),
            vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255],
        );
        // 源数据不足时不 panic，缺的部分补全透明黑
        assert_eq!(bgra_to_rgba(&src[..14], 2, 2, 12).len(), 16);
    }

    #[test]
    fn error_codes_and_messages_are_stable() {
        assert_eq!(ShotErr::Timeout.code(), "PORTAL_TIMEOUT");
        assert_eq!(ShotErr::PortalDenied(2).code(), "PORTAL_DENIED");
        assert_eq!(ShotErr::MacPermission.code(), "MAC_PERMISSION");
        assert!(ShotErr::PortalDenied(2).message().contains('2'));
        // 错误码是给前端做分支判断用的，必须是稳定的大写常量
        for e in [
            ShotErr::PortalMissing("x".into()),
            ShotErr::PortalDenied(1),
            ShotErr::Timeout,
            ShotErr::Decode("x".into()),
            ShotErr::MacPermission,
            ShotErr::CaptureFailed("x".into()),
        ] {
            assert!(e.code().chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }

    #[test]
    fn png_dimensions_are_read_without_decoding_failure() {
        // 用 image 现场编码一张 1×1 再解回来：不依赖手写 PNG 字节常量
        // （手写常量一旦 IDAT 长度写错，测试失败会指向错误的方向）
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[0u8, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode");
        let c = decode_captured(png).expect("decode");
        assert_eq!((c.width, c.height), (1, 1));
    }

    #[test]
    fn shot_cache_expires_and_is_single_slot() {
        let cache = ShotState::default();
        assert!(cache.get("s1").is_err(), "空缓存应取不到");
        cache.put(CachedShot {
            session: "s1".into(),
            png_b64: "AAA".into(),
            width: 10,
            height: 10,
            monitors: vec![],
        });
        assert_eq!(cache.get("s1").unwrap().png_b64, "AAA");
        // 会话 id 不匹配（旧遮罩窗口）取不到
        assert!(cache.get("s0").is_err());
        // 新会话替换旧会话：旧 id 立即失效
        cache.put(CachedShot {
            session: "s2".into(),
            png_b64: "BBB".into(),
            width: 10,
            height: 10,
            monitors: vec![],
        });
        assert!(cache.get("s1").is_err());
        assert_eq!(cache.get("s2").unwrap().png_b64, "BBB");
        // close 幂等
        cache.clear();
        cache.clear();
        assert!(cache.get("s2").is_err());
    }

    #[test]
    fn monitor_index_out_of_range_is_rejected() {
        let mons = vec![ShotMonitor {
            index: 0,
            name: "DP-1".into(),
            px: Rect { x: 0, y: 0, w: 8, h: 8 },
            logical: Rect { x: 0, y: 0, w: 8, h: 8 },
        }];
        assert!(monitor_at(&mons, 0).is_ok());
        assert_eq!(monitor_at(&mons, 3).unwrap_err().code(), "CAPTURE_FAILED");
    }
}
