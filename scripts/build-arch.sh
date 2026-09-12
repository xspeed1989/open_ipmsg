#!/usr/bin/env bash
# 用已编译好的 release 二进制打出 Arch Linux 包（.pkg.tar.zst）。
#
# Tauri v2 的打包器只支持 deb/rpm/appimage，没有 Arch 目标，所以单独做一个。
# 这里走「二进制打包」而不是源码构建：CI 里只编译一次，随后各格式复用同一份产物。
#
#   用法: scripts/build-arch.sh [--bin 路径] [--version 版本] [--out 输出目录]
#
# 在 root 下（容器/CI）会自动切到普通用户执行 makepkg —— makepkg 拒绝以 root 运行。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/src-tauri/target/release/open-ipmsg"
OUT="$ROOT/dist-packages"
VERSION=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

# 版本号以 tauri.conf.json 为准，保持与应用内显示一致
if [[ -z "$VERSION" ]]; then
  VERSION="$(python3 -c "import json;print(json.load(open('$ROOT/src-tauri/tauri.conf.json'))['version'])")"
fi

[[ -f "$BIN" ]] || { echo "找不到二进制: $BIN（先跑 pnpm exec tauri build --no-bundle）" >&2; exit 1; }

# 防呆：dev 模式构建不内嵌前端资源，窗口会去加载 devUrl（http://localhost:1420），
# 用户机上没有 dev server 就是白屏 —— v0.1.4 的 Arch 包正是这样坏掉的。
# 只有走 tauri CLI 才会带上 tauri/custom-protocol，内嵌资源清单里才有 /assets/。
if command -v strings >/dev/null && ! strings -a "$BIN" | grep -q "/assets/"; then
  echo "错误: $BIN 疑似 dev 模式构建（没有内嵌前端资源，运行会白屏）。" >&2
  echo "      请改用: pnpm exec tauri build --no-bundle" >&2
  exit 1
fi

# makepkg 不允许 root 执行：在容器里自动降权重跑一遍
if [[ "$(id -u)" -eq 0 ]]; then
  if ! id builder &>/dev/null; then
    useradd -m builder
    printf 'builder ALL=(ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/builder
  fi
  chown -R builder "$ROOT"
  exec sudo -u builder env HOME=/home/builder "$0" --bin "$BIN" --version "$VERSION" --out "$OUT"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

install -Dm755 "$BIN" "$WORK/open-ipmsg"
install -Dm644 "$ROOT/packaging/linux/open-ipmsg.desktop" "$WORK/open-ipmsg.desktop"
for s in 32x32 128x128; do
  install -Dm644 "$ROOT/src-tauri/icons/$s.png" "$WORK/icons/$s.png"
done
install -Dm644 "$ROOT/src-tauri/icons/icon.png" "$WORK/icons/512x512.png"

cat > "$WORK/PKGBUILD" <<PKGEOF
# 由 scripts/build-arch.sh 生成：把已编译的二进制装进 Arch 包
pkgname=open-ipmsg
pkgver=$VERSION
pkgrel=1
pkgdesc="局域网即时通讯客户端（IP Messenger 协议，UDP/TCP 2425）"
arch=('x86_64')
url="https://github.com/xspeed1989/open_ipmsg"
license=('MIT')
# webkit2gtk-4.1 与 gtk3 是 ldd 实测的直接依赖，其余由它们自行拉取
depends=('webkit2gtk-4.1' 'gtk3')
# 托盘优先走自实现的 StatusNotifierItem（DBus），
# 只有在没有 SNI 宿主时才回退到 dlopen 的 appindicator
optdepends=('libayatana-appindicator: 无 StatusNotifier 宿主时的托盘回退')
options=('!strip' '!debug')

# 文件就在 PKGBUILD 同级目录（本脚本准备好的临时构建目录），
# 没有 source 数组，因此用 startdir 而不是 srcdir
package() {
  install -Dm755 "\$startdir/open-ipmsg" "\$pkgdir/usr/bin/open-ipmsg"
  install -Dm644 "\$startdir/open-ipmsg.desktop" "\$pkgdir/usr/share/applications/open-ipmsg.desktop"
  for s in 32x32 128x128 512x512; do
    install -Dm644 "\$startdir/icons/\$s.png" \\
      "\$pkgdir/usr/share/icons/hicolor/\$s/apps/open-ipmsg.png"
  done
}
PKGEOF

cd "$WORK"
# --nodeps：这里只是把现成文件装进包里，不需要在构建机上装齐运行时依赖
makepkg -f --nodeps --noconfirm

mkdir -p "$OUT"
cp -f "$WORK"/*.pkg.tar.* "$OUT/"
echo "已生成："
ls -1 "$OUT"/*.pkg.tar.*
