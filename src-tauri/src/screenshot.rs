//! 截图子系统：抓屏后端 / 会话缓存 / 遮罩窗口 / 命令。
//!
//! 坐标约定：`logical` 是窗口系统逻辑像素（开窗与遮罩定位用），
//! `px` 是整幅抓屏图像里的物理像素（裁剪用）。两者的换算只在这里做一次，
//! 前端拿到的永远是物理像素矩形。

use serde::Serialize;
use std::time::Duration;

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
}
