#!/usr/bin/env python3
"""生成截图遮罩「画笔」工具的自定义光标图片（纯标准库实现，与 gen_icons.py 同风格）。

CSS 没有「笔」这个光标关键字，只能自带图片：32×32 PNG，热点在笔尖 (3,3)。
- 选 PNG 不选 SVG：遮罩窗口跑在 WebKitGTK(Linux) / WebView2(Windows) / WKWebView(macOS)
  三种内核上，WebKitGTK 的光标图走 gdk-pixbuf，不保证装了 SVG loader —— PNG 三种内核都稳。
- 边长卡 32×32：Windows 上 `cursor: url()` 的自定义光标上限就是 32×32；超过会被缩，
  高 DPI 下也只会轻微软化，不至于变形。
- 图片之外还要自带 crosshair 兜底（写在 shot.js 的 cursorFor 里）：万一图片没加载出来，
  退化的是十字准星，而不是浏览器的默认箭头。

画法：多边形在 8 倍超采样下按扫描线填充，再盒式降采样 → 抗锯齿边缘；白色外描边是为了
在任意截图底色（深色桌面 / 白底文档）上都看得见 —— 系统光标主题也是这个套路。

输出：src/lib/shotCursor.js —— 改图形后重跑本脚本，不要手改里面的 base64。
"""
import base64
import math
import struct
import textwrap
import zlib
from pathlib import Path

N = 32            # 输出边长（CSS px）
SS = 8            # 超采样倍数
TIP = (3.0, 3.0)  # 笔尖坐标 = 光标热点
W = 2.9           # 笔杆半宽
NIB = 6.5         # 笔尖到笔杆的轴向长度
LEN = 16.0        # 笔杆长度

U = (math.sqrt(0.5), math.sqrt(0.5))    # 笔轴：从笔尖指向笔尾（右下）
P = (-math.sqrt(0.5), math.sqrt(0.5))   # 垂直方向

OUT = Path(__file__).resolve().parent.parent / "src" / "lib" / "shotCursor.js"


def silhouette(tip=0.0, grow=0.0, nib=NIB, length=LEN):
    """笔的剪影多边形。tip 沿轴向移动起点（负值 = 往笔尖外扩），grow 是半宽/笔帽半径的增量。"""
    t = (TIP[0] + tip * U[0], TIP[1] + tip * U[1])
    w = W + grow
    b = (t[0] + nib * U[0], t[1] + nib * U[1])
    c = (b[0] + length * U[0], b[1] + length * U[1])
    pts = [t, (b[0] + w * P[0], b[1] + w * P[1]), (c[0] + w * P[0], c[1] + w * P[1])]
    # 笔帽的半圆：从 +P 一侧绕过轴向到 -P 一侧（135° → -45°）
    steps = 24
    for i in range(steps + 1):
        a = math.radians(135 - 180 * i / steps)
        pts.append((c[0] + w * math.cos(a), c[1] + w * math.sin(a)))
    pts += [(c[0] - w * P[0], c[1] - w * P[1]), (b[0] - w * P[0], b[1] - w * P[1])]
    return pts


def coverage(poly):
    """8 倍超采样扫描线填充 → N×N 覆盖率（0..1），即该多边形的抗锯齿 alpha 蒙版。"""
    res = N * SS
    rows = [bytearray(res) for _ in range(res)]
    ys = [y for _, y in poly]
    y0 = max(0, int(math.floor(min(ys) * SS)))
    y1 = min(res - 1, int(math.ceil(max(ys) * SS)))
    n = len(poly)
    for py in range(y0, y1 + 1):
        yc = (py + 0.5) / SS
        xs = []
        for i in range(n):
            x1, ya = poly[i]
            x2, yb = poly[(i + 1) % n]
            if (ya <= yc < yb) or (yb <= yc < ya):
                xs.append(x1 + (yc - ya) / (yb - ya) * (x2 - x1))
        xs.sort()
        row = rows[py]
        for i in range(0, len(xs) - 1, 2):
            xa = max(0, int(math.ceil(xs[i] * SS - 0.5)))
            xb = min(res - 1, int(math.floor(xs[i + 1] * SS - 0.5)))
            row[xa:xb + 1] = b"\x01" * (xb + 1 - xa)
    cov = [[0.0] * N for _ in range(N)]
    inv = 1.0 / (SS * SS)
    for y in range(N):
        for x in range(N):
            s = 0
            for dy in range(SS):
                row = rows[y * SS + dy]
                s += sum(row[x * SS:(x + 1) * SS])
            cov[y][x] = s * inv
    return cov


# 由下到上：白色外描边 → 深色笔身（含笔尖） → 浅色笔杆
LAYERS = [
    (silhouette(tip=-1.5, grow=1.5), (255, 255, 255)),
    (silhouette(), (27, 27, 27)),
    (silhouette(tip=6.0, grow=-1.15), (233, 233, 233)),
]


def render():
    px = [[(0.0, 0.0, 0.0, 0.0) for _ in range(N)] for _ in range(N)]
    for poly, rgb in LAYERS:
        cov = coverage(poly)
        for y in range(N):
            for x in range(N):
                a = cov[y][x]
                if a <= 0:
                    continue
                r, g, b, da = px[y][x]
                px[y][x] = (
                    rgb[0] * a + r * (1 - a),
                    rgb[1] * a + g * (1 - a),
                    rgb[2] * a + b * (1 - a),
                    a + da * (1 - a),
                )
    return px


def png_chunk(tag, data):
    return (struct.pack(">I", len(data)) + tag + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))


def png_bytes(px):
    raw = bytearray()
    for y in range(N):
        raw.append(0)  # filter: None
        for x in range(N):
            r, g, b, a = px[y][x]
            raw += bytes((round(r), round(g), round(b), round(a * 255)))
    ihdr = struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)  # 8bit RGBA
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
            + png_chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + png_chunk(b"IEND", b""))


def main():
    data = png_bytes(render())
    b64 = base64.b64encode(data).decode()
    expr = "\n  + ".join(
        ["'url(\"data:image/png;base64,'"] + [f"'{c}'" for c in textwrap.wrap(b64, 96)]
        + ["'\") 3 3, crosshair'"]
    )
    OUT.write_text(f'''/**
 * 截图「画笔」工具的光标图片 —— **生成文件，别手改**（改图形请重跑
 * `scripts/gen_shot_cursor.py`，那里有画法与尺寸取舍的完整说明）。
 *
 * 32×32 PNG，热点在笔尖 (3,3)；结尾的 `crosshair` 是兜底：图片没加载出来时退化成
 * 十字准星，而不是默认箭头。CSS 没有「笔」这个关键字，只能自带图片。
 */
export const PEN_CURSOR =
  {expr}
''', encoding="utf-8")
    print(f"{OUT.relative_to(OUT.parent.parent.parent)}: {len(data)} bytes PNG, "
          f"{len(b64)} base64 chars, {N}x{N}, hotspot {TIP[0]:.0f},{TIP[1]:.0f}")


if __name__ == "__main__":
    main()
