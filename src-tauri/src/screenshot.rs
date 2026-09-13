//! 截图子系统：抓屏后端 / 会话缓存 / 遮罩窗口 / 命令。
//!
//! 坐标约定：`logical` 是窗口系统逻辑像素（开窗与遮罩定位用），
//! `px` 是整幅抓屏图像里的物理像素（裁剪用）。两者的换算只在这里做一次，
//! 前端拿到的永远是物理像素矩形。

use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
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
                // 「必须重启」不是客套：TCC 的屏幕录制授权对已运行的进程不生效，
                // 不重启的话用户授了权也还是抓不到图（见 platform::has_permission）
                "需要「屏幕录制」权限：系统设置 → 隐私与安全性 → 屏幕录制，\
                 勾选本应用后重启应用生效"
                    .into()
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

/// 其余平台（非 Linux / Windows / macOS）：占位，保持 `platform::capture()` 在
/// 任何 target 上都存在，不参与实际功能。
#[cfg(all(not(target_os = "linux"), not(target_os = "windows"), not(target_os = "macos")))]
mod platform {
    use super::ShotErr;

    pub fn capture() -> Result<super::Captured, ShotErr> {
        Err(ShotErr::CaptureFailed("当前平台尚未实现抓屏".into()))
    }
}

/// Windows：GDI BitBlt 抓整个虚拟桌面。
///
/// 选 BitBlt 而不是 DXGI Desktop Duplication：后者要求 D3D11 设备、在混合
/// 显卡/远程桌面下容易失败，而抓屏只需要「屏幕现在长什么样」这一件事。
#[cfg(target_os = "windows")]
mod platform {
    use super::{bgra_to_rgba, Captured, ShotErr};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    pub fn capture() -> Result<Captured, ShotErr> {
        unsafe {
            // 虚拟桌面（所有显示的并集）：副屏在主屏左侧时 x 为负，必须按
            // 「虚拟桌面原点」抓，而不是按主屏原点
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let w = GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32;
            let h = GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32;
            if w == 0 || h == 0 {
                return Err(ShotErr::CaptureFailed("虚拟桌面尺寸为 0".into()));
            }
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
            // 句柄创建失败必须显式拒绝：CreateCompatibleBitmap 返回 NULL 时 BitBlt 会
            // 「成功」地画进 DC 自带的 1×1 单色位图，GetDIBits 再把它转成 32bpp ——
            // 结果是返回一张全屏黑白图，比直接报错难查得多。
            if mem.is_null() || bmp.is_null() {
                if !bmp.is_null() {
                    DeleteObject(bmp);
                }
                if !mem.is_null() {
                    DeleteDC(mem);
                }
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ShotErr::CaptureFailed("创建 GDI 位图失败".into()));
            }
            let old = SelectObject(mem, bmp);
            // CAPTUREBLT 才能抓到分层窗口（否则只有桌面壁纸）
            let ok = BitBlt(mem, 0, 0, w as i32, h as i32, screen, x, y, SRCCOPY | CAPTUREBLT);
            // 先摘下位图再谈别的：
            //  · DeleteObject 对「仍被选入 DC」的位图不会真正释放（每次失败泄漏一张全屏位图）
            //  · GetDIBits 的文档前置条件同样要求 hbmp 未被选入任何 DC
            SelectObject(mem, old);
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                DeleteObject(bmp);
                DeleteDC(mem);
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ShotErr::CaptureFailed(format!("BitBlt 失败（{e}）")));
            }
            let stride = ((w * 32 + 31) / 32 * 4) as usize;
            let mut buf = vec![0u8; stride * h as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // 负高度 = 自顶向下，省掉一次翻转
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let lines = GetDIBits(
                mem,
                bmp,
                0,
                h,
                buf.as_mut_ptr() as *mut _,
                &mut info,
                DIB_RGB_COLORS,
            );
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            // GetDIBits 返回「实际写回的行数」：少于 h 说明只拿到部分图像，
            // 当成成功会得到一张下半截全黑的图（比直接报错更难排查）
            if lines != h as i32 {
                return Err(ShotErr::CaptureFailed(format!(
                    "GetDIBits 只写回 {lines} 行（应为 {h} 行）"
                )));
            }
            let mut rgba = bgra_to_rgba(&buf, w, h, stride);
            // BitBlt 到 DIB 的 alpha 字节是未定义的（实测常见为 0）。若原样当成
            // 透明度用，PNG 会是一张全透明图 —— 抓屏必须强制不透明。
            for px in rgba.chunks_exact_mut(4) {
                px[3] = 255;
            }
            encode_png(rgba, w, h)
        }
    }

    fn encode_png(rgba: Vec<u8>, w: u32, h: u32) -> Result<Captured, ShotErr> {
        use image::ImageEncoder;
        let img = image::RgbaImage::from_raw(w, h, rgba)
            .ok_or_else(|| ShotErr::CaptureFailed("缓冲尺寸不匹配".into()))?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| ShotErr::CaptureFailed(format!("PNG 编码失败: {e}")))?;
        Ok(Captured { png, width: w, height: h })
    }
}

