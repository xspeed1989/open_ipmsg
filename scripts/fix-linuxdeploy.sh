#!/usr/bin/env bash
# 修复 Arch / Manjaro 等滚动发行版上 `pnpm tauri build` 打包 AppImage 失败的问题。
#
# 背景两个坑：
#   1) tauri 缓存的 linuxdeploy（build 10, 2024-07）自带的老 binutils strip
#      不认识新工具链产出的 DT_RELR (.relr.dyn) 段，逐库报
#      "unknown type [0x13]" → 换最新 continuous 构建即可；
#   2) linuxdeploy-plugin-gtk.sh 假设 gdk-pixbuf 的 loaders 目录存在，
#      而 gdk-pixbuf >= 2.44（Arch 打包）loaders 为内置、目录不存在；
#      且其 find 会递归扫进 /usr/lib/vmware/lib 部署 VMware 自带的老 GTK 库。
#
# 用法：先随便跑一次 pnpm tauri build（让它下载工具），再运行本脚本。
set -euo pipefail

CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/tauri"
mkdir -p "$CACHE"

echo "[1/2] 升级 linuxdeploy 到最新 continuous 构建..."
curl -fL --retry 3 -o "$CACHE/linuxdeploy-x86_64.AppImage.new" \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
chmod +x "$CACHE/linuxdeploy-x86_64.AppImage.new"
mv -f "$CACHE/linuxdeploy-x86_64.AppImage.new" "$CACHE/linuxdeploy-x86_64.AppImage"

PLUGIN="$CACHE/linuxdeploy-plugin-gtk.sh"
if [ ! -f "$PLUGIN" ]; then
  echo "错误：$PLUGIN 不存在。请先运行一次 'pnpm tauri build' 让 tauri 下载工具后再执行本脚本。"
  exit 1
fi

echo "[2/2] 为 linuxdeploy-plugin-gtk.sh 应用滚动发行版补丁..."
python3 - <<'PYEOF'
import os

p = os.path.join(os.environ.get("XDG_CACHE_HOME", os.path.expanduser("~/.cache")),
                 "tauri", "linuxdeploy-plugin-gtk.sh")
s = open(p).read()

def sub(old, new, tag):
    global s
    if tag in s:
        print(f"  - {tag}: 已应用，跳过")
        return
    if old not in s:
        print(f"  - {tag}: 未找到目标代码（上游可能已变化），跳过")
        return
    s = s.replace(old, new)
    print(f"  - {tag}: OK")

# ① gdk-pixbuf 内置 loaders：目录不存在时跳过复制
sub(
    'copy_tree "$gdk_pixbuf_binarydir" "$APPDIR/"',
    '''if [ -d "$gdk_pixbuf_binarydir" ]; then
    copy_tree "$gdk_pixbuf_binarydir" "$APPDIR/"
else
    echo "WARNING: '$gdk_pixbuf_binarydir' not found (built-in loaders?) - skipping pixbuf dir copy"
fi''',
    "builtin-loaders")

# ② 写 loaders.cache 前确保目标目录存在（目录被跳过时重定向会失败）
sub(
    '''    echo "Updating pixbuf cache in $APPDIR/$gdk_pixbuf_cache_file"
    "$gdk_pixbuf_query" > "$APPDIR/$gdk_pixbuf_cache_file"''',
    '''    echo "Updating pixbuf cache in $APPDIR/$gdk_pixbuf_cache_file"
    mkdir -p "$(dirname "$APPDIR/$gdk_pixbuf_cache_file")"
    "$gdk_pixbuf_query" > "$APPDIR/$gdk_pixbuf_cache_file"''',
    "cache-mkdir")

# ③ find 时剪枝 VMware 等第三方厂商目录里的陈旧 GTK 库副本
sub(
    'done < <(find "$directory" \\( -type l -o -type f \\) -name "$library" -print0)',
    'done < <(find "$directory" \\( -name vmware -prune \\) -o \\( -type l -o -type f \\) -name "$library" -print0)',
    "vmware-prune")

# ④ loaders.cache 可能不存在，保护 sed
sub(
    '''    echo "WARNING: loaders.cache file is missing"
fi
sed -i "s|$gdk_pixbuf_moduledir/||g" "$APPDIR/$gdk_pixbuf_cache_file"''',
    '''    echo "WARNING: loaders.cache file is missing"
elif [ -n "$gdk_pixbuf_moduledir" ] && [ -f "$APPDIR/$gdk_pixbuf_cache_file" ]; then
    sed -i "s|$gdk_pixbuf_moduledir/||g" "$APPDIR/$gdk_pixbuf_cache_file"
fi''',
    "sed-guard")

open(p, "w").write(s)
print("插件脚本更新完成")
PYEOF

bash -n "$PLUGIN"
echo "完成。重新运行 'pnpm tauri build' 即可打包 AppImage。"
