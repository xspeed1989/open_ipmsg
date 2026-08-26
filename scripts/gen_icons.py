#!/usr/bin/env python3
"""生成应用图标（纯标准库实现）：绿色渐变圆角方块 + 白色对话气泡 + 挖空的「IP」字样。

气泡表明是聊天工具（不用信封，避免被读成邮件客户端），
字样直接点名 IP Messenger —— 24px 托盘尺寸下仍然认得出。
字形是手绘几何（圆角矩形拼合），不依赖任何字体文件。

托盘图标出两态 PNG：正常态 tray.png 与全透明帧 tray_blank.png；有新消息时的
闪烁由 Rust 侧在两者之间交替实现（与微信一致，Electron 同款「两张图来回换」）。
透明帧要真正透明依赖 Windows 图标带真 alpha 通道：tray-icon 已 vendor 修复为
CreateIconIndirect + DIB section（见 Cargo.toml 的 patch 注释与 vendor/tray-icon）。

输出到 src-tauri/icons/: 32x32.png / 128x128.png / 128x128@2x.png / icon.png(512)
            icon.ico / tray.png / tray_blank.png
"""
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"

# ---- 基础 PNG 编码 ----

def png_chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path: Path, w: int, h: int, rgba: bytes) -> None:
    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)  # filter: none
        raw += rgba[y * stride : (y + 1) * stride]
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + png_chunk(b"IEND", b"")
    )
    path.write_bytes(png)


# ---- 绘制 ----

def lerp(a, b, t):
    return a + (b - a) * t


def in_rounded_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    nx = min(max(x, x0 + r), x1 - r)
    ny = min(max(y, y0 + r), y1 - r)
    return (x - nx) ** 2 + (y - ny) ** 2 <= r * r


def in_bubble(x, y):
    """对话气泡（圆角矩形 + 左下尾巴）"""
    if in_rounded_rect(x, y, 0.11, 0.16, 0.89, 0.62, 0.11):
        return True
    tx, tw, th = 0.28, 0.15, 0.16
    if 0.60 <= y <= 0.62 + th and tx <= x <= tx + tw:
        t = (y - 0.60) / (th + 0.02)
        return x <= lerp(tx + tw, tx + 0.02, t)
    return False


# 「IP」字形参数：竖干宽度、字高、字碗尺寸（手绘几何，不依赖字体）
GLY_TOP, GLY_BOT, GLY_W = 0.262, 0.518, 0.061
I_X = 0.305          # I 的竖干左边缘
P_X = 0.445          # P 的竖干左边缘
P_BOWL_W = 0.245     # P 字碗外宽
P_BOWL_H = 0.140     # P 字碗外高


def in_glyph_ip(x, y):
    """气泡里的「IP」—— IP Messenger 的身份，小尺寸下也认得出"""
    # I：一根竖干
    if in_rounded_rect(x, y, I_X, GLY_TOP, I_X + GLY_W, GLY_BOT, GLY_W * 0.45):
        return True
    # P：竖干 + 上方字碗（外框减内框）
    if in_rounded_rect(x, y, P_X, GLY_TOP, P_X + GLY_W, GLY_BOT, GLY_W * 0.45):
        return True
    if in_rounded_rect(x, y, P_X, GLY_TOP, P_X + P_BOWL_W, GLY_TOP + P_BOWL_H, 0.036):
        inner = in_rounded_rect(
            x,
            y,
            P_X + GLY_W,
            GLY_TOP + GLY_W * 0.86,
            P_X + P_BOWL_W - GLY_W,
            GLY_TOP + P_BOWL_H - GLY_W * 0.86,
            0.02,
        )
        return not inner
    return False