/// macOS：CoreGraphics 逐显示器抓图，再按各自的 bounds 拼成一张虚拟桌面图。
#[cfg(target_os = "macos")]
mod platform {
    use super::{Captured, ShotErr};
    use core_graphics::display::CGDisplay;

    /// 未授权时 CGDisplayCreateImage 会静默返回一张只有桌面壁纸的图 ——
    /// 这是最难排查的失败形态，所以先做权限预检并给出明确文案。
    pub fn capture() -> Result<Captured, ShotErr> {
        if !has_permission() {
            return Err(ShotErr::MacPermission);
        }
        let ids =
            CGDisplay::active_displays().map_err(|e| ShotErr::CaptureFailed(format!("{e:?}")))?;
        // 多屏按 bounds 的并集拼接：副屏排在主屏左侧/上方时原点为负
        let mut shots = Vec::new();
        let mut min_x = i64::MAX;
        let mut min_y = i64::MAX;
        let mut max_x = i64::MIN;
        let mut max_y = i64::MIN;
        for id in ids {
            let display = CGDisplay::new(id);
            let b = display.bounds();
            min_x = min_x.min(b.origin.x as i64);
            min_y = min_y.min(b.origin.y as i64);
            max_x = max_x.max((b.origin.x + b.size.width) as i64);
            max_y = max_y.max((b.origin.y + b.size.height) as i64);
            let img = display
                .image()
                .ok_or_else(|| ShotErr::CaptureFailed(format!("抓取显示器 {id} 失败")))?;
            shots.push((b, img));
        }
        if shots.is_empty() {
            return Err(ShotErr::CaptureFailed("没有可用显示器".into()));
        }
        // bounds 是逻辑点、image 是物理像素，Retina 上差 2 倍。
        //
        // 简报这里写的是 `img.bounds()`，但 core-graphics 0.24.0 的 `CGImage`
        // **没有** `bounds()` 方法（只有 `width()/height()`），照抄会直接编译不过。
        // 而它想算的就是「一点等于几像素」—— 分母换成显示器自身的 bounds 既成立，
        // 也才与下面的 ox/oy 同源。若沿用 img 自身的尺寸，比值恒为 1.0：
        // Retina 上画布只有真实画面的 1/4，PNG 里只剩左上角那一块。
        let scale = shots
            .first()
            .map(|(b, img)| img.width() as f64 / b.size.width.max(1.0))
            .unwrap_or(1.0);
        let w = ((max_x - min_x) as f64 * scale).round() as u32;
        let h = ((max_y - min_y) as f64 * scale).round() as u32;
        let mut canvas = image::RgbaImage::new(w, h);
        for (b, img) in shots {
            let sw = img.width();
            let sh = img.height();
            let mut data = vec![0u8; sw * sh * 4];
            let ctx = core_graphics::context::CGContext::create_bitmap_context(
                Some(data.as_mut_ptr() as *mut _),
                sw,
                sh,
                8,
                sw * 4,
                &core_graphics::color_space::CGColorSpace::create_device_rgb(),
                core_graphics::base::kCGImageAlphaPremultipliedLast,
            );
            ctx.draw_image(
                core_graphics::geometry::CGRect::new(
                    &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                    &core_graphics::geometry::CGSize::new(sw as f64, sh as f64),
                ),
                &img,
            );
            // 该屏在画布里的物理像素原点
            let ox = ((b.origin.x as i64 - min_x) as f64 * scale).round() as i64;
            let oy = ((b.origin.y as i64 - min_y) as f64 * scale).round() as i64;
            for y in 0..sh {
                for x in 0..sw {
                    let si = (y * sw + x) * 4;
                    let dx = ox + x as i64;
                    let dy = oy + y as i64;
                    if dx < 0 || dy < 0 || dx >= w as i64 || dy >= h as i64 {
                        continue;
                    }
                    canvas.put_pixel(
                        dx as u32,
                        dy as u32,
                        image::Rgba([data[si], data[si + 1], data[si + 2], data[si + 3]]),
                    );
                }
            }
        }
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(canvas.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| ShotErr::CaptureFailed(format!("PNG 编码失败: {e}")))?;
        Ok(Captured { png, width: w, height: h })
    }

