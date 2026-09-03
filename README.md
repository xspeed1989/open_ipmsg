# Open IPMsg

基于 **Tauri v2** 的跨平台 [IP Messenger](https://ipmsg.org/)（飞鸽传书）局域网即时通讯客户端，
界面参考微信 PC 版：左侧功能栏 / 联系人列表 / 聊天窗口三栏布局，绿色气泡，自定义无边框标题栏。

- Rust 后端：完整实现 IPMsg UDP/TCP 协议（端口 2425），不依赖任何第三方协议库
- Vue 3 前端：微信 PC 风格 UI，全部矢量图标与程序化头像，零外部图片资源
- 支持 Windows / macOS / Linux

## 功能

| 模块 | 说明 |
| --- | --- |
| 用户发现 | BR_ENTRY 广播上线、ANSENTRY 应答、BR_EXIT 下线、45s 周期刷新、30min 超时清理 |
| 即时消息 | SENDMSG 文本收发、陌生来源自动注册、UDP 去重 |
| 文件传输 | 多文件附件（FILEATTACHOPT）、TCP GETFILEDATA 流式传输、断点偏移、进度事件、断点重试 |
| 剪贴板图片 | 官方「粘贴图片」双向兼容：我方发送按 FILE_CLIPBOARD+CLIPBOARDPOS+ipmsgclip_s_* 命名（官方对端消息内内嵌显示）；接收官方贴图自动内联预览 |
| 已读回执 | 发送自动携带 READCHECKOPT；对端查看后回 READMSG，气泡显示「已读/未读」 |
| 在线状态 | 联系人列表头像绿点·灰点，聊天窗口顶部在线/离线标签 |
| 系统托盘 | 关闭窗口最小化到托盘（微信式），左键唤起主窗口，菜单：显示/刷新/退出 |
| 消息通知 | 未聚焦时来消息弹系统通知（tauri-plugin-notification） |
| 聊天记录 | 每个会话一个 JSONL 文件，按会话加载最近记录；下载/已读状态回写 |
| 群组 | 上线时广播所在群组，联系人列表按群组分栏展示 |
| 不在模式 | BR_ABSENCE+ABSENCEOPT 广播「离开」、自动回复不在通知文、GETABSENCEINFO/SENDABSENCEINFO 索取与应答 |
| 撤回/封书 | DELMSG 撤回自己发出的文本消息；SECRETEXOPT 封书（对方点开查看后才回已读）；PASSWORDOPT 密码锁 |
| 广播/群发 | BROADCASTOPT 全网同报（不回执）；MULTICASTOPT 多选群发 |
| 主机列表 | BR_ISGETLIST/OKGETLIST/GETLIST/ANSLIST 主机列表交换（含官方 htons 端口小端怪癖兼容） |
| 成员主 | DIR_MASTER 目录服务：成员 POLL / 代理广播 / DIR_PACKET 全网列表分发（IPDict + RSA2048/SHA256 签名） |
| NAT 代理 | AGENT 协议中继（AGENT_REQ/ANSREQ/PACKET），配置代理地址后消息经代理转发 |
| IPv6 | ff15::979 / ff02::1 组播成员发现（无 IPv6 环境自动降级纯 IPv4） |
| 编码 | 发送可选 UTF-8 / GBK（兼容老版中文飞鸽）；接收自动识别 UTF-8/GBK |
| 多语言 | 界面支持简体中文 / English，设置页切换（选中即预览、持久化到 config.json）；语言名以各自语言显示 |
| 其他 | 表情面板、联系人未读角标、搜索、自定义接收目录、文件管理器定位 |

## 协议实现范围

```
报文格式   "1:包编号:用户名:主机名:命令字:附加数据"
命令       BR_ENTRY / ANSENTRY / BR_EXIT / BR_ABSENCE / SENDMSG / READMSG / DELMSG
           GETINFO→SENDINFO / GETABSENCEINFO→SENDABSENCEINFO / RELEASEFILES
           GETFILEDATA / GETDIRFILES / BR_ISGETLIST / OKGETLIST / GETLIST / ANSLIST
           GETPUBKEY / ANSPUBKEY / ANSREADMSG / ANSPUBKEY / AGENT_* / DIR_*（IPDict）
           （基本命令按低 8 位匹配，选项标志位于 bit8 以上）
选项       FILEATTACHOPT / READCHECKOPT / AUTORETOPT / NOADDLISTOPT / SENDCHECKOPT
           BROADCASTOPT / MULTICASTOPT / SECRETEXOPT / PASSWORDOPT / ABSENCEOPT
           CAPIPDICTOPT / DIR_MASTER / UTF8OPT / CAPUTF8OPT 等
已读流程   发送端置 READCHECKOPT → 接收端用户查看后回 READMSG(原包号) →
           发送端标记「已读」；对方离线时仅本地标记
文件项     id:name:size:mtime:attr（ID 与 attr 十六进制、size/mtime 十进制，
           解析端对两种进制均宽容兼容）
```

已实现（v4 协议加密，默认开启，设置页可关）：RSA-2048 密钥协商（GETPUBKEY/ANSPUBKEY）、
消息体加密（AES-256-CBC + SHA-256 签名，含文件公告元数据）、TCP 文件流加密
（AES-CTR，下载方发起）；对端不支持加密时自动回退明文，互通零感知。
目录递归传输（GETDIRFILES）收发双向已实现。其余协议面见功能表（不在模式、
撤回/封书/密码、广播/群发、主机列表、成员主、NAT 代理、IPv6 组播均可用）。

## 目录结构

```
├── index.html / vite.config.js / package.json
├── src/                    # Vue 3 前端
│   ├── App.vue             # 三栏布局
│   ├── store.js            # 全局状态 + 后端事件桥接
│   ├── lib/ipc.js          # invoke/listen 封装
│   └── components/         # TitleBar / SideBar / ListPanel / ChatWindow /
│                           # EmojiPicker / SettingsModal / Avatar
├── scripts/gen_icons.py    # 图标生成（纯标准库 PNG/ICO 编码器）
└── src-tauri/
    ├── tauri.conf.json     # 无边框窗口 980x660
    ├── capabilities/       # Tauri v2 权限声明
    └── src/
        ├── protocol.rs     # 报文编解码 / 文件项解析（含单元测试）
        ├── state.rs        # 配置 / 用户表 / 文件槽 / JSONL 历史
        ├── net.rs          # UDP 发现与消息循环、TCP 文件服务与下载
        ├── selftest.rs     # --selftest 无头互通自检
        └── lib.rs          # 命令注册 / 生命周期
```

## 构建与运行

依赖：Rust 1.77+、Node 18+、pnpm（或 npm）。

```bash
pnpm install

# 开发模式（热更新）
pnpm tauri dev

# 构建
pnpm tauri build          # 产物在 src-tauri/target/release/bundle/
pnpm tauri build --debug --no-bundle   # 仅编译调试二进制，快速验证
```

Linux 系统依赖（Debian/Ubuntu 示例，Arch/Manjaro 同名包）：

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

Windows 需 WebView2（Win11 自带）；macOS 需 Xcode Command Line Tools。
详见 [Tauri v2 先决条件](https://tauri.app/start/prerequisites/)。

### Arch Linux 包（.pkg.tar.zst）

Tauri v2 的打包器只支持 `deb / rpm / appimage`，没有 Arch 目标，因此单独提供脚本：

```bash
pnpm build && (cd src-tauri && cargo build --release)   # 先有 release 二进制
./scripts/build-arch.sh                                  # 产物在 dist-packages/
sudo pacman -U dist-packages/open-ipmsg-*.pkg.tar.zst
```

脚本把现成二进制、desktop 文件与三种尺寸图标装进包里（不重新编译），
以 root 运行时会自动降权到普通用户（`makepkg` 拒绝 root 执行），便于在 CI 容器里用。

想从源码构建（AUR 风格，含 `check()` 跑单测与无头自检）：

```bash
makepkg -si -p packaging/arch/PKGBUILD
```

依赖按 `ldd` 实测确定：`webkit2gtk-4.1`、`gtk3`；
`libayatana-appindicator` 是运行时 dlopen 的托盘回退，列为 optdepends
��默认走自实现的 StatusNotifierItem）。

### 自动构建（GitHub Actions）

`.github/workflows/release.yml` 在推送 `v*` 标签时自动构建并发布 Release：

| Job | 运行环境 | 产物 |
|-----|----------|------|
| `linux` | ubuntu-22.04 | `.deb` / `.rpm` / `.AppImage` |
| `arch` | `archlinux:base-devel` 容器 | `.pkg.tar.zst` |
| `release` | 汇总上面两个 job 的产物 | GitHub Release |

两个 job 都会先跑测试（前端单测 + Rust 单测 + `--selftest` 无头自检）再打包。
也可在 Actions 页面手动触发（`workflow_dispatch`）只产出构建物、不发 Release。

发布流程：

```bash
# 改 src-tauri/tauri.conf.json 与 package.json 的 version，然后
git tag v0.2.0 && git push origin v0.2.0
```

### 打包故障排查（Arch / Manjaro）

滚动发行版上 AppImage 打包有两个已知坑（deb/rpm 不受影响），已提供一键修复：

```bash
pnpm tauri build            # 先跑一次，让 tauri 下载打包工具
./scripts/fix-linuxdeploy.sh  # 应用修复
pnpm tauri build            # 重新打包
```

脚本做两件事：
1. 将 `~/.cache/tauri/linuxdeploy-x86_64.AppImage` 升级到最新 continuous 构建
   （旧版自带的 strip 不识别新工具链的 DT_RELR/`.relr.dyn` 段，报
   `unknown type [0x13]`）；
2. 给 `linuxdeploy-plugin-gtk.sh` 打补丁：兼容 gdk-pixbuf ≥ 2.44 的内置 loaders
   （目录不存在时跳过复制并确保缓存目录存在）、find 时剪枝 `/usr/lib/vmware`
   等厂商目录中的陈旧 GTK 库副本。

另外 `bundle.category` 必须使用 Tauri 预定义分类（如 `SocialNetworking`），
自定义字符串会报 `invalid category`。

## 测试与自检

```bash
cd src-tauri
cargo test                 # 协议编解码 / 状态管理单元测试（121 项）
cargo run -- --selftest    # 无头互通自检（113 项 PASS，含协议补齐全链路）
```

`--selftest` 会在本机回环地址启动完整网络栈，并内置一个"假对端"完成：
发现注册 → 双向文本 → 发送附件（对端经 TCP 取回逐字节比对）→ 接收附件
（从对端 TCP 服务下载比对）→ 已读回执双向闭环（READMSG 收发与状态落库）→
BR_EXIT 下线广播，全部通过后退出码为 0。

## 使用说明

1. 启动后首次运行弹出设置：填写**昵称**（必填）与**群组**（选填），保存即向局域网广播上线
2. 局域网内其他 IPMsg 客户端（官方 Windows 版、Mac 版、iptux 等）会出现在中栏**联系人列表**，头像右下角绿点=在线
3. 点击联系人发起会话；输入框 **Enter 发送 / Ctrl+Enter 换行**；自己发出的消息下方显示「已读/未读」
4. 工具栏 📎 选择附件发送；对方发来的附件点「下载」，完成后可「打开」或定位文件夹
5. 关闭窗口 = 最小化到托盘（微信式），托盘左键唤起窗口，托盘菜单「退出」才真正下线
6. 窗口未聚焦时收到消息会弹系统通知，对应联系人行显示未读角标，点击即查看
7. 接收目录默认为应用数据目录下的 `接收文件`，可在设置中修改

## 兼容性说明
- 与官方 IPMsg v2/v3 及主流开源实现（golang-ipmsg、iptux 等）在同一网段可直接互discover、互发消息与文件
- 官方客户端「粘贴图片」（剪贴板里复制截图后 Ctrl+V 发送）把图片作为
  IPMSG_FILE_CLIPBOARD 附件（文件名 ipmsgclip_s_*.png、attr=0x20）随 FILEATTACHOPT 公告发送，
  且必须在对端 Entry 声明 IPMSG_CLIPBOARDOPT 能力位后才会发出（senddlg.cpp 的撤回逻辑）——
  本客户端已声明该能力位并自动接收，聊天内直接预览
- 老版中文客户端默认 GBK 编码：接收方向自动识别无需设置；若对方显示乱码，将「设置 → 发送编码」切换为 GBK
- 协议加密已实现且与官方 v3/v4 规格一致：双方均支持时消息与文件传输自动加密，
  任一方不支持（或关闭）则该对端自动回退明文；设置页可查看本机密钥指纹供双方核对
- 官方 5.8.x 的 v5 新格式密文消息（EncIPDict，完整
  `IP2:<内容长度>:EF/EI/EK/EB...:Z` 字典报文）已支持：首次发送与 delayed
  重试追加的 64 字节 NUL 填充均按官方源码语义解析、解密、验签并确认送达。
- 若对端名单出现 v4/v6 双条目导致互通异常，可在设置关闭「IPv6 组播成员发现」

## 文件传输故障排查

文件传输走 **TCP 2425**（消息发现走 UDP 2425，两者需都放行）：

1. 双方防火墙放行 TCP 2425 入站。Windows 端尤其注意：首次运行若弹出防火墙提示请选择「允许」，
   或手动添加入站规则；Linux 端 `sudo systemctl status firewalld` 确认未拦截。
2. 日志默认关闭（默认运行完全静默）。排查时用 `--log` 参数启动即可打开日志：
   - 终端 stderr 输出传输日志：
     - `[tcp] <ip> 请求文件 xxx` —— 对方来取文件（我方为服务端）
     - `[download] 连接 <ip:port> 请求 pkt=.. id=..` / `接收完成: n 字节` —— 我方下载（我方为客户端）
     - `[tcp] GETFILEDATA 未命中 ...` —— 对方请求的 ID 与我方登记不符（请把此行反馈给开发者）
   - 同时向应用数据目录写 `diag.log`（入站报文摘要与失败原因，超 512KB 自动截断）。
   - 已运行中的实例无需重启：再带 `--log` 启动一次（第二个实例会被单实例互斥拦下），
     运行中的实例会当场打开日志开关。
3. 若连接超时（6 秒无响应），基本可断定对端 TCP 2425 被防火墙拦截或对方客户端 TCP 服务未开启。

## Roadmap

- [x] RSA-2048/AES 密钥协商与加密消息（GETPUBKEY/ANSPUBKEY）+ TCP 文件流加密
- [x] 文件夹递归传输（GETDIRFILES，收发双向）
- [x] 广播群发模式（BROADCASTOPT 全网同报）
- [ ] 开机自启、截图发送