def render(size: int) -> bytes:
    ss = size * 3  # 3x 超采样抗锯齿
    img = bytearray(ss * ss * 4)
    c00 = (0x33, 0xD8, 0x77)  # 左上浅绿
    c11 = (0x05, 0xB8, 0x5A)  # 右下深绿

    def px(i, j):
        """返回 (r,g,b,a)，坐标为超采样空间 [0,ss)"""
        x, y = i / ss, j / ss
        t = (x + y) / 2
        r = lerp(c00[0], c11[0], t)
        g = lerp(c00[1], c11[1], t)
        b = lerp(c00[2], c11[2], t)
        a = 255 if in_rounded_rect(x, y, 0.02, 0.02, 0.98, 0.98, 0.21) else 0
        if a and in_bubble(x, y):
            # 气泡白色，字用底色挖出来
            if not in_glyph_ip(x, y):
                r = g = b = 255
        return r, g, b, a

    k = 0
    for j in range(ss):
        for i in range(ss):
            r, g, b, a = px(i, j)
            img[k] = int(r); img[k+1] = int(g); img[k+2] = int(b); img[k+3] = int(a)
            k += 4

    # 盒式降采样 ss -> size
    f = ss // size
    out = bytearray(size * size * 4)
    n = f * f
    for oy in range(size):
        for ox in range(size):
            rs = gs = bs = as_ = 0
            base_y = oy * f
            base_x = ox * f
            for dy in range(f):
                row = ((base_y + dy) * ss + base_x) * 4
                for dx in range(f):
                    o = row + dx * 4
                    as_ += img[o+3]
                    rs += img[o] * img[o+3]
                    gs += img[o+1] * img[o+3]
                    bs += img[o+2] * img[o+3]
            o2 = (oy * size + ox) * 4
            if as_:
                out[o2] = rs // as_; out[o2+1] = gs // as_; out[o2+2] = bs // as_
            out[o2+3] = as_ // n
    return bytes(out)


def write_ico(path: Path, sizes_png: dict) -> None:
    """BMP 方式编码的多尺寸 ICO（兼容性最好）"""
    entries = []
    blobs = []
    for size in sorted(sizes_png):
        w = h = size
        rgba = sizes_png[size]
        # BGRA 自底向上
        xor = bytearray()
        for y in range(h - 1, -1, -1):
            row = rgba[y * w * 4:(y + 1) * w * 4]
            for x in range(w):
                r, g, b, a = row[x*4], row[x*4+1], row[x*4+2], row[x*4+3]
                xor += bytes((b, g, r, a))
            # 行尾补齐到 4 字节对齐（32bpp 天然对齐）
        and_mask = bytearray(((w + 31) // 32) * 4 * h)  # 全 0 = 不透明位由 alpha 决定
        bmp_header = struct.pack(
            "<IiiHHIIiiII",
            40, w, h * 2, 1, 32, 0,
            len(xor) + len(and_mask), 0, 0, 0, 0,
        )
        blobs.append(bytes(bmp_header) + bytes(xor) + bytes(and_mask))
        entries.append((size, len(blobs[-1])))

    offset = 6 + 16 * len(entries)
    ico = struct.pack("<HHH", 0, 1, len(entries))
    for size, blen in entries:
        s = 0 if size >= 256 else size
        ico += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, blen, offset)
        offset += blen
    for b in blobs:
        ico += b
    path.write_bytes(ico)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    rendered = {}
    for size, name in [(32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png"), (512, "icon.png")]:
        print(f"render {name} ...")
        rendered[size] = render(size)
        write_png(OUT / name, size, size, rendered[size])
    print("render icon.ico ...")
    write_ico(OUT / "icon.ico", {s: rendered[s] for s in (32, 128, 256)})
    # 托盘两态 PNG：正常帧 tray.png + 全透明帧 tray_blank.png。
    # Rust 侧启动时用 tauri::image::Image::from_bytes 解码并缓存（lib.rs 的
    # tray_idle_image / tray_blank_image），闪烁 = 两帧交替。
    # （Windows 上全 0 帧的透明度由 vendor 修复后的 tray-icon DIB alpha 图标
    #   保证，见 Cargo.toml [patch.crates-io] 的注释）
    print("render tray ...")
    rgba = render(64)
    write_png(OUT / "tray.png", 64, 64, rgba)
    write_png(OUT / "tray_blank.png", 64, 64, bytes(64 * 64 * 4))
    print("done ->", OUT)


if __name__ == "__main__":
    main()