    /// macOS 的 TCC 屏幕录制权限预检 + 申请。
    ///
    /// 简报的写法是自己声明
    /// `extern "C" { fn CGPreflightScreenCaptureAccess() -> bool; }`，
    /// 理由是「core-graphics 0.24 不导出这个符号」。这句与 0.24.0 的实际源码不符：
    /// `core_graphics::access::ScreenCaptureAccess::preflight()`（src/access.rs）
    /// 正是它的安全封装，且该模块就挂在 `target_os = "macos"` 下，可直接用。
    /// 另外该符号的 C 原型返回 `boolean_t`（c_uint，4 字节），而 Rust 的 `bool`
    /// 只有 1 字节，手写 `-> bool` 是 ABI 不匹配（aarch64 上属未定义行为）。
    /// 因此改为复用 crate 自带的封装，不再自行声明外部符号。
    ///
    /// 同样用 `ScreenCaptureAccess::request()`（同一个 crate 源码里
    /// `CGRequestScreenCaptureAccess()` 的封装，返回值同样是 `boolean_t` 比较后再转
    /// `bool`）：**只 preflight 不 request 是一个死循环** —— 系统设置里那份
    /// 「屏幕录制」清单只登记「申请过」的应用，首次运行的应用根本不在列表里，
    /// 用户按提示去设置里翻不到本应用，也就永远授权不了；而 preflight 又永远失败。
    /// 所以预检失败时必须发出一次申请，让系统把本应用登记进去。
    fn has_permission() -> bool {
        let access = core_graphics::access::ScreenCaptureAccess;
        if access.preflight() {
            return true;
        }
        // 申请本身在首次运行时可能返回 false（用户还没点授权），也可能已经 true；
        // 这次调用的意义在于「让本应用出现在系统设置的清单里」，返回值不作为放行依据，
        // 一律回到预检结论：本次仍按未授权处理，用户授权并重启后才能抓屏。
        let granted_now = access.request();
        oim_log!("[shot] 屏幕录制权限未授予（本次申请结果：{granted_now}），已请求系统登记本应用");
        false
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
pub struct ShotState {
    cache: Mutex<Option<CachedShot>>,
    /// 抓屏在途标记：抓屏最长阻塞 15 秒，期间必须挡住第二次触发
    capturing: Mutex<bool>,
    /// 已报「底图已画完」（`shot_overlay_ready`）的遮罩 label。
    ///
    /// 看门狗靠它判断某个遮罩是不是**彻底没起来**：窗口是透明窗，页面没跑起来时
    /// 它就是一块看不见的全屏置顶窗，还吃着输入焦点 —— 那种会话必须收掉，
    /// 不能靠「显示窗口」来兜底（窗口本来就已经显示了）。
    ready: Mutex<HashSet<String>>,
}

/// 在途抓屏的 RAII 凭据：Drop 即释放标记，任何返回路径（含 `?` 提前返回）都不会漏放
struct CaptureGuard<'a> {
    busy: &'a Mutex<bool>,
}

impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        // 不用 unwrap：panic 毒化了锁也只该让标记保持「忙」，绝不能在 drop 里再 panic
        if let Ok(mut b) = self.busy.lock() {
            *b = false;
        }
    }
}

