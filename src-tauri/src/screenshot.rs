//! 截图子系统：抓屏后端 / 会话缓存 / 遮罩窗口 / 命令。
//!
//! 坐标约定：`logical` 是窗口系统逻辑像素（开窗与遮罩定位用），
//! `px` 是整幅抓屏图像里的物理像素（裁剪用）。两者的换算只在这里做一次，
//! 前端拿到的永远是物理像素矩形。

use serde::Serialize;

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
}