impl ShotState {
    /// 认领一次抓屏。已有人在抓则返回 `None`（**不阻塞**：portal 可能要等 15 秒，
    /// 第二次触发要的是立刻被拒，而不是排队到最后拿到一个已被覆盖的会话）。
    fn claim(&self) -> Option<CaptureGuard<'_>> {
        match self.capturing.try_lock() {
            Ok(mut busy) if !*busy => {
                *busy = true;
                Some(CaptureGuard { busy: &self.capturing })
            }
            _ => None,
        }
    }

    /// 「正在截屏」拒绝文案只此一处，测试与调用点共用
    fn busy_err() -> ShotErr {
        ShotErr::CaptureFailed("正在截屏，请稍候".into())
    }

    pub fn put(&self, shot: CachedShot) {
        *self.cache.lock().unwrap() = Some(shot);
    }

    pub fn get(&self, session: &str) -> Result<CachedShot, ShotErr> {
        let guard = self.cache.lock().unwrap();
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
        *self.cache.lock().unwrap() = None;
    }

    /// 遮罩页面报「底图已画完」：登记这个 label，看门狗不再收它
    pub fn mark_ready(&self, label: &str) {
        self.ready.lock().unwrap().insert(label.to_string());
    }

    /// 这个遮罩是不是已经报过就绪（没报过的会被看门狗收掉）
    pub fn is_ready(&self, label: &str) -> bool {
        self.ready.lock().unwrap().contains(label)
    }

    /// 窗口销毁时忘掉它：同名 label 再来必然是全新一轮，必须重新等就绪
    pub fn forget_ready(&self, label: &str) {
        self.ready.lock().unwrap().remove(label);
    }

    /// 整个会话结束时清空
    pub fn clear_ready(&self) {
        self.ready.lock().unwrap().clear();
    }

    pub fn active_session(&self) -> Option<String> {
        self.cache.lock().unwrap().as_ref().map(|s| s.session.clone())
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
    // 先认领再干活：抓屏那 1~15 秒里第二次触发必须立刻被拒。
    //
    // 这一步**必须**排在「已有会话就直接返回」之前：否则第二次触发会绕过在途检查，
    // 抓出第二张图覆盖第一张，而按旧 session 建出来的遮罩窗口拿不到图，
    // 只能报「截图会话已失效，请重新截图」—— 双击按钮就能造出一个死窗口。
    let _claim = match state.claim() {
        Some(g) => g,
        None => {
            oim_log!(
                "[shot] 已有抓屏在途，忽略本次触发：{}",
                ShotState::busy_err().message()
            );
            return Err(ShotState::busy_err());
        }
    };

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
/// 抓屏在途时的第二次触发会被 `capture_and_cache` 直接拒掉（不排队、不建第二层
/// 遮罩）；会话已存在时会复用旧会话，`open_overlays` 按本会话的 label 找到窗口后
/// 只做聚焦 —— 两种情况都不会叠加遮罩。
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

/// 遮罩窗口 label：`shot-overlay-<会话号>-<显示器序号>`。
///
/// 带上会话号是必须的：label 若只按序号命名，上一次截图残留的窗口（例如前端关闭
/// 时漏掉一个）会被新会话原样复用，而它的 URL 与 `window.__OIM_SHOT__` 绑的是旧
/// 会话 —— 新图永远进不去，只能报「截图会话已失效」。前缀在，按前缀查找的
/// close_shot_overlays / Destroyed / capabilities 通配都不受影响。
fn overlay_label(session: &str, index: usize) -> String {
    format!("shot-overlay-{session}-{index}")
}

/// 遮罩窗口的初始化脚本前缀：把 html/body 的底色在**任何页面脚本之前**钉成透明。
///
/// 打包版 `index.html` 用 `<link>` 引入 `global.css`，里面 `body{background:var(--c-card)}`
/// 会在模块脚本（以及组件 `onMounted` 里那句兜底）执行之前就被应用、被画出去；
/// 而遮罩窗口建出来即已映射 —— 那一帧就是用户看到的闪。初始化脚本先于页面脚本运行，
/// 这里插一条 `!important` 规则，与样式表先后顺序无关。
///
/// dev 模式下样式由 vite 用 JS 注入（时机在挂载之前），所以这条规则在 dev 下几乎看不出
/// 差别 —— 它救的是打包版，别因为 dev 干净就把它删了。
///
/// `document.documentElement` 在文档最早期可能还不存在，所以挂三个入口：立即试一次、
/// 下一次 `readystatechange`（'loading' 阶段就会触发）、以及首帧之前的
/// `requestAnimationFrame`（rAF 回调排在绘制之前，赶得上第一帧）。
/// 整段包在 try/catch 里：它绝不能影响后面那句 `window.__OIM_SHOT__` 的赋值。
const OVERLAY_BOOT_CSS: &str = r#"try{(function(){
var css='html,body{background:transparent !important}';
function put(){
  if(!document.documentElement)return false;
  var s=document.createElement('style');
  s.textContent=css;
  document.documentElement.appendChild(s);
  return true;
}
if(put())return;
document.addEventListener('readystatechange',function(){put();},{once:true});
if(typeof requestAnimationFrame==='function')requestAnimationFrame(put);
})();}catch(e){}"#;

/// 建立遮罩窗口
///
/// 防闪烁靠两条，缺一不可：
///  1. **透明窗** + **页面在底图就绪前什么都不画**（见 ScreenshotOverlay.vue 的
///     `painted`）：建窗到出图之间提交的任何一帧都是全透明的，用户什么都看不到；
///  2. **指定屏全屏在建窗时就请求**（`schedule_fullscreen`）：底图因此是在最终尺寸上
///     一次性画好的，不会先以 320×200 占位尺寸露一下。
///
/// 注意这里**不能**改成「先隐藏窗口、等页面报就绪再 show」（Task 15 最初的方案）：
/// GTK 的帧时钟跟着窗口的 map 状态走，未映射的窗口里 `requestAnimationFrame`
/// **一次都不会触发**（实测：隐藏 1.5 秒内 rAF 计数 0，show_all() 后立刻涨到 62）。
/// 也就是说页面压根没法在隐藏状态下等到「帧已提交」，那条路只会让就绪信号永远
/// 迟到，最后靠看门狗在第 3 秒强制显示 —— 实测遮罩要 4.7 秒才出现。
fn open_overlays(app: &tauri::AppHandle, cap: &ShotCapture) -> Result<(), ShotErr> {
    let wayland = is_wayland();
    let count = if wayland { cap.monitors.len().max(1) } else { 1 };
    for i in 0..count {
        // 只认本次会话的 label：旧会话残留的窗口绝不会被这条路「复用」
        let label = overlay_label(&cap.session, i);
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.set_focus();
            continue;
        }
        let url = format!("index.html?viewer=shot&session={}&i={i}", cap.session);
        let boot = format!(
            "{OVERLAY_BOOT_CSS}window.__OIM_SHOT__ = {};",
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
        // 透明窗：窗口建出来就是映射状态（里面必须先有帧，见上面的注释），
        // 未绘制的部分因此是全透明的 —— 这就是「不闪」的那道保险。
        //
        // 透明窗与不透明底色互斥，所以这里不设 background_color。
        //
        // macOS 用 tauri 自己的门：那边的 `transparent()` 要 `macos-private-api`
        // 特性（`tauri.conf.json` 的 `macOSPrivateApi: true` + `tauri/macos-private-api`）。
        // 没开该特性时这个方法在 macOS 上被整个 cfg 掉，写上去会编译不过；开了就自动
        // 生效 —— 所以这里照抄 tauri 的条件，而不是写死 `not(target_os = "macos")`。
        // 维护者待办见设计文档 §15.5（macOS 透明窗未开启前，那边仍有「未绘制白窗」一闪）。
        #[cfg(any(not(target_os = "macos"), feature = "macos-private-api"))]
        {
            builder = builder.transparent(true);
        }
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
        let watcher_label = label.clone();
        win.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                let state = watcher.state::<ShotState>();
                // 就绪标记跟着窗口走：同一个 label 再来是全新一轮，必须重新等就绪
                state.forget_ready(&watcher_label);
                let remaining = watcher
                    .webview_windows()
                    .keys()
                    .any(|l| l.starts_with("shot-overlay-"));
                if !remaining {
                    state.clear();
                    oim_log!("[shot] 遮罩全部关闭，会话缓存已释放");
                }
            }
        });
        // 全屏请求必须赶在页面画底图之前：窗口此刻是映射着的（build 出来的窗口就是
        // 可见状态）、页面还在加载且什么都没画，所以这里显示出来也看不见任何东西。
        if wayland {
            fullscreen_on_monitor(&win, i)?;
        } else {
            let _ = win.set_focus();
        }
        // 兜底看门狗：页面要是始终没报「底图已画完」（模块脚本抛错、webview 卡死、
        // 页面根本没加载出来），窗口里就永远是一块**透明的**全屏置顶窗 —— 用户看不见
        // 它，它却盖住整屏、还抢着输入焦点，会话也一直占着不放。
        // 8 秒（远高于 dev 模式实测 2.5–3.5 秒的页面加载）后仍未就绪，就把本会话的
        // 遮罩全部收掉，让屏幕回到可用状态；已经报过就绪的遮罩不受影响
        // （就绪标记在 ShotState 里，窗口销毁时清掉）。
        let wd = win.clone();
        let wd_label = label.clone();
        let wd_session = cap.session.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(8));
            let state = wd.state::<ShotState>();
            if state.is_ready(&wd_label) {
                return;
            }
            // 同一 session 的遮罩一起收：一块没起来，留着另一块也只是半张遮罩
            let prefix = format!("shot-overlay-{wd_session}-");
            let stale: Vec<tauri::WebviewWindow> = wd
                .app_handle()
                .webview_windows()
                .into_iter()
                .filter(|(l, _)| l.starts_with(&prefix))
                .map(|(_, w)| w)
                .collect();
            if stale.is_empty() {
                return;
            }
            oim_log!(
                "[shot] 遮罩未就绪，已关闭 {wd_label}（本会话共 {} 个遮罩）：8 秒内没等到页面报「底图已画完」",
                stale.len()
            );
            for w in stale {
                let _ = w.destroy();
            }
        });
    }
    Ok(())
}

/// 请求在指定显示器上全屏（xdg-shell 的 set_fullscreen 支持 output）。
///
/// **要求调用者已在 GTK 主线程**；跨线程的调用点走 [`fullscreen_on_monitor`]。
///
/// 时序：**先映射、后请求**。GDK 的 xdg_toplevel 是窗口映射时才建出来的，未映射
/// 就请求全屏会被丢掉；而 `show_all()` 之后未必立刻映射，所以已映射时走 idle、
/// 未映射时等 map 信号再进 idle —— 两条路都保证请求发生在映射完成之后
/// （也顺带排在 tao/wry 排队的尺寸请求之后）。
///
/// 这一段原先整块叫 `fullscreen_on_monitor`（含 `run_on_main_thread`）。Task 15 的就绪
/// 路径本身已经在主线程里，再调那个版本会等自己 → 死锁，于是把主体拆成这个
/// **只在主线程调用**的版本（就绪路径直接调它）；跨线程那半边仍是
/// `fullscreen_on_monitor`（建窗路径与看门狗用）。
#[cfg(target_os = "linux")]
fn schedule_fullscreen(gw: &gtk::ApplicationWindow, index: usize) {
    use gtk::prelude::*;
    // show_all：让窗口进入映射流程（已显示过就是空操作）—— 这就是「先映射」那一步
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

/// `schedule_fullscreen` 的跨线程入口：把请求投到 GTK 主线程，并等它执行完。
#[cfg(target_os = "linux")]
fn fullscreen_on_monitor(win: &tauri::WebviewWindow, index: usize) -> Result<(), ShotErr> {
    let w = win.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    win.app_handle()
        .run_on_main_thread(move || {
            if let Ok(gw) = w.gtk_window() {
                schedule_fullscreen(&gw, index);
            }
            let _ = tx.send(());
        })
        .map_err(|e| ShotErr::CaptureFailed(format!("请求全屏失败: {e}")))?;
    let _ = rx.recv_timeout(Duration::from_secs(3));
    Ok(())
}

/// 发出全屏请求。**只能在窗口映射之后调用**（见 `schedule_fullscreen` 的注释），
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

/// 遮罩页面报告「底图已画完、帧已提交给合成器」。
///
/// 窗口在建出来时就已经显示（透明窗 + 页面在底图就绪前什么都不画，见 `open_overlays`
/// 的注释），所以这里的 `show()` 通常是空操作；它存在的意义是：
///  · 内容真的上屏之后再把窗口端到前台/交回焦点（X11/Windows 的 `set_focus`）；
///  · Wayland 上补一次幂等的「指定屏全屏」重试 —— 建窗时那次请求万一被合成器丢了，
///    这里是第二次机会；
///  · 给 `ShotState` 打上「这个遮罩已经起来了」的标记（建窗时的 8 秒看门狗据此
///    决定要不要把这个会话收掉），并在日志里留一条锚点。
#[tauri::command]
pub async fn shot_overlay_ready(
    app: tauri::AppHandle,
    session: String,
    index: usize,
    state: tauri::State<'_, ShotState>,
) -> Result<(), String> {
    let label = overlay_label(&session, index);
    let win = app
        .get_webview_window(&label)
        .ok_or_else(|| format!("截图遮罩已关闭：{label}"))?;
    // 先登记再动手：看门狗可能正好在 8 秒这一拍上醒来，先登记能让它不再收这个会话
    state.mark_ready(&label);
    let wayland = is_wayland();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    app.run_on_main_thread(move || {
        // show + 全屏在**同一个主线程回合**里做完。
        //
        // 这里绝不能调 fullscreen_on_monitor：它内部自己会 run_on_main_thread，
        // 从主线程里再调一次就是等自己 → 死锁。所以主线程这半边单独拆成了
        // schedule_fullscreen。
        let _ = win.show();
        if wayland {
            #[cfg(target_os = "linux")]
            {
                if let Ok(gw) = win.gtk_window() {
                    schedule_fullscreen(&gw, index);
                }
            }
        } else {
            let _ = win.set_focus();
        }
        let _ = tx.send(());
    })
    .map_err(|e| format!("显示遮罩失败: {e}"))?;
    // 只等到「主线程跑完这一段」；全屏请求本身还排在随后的 idle 回调里。
    // 这一步超时不能报成「已就绪并显示」—— 那就成了没显示却说显示成功
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(()) => oim_log!("[shot] 遮罩已就绪并显示 {label}"),
        Err(_) => oim_log!(
            "[shot] 遮罩 {label} 已报就绪，但主线程显示请求等待超时（窗口可能没能前置/全屏）"
        ),
    }
    Ok(())
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
    // 遮罩全没了，就绪标记没必要留着（Destroyed 处理器也会逐个清）
    state.clear_ready();
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
    fn bgra_to_rgba_handles_odd_width_rows() {
        // 3×1 行宽 12 字节，stride 16（GDI 常见 4 字节对齐）
        let src = vec![
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 0, 0, 0, 0,
        ];
        assert_eq!(
            bgra_to_rgba(&src, 3, 1, 16),
            vec![30, 20, 10, 255, 60, 50, 40, 255, 90, 80, 70, 255],
        );
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
    fn in_flight_capture_claim_refuses_second_trigger_and_releases() {
        let state = ShotState::default();
        // 第一次触发认领成功
        let claim = state.claim().expect("首次认领应当成功");
        // 抓屏在途（1~15 秒）时的第二次触发：立刻被拒，绝不能放第二个抓屏进去
        assert!(state.claim().is_none());
        let e = ShotState::busy_err();
        assert_eq!(e.code(), "CAPTURE_FAILED");
        assert!(e.message().contains("正在截屏"));
        // 释放后可以再次截屏（RAII：任何返回路径都会走到这一步）
        drop(claim);
        assert!(state.claim().is_some(), "释放后应当能再次认领");
    }

    #[test]
    fn overlay_labels_are_session_scoped_but_keep_the_prefix() {
        let a = overlay_label("abc-1", 0);
        assert_eq!(a, "shot-overlay-abc-1-0");
        // 序号相同、会话不同 → label 不同（旧窗口不会被新会话复用）
        assert_ne!(a, overlay_label("def-2", 0));
        // 前缀保持：close_shot_overlays / Destroyed 的处理与 capabilities 的
        // `shot-overlay-*` 通配都靠它
        assert!(a.starts_with("shot-overlay-"));
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
