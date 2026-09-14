# Open IPMsg

> **English version → [README.md](README.md)**

Open IPMsg 是基于 **Tauri v2** 的跨平台 [IP Messenger](https://ipmsg.org/)(IPMsg/飞鸽传书)局域网即时通讯客户端,界面参考微信 PC 版:左侧功能栏 / 联系人列表 / 聊天窗口三栏布局、绿色气泡、自定义无边框标题栏。

- **Rust 后端**:完整实现 IPMsg UDP/TCP 协议(端口 2425),不依赖任何第三方协议库
- **Vue 3 前端**:微信 PC 风格 UI,全部矢量图标与程序化头像,零外部图片资源
- **跨平台**:支持 Windows / macOS / Linux

---

## 亮点

- **与官方客户端及其他开源实现互通** — 同网段下可与官方 IPMsg、iptux、golang-ipmsg 等直接互发现、互发消息与文件
- **端到端加密** — RSA-2048 密钥协商 + AES-256-CBC 消息加密 + SHA-256 签名,以及 AES-CTR 加密的 TCP 文件流;对端不支持时自动回退明文,互通零感知
- **文件与文件夹传输** — 多文件附件、TCP 流式断点续传、传输进度、目录递归传输(收发双向)
- **剪贴板贴图** — 与官方「粘贴图片」原生双向兼容:Ctrl+V 发送截图;官方客户端贴的图在聊天内自动内联预览
- **截图发送** — 工具栏按钮或全局热键(默认 Alt+A)触发,全屏遮罩上拖拽选区 + 六种标注(矩形/椭圆/箭头/画笔/文字/马赛克),确认后进待发送列表并可自动复制到剪贴板
- **已读回执** — 对方查看后显示「已读/未读」状态气泡
- **托盘与通知** — 关闭窗口最小化到托盘(微信式);窗口未聚焦时来消息弹系统通知;Windows 下来消息还会闪烁**任务栏按钮**,切到前台自动停
- **双语界面** — 简体中文 / English,运行时可随时切换

---

## 功能

| 模块 | 说明 |
| --- | --- |
| 用户发现 | 局域网自动发现(广播)、在线/离线状态、45s 周期刷新、30min 超时清理 |
| 即时消息 | 文本消息、Unicode/表情、消息去重 |
| 文件传输 | 多文件附件、TCP 流式传输、进度、断点续传、文件夹双向传输 |
| 粘贴图片 | Ctrl+V 发送截图;官方客户端粘贴的图片聊天内内联预览 |
| 截图发送 | 工具栏按钮 / 全局热键触发，拖拽选区 + 标注（矩形/椭圆/箭头/画笔/文字/马赛克），确认后进待发送列表 |
| 已读回执 | READMSG 回执,气泡显示已读/未读 |
| 群组 | 联系人列表按广播群组分栏展示 |
| 不在模式 | 「离开」状态广播、自动回复、可配置离开文案 |
| 撤回 / 封书 | 撤回自己发出的文本消息;收到的封书默认自动打开显示正文(设置里可关掉,改为点「开封」后才可见、才回已读);密码锁 |
| 广播 / 群发 | 全网同报(不回执);中栏置顶的「广播」信箱收齐收发双方的广播,点进去即发;群发给多选联系人(逐条单发,保留已读回执) |
| 成员主 (IPDict) | 目录服务,签名全网成员列表(RSA-2048/SHA-256) |
| NAT 代理 | 通过配置的代理地址中继转发 |
| IPv6 | 组播成员发现(`ff15::979` / `ff02::1`),无 IPv6 环境自动降级 IPv4 |
| 编码 | 发送可选 UTF-8 / GBK(兼容老版中文飞鸽);接收自动识别 |
| 隐私与加密 | 消息与文件端到端加密,可查看本机密钥指纹 |
| 其他 | 表情面板、未读角标、搜索、自定义接收目录、文件管理器定位 |

---

## 安装

预编译安装包在打出版本后发布到 [Releases](https://github.com/xspeed1989/open_ipmsg/releases) 页面:

- **Windows**:`.msi` / `.exe` 安装包(需 WebView2,Win11 自带)
- **macOS**:`.dmg` / `.app`(需 Xcode Command Line Tools)
- **Linux**:`.deb` / `.rpm` / `.AppImage`;Arch Linux 用户另有 `.pkg.tar.zst` 包

> 想自己从源码构建?见下文[从源码构建](#从源码构建)。

---

## 快速上手

1. **首次启动**:弹出设置,填写**昵称**(必填)与**群组**(选填),保存即向局域网广播上线
2. **联系人**:网内其他 IPMsg 客户端(官方 Windows/Mac 版、iptux 等)会出现在联系人列表,头像右下角绿点 = 在线
3. **聊天**:点击联系人发起会话;**Enter 发送 / Ctrl+Enter 换行**;自己发出的消息下方显示「已读/未读」
4. **发文件**:工具栏 📎 选择附件;对方发来的附件点「下载」,完成后可「打开」或定位文件夹
5. **托盘**:关闭窗口 = 最小化到托盘(微信式);托盘左键唤起窗口,托盘菜单「退出」才真正下线
6. **通知**:窗口未聚焦时收到消息弹系统通知,对应联系人显示未读角标,点击即查看
7. **接收目录**:默认在应用数据目录下的 `接收文件`,可在设置中修改

---

## 从源码构建

依赖:Rust 1.77+、Node 18+、pnpm(或 npm)。

```bash
pnpm install
pnpm tauri dev        # 开发模式(热更新)
pnpm tauri build      # 打包产物在 src-tauri/target/release/bundle/
```

Linux 系统依赖(Debian/Ubuntu 示例,Arch/Manjaro 同名包):

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

各平台前置条件(Windows 的 WebView2、macOS 的 Xcode CLT 等)详见 [Tauri v2 先决条件](https://tauri.app/start/prerequisites/)。

---

## 常见问题

**Q:老版中文客户端显示乱码?**
老版中文 IPMsg 默认 GBK 编码。接收方向自动识别,无需设置;若*发送*给对方显示乱码,把「设置 → 发送编码」切为 GBK 即可。

**Q:文件传输失败或超时?**
文件传输走 **TCP 2425**(发现/消息走 UDP 2425,两者都需放行)。Windows 首次运行弹出防火墙提示请选「允许」;Linux 请确认防火墙未拦截入站 TCP 2425。

**Q:联系人出现 v4/v6 双条目导致互通异常?**
可在设置里关闭「IPv6 组播成员发现」。

**Q:如何确认加密生效?**
设置页双方均显示本机密钥指纹,核对一致即可确认使用同一密钥对。

**Q:聊天记录存在哪里?**
聊天记录存放在应用数据目录,每个会话一个文件;打开会话时自动加载最近记录。

**Q:截图热键按了没反应?**
Windows/macOS/X11 由应用注册全局热键；Wayland 下改由桌面环境授权绑定：应用只在启动时注册一次，首次启动会弹一次系统确认框，之后在设置里改了快捷键要重启应用才生效。若你的桌面环境不支持，可在系统设置里把命令 `open-ipmsg --screenshot` 绑成自定义快捷键。抓屏本身在 Linux 走 xdg-desktop-portal：从工具栏点「截图」时，系统缺少该服务会直接提示「系统未提供截图服务」；热键与 `--screenshot` 这两条路径只在日志里记录失败原因。

**Q:Wayland 下热键只在安装版里不可用?**
桌面环境用「已安装桌面文件名」给应用分配 portal app id：KDE 从 systemd scope `app-<appid>-<随机>.scope` 反推，而 scope 名取自 `.desktop` 的 basename。旧包把桌面文件装成与应用同名的 `open-ipmsg`（加 `.desktop` 后缀），portal 会把 `open-` 当成启动器前缀、解析出 `ipmsg`，找不到对应桌面文件 → 没有 app id → `org.freedesktop.portal.GlobalShortcuts.CreateSession` 被拒（`NotAllowed: An app id is required`），热键降级为禁用；`tauri dev` 直接启动没有这个 scope，所以开发时反而正常。仓库已把桌面文件改名为 `io.github.open-ipmsg.app.desktop`（与 `src-tauri/tauri.conf.json` 的 `identifier` 一致），自行打包时请保持该文件名，否则仍可用 `open-ipmsg --screenshot` 兜底。

**Q:截图有哪些已知限制?**
混合 DPI 多屏（所有平台都是同一套换算逻辑，如一屏 100% + 一屏 200%）目前按整幅图使用同一个缩放比例，副屏可能错位；标注层按设备像素合成，撤销栈按 64MiB 字节预算封顶并保底 3 步（2560×1440 下 4 步 / 约 56MiB，4K 下 3 步 / 约 95MiB，8K 按保底约 398MiB）。Windows 与 macOS 的抓屏/热键代码已实现，但从未在真机上运行过：Windows 分支能通过交叉类型检查（`cargo check --target x86_64-pc-windows-gnu`），macOS 分支只在隔离夹具里类型检查过——完整应用对 `aarch64-apple-darwin` 编译会卡在既有依赖 `objc2-exception-helper`（需要 macOS SDK），详见设计文档 §15.4。另外，设置页录制的 `CmdOrCtrl` 在 Wayland 映射为 Super，在 Windows/X11 映射为 Ctrl。

---

## Roadmap

- [x] RSA-2048/AES 密钥协商与加密消息 + TCP 文件流加密
- [x] 文件夹递归传输(收发双向)
- [x] 广播群发模式(BROADCASTOPT)
- [x] 截图发送
- [ ] 开机自启

---

## 开源许可

本项目尚未选择许可证,详见仓库说明;如需分发或修改请先联系维护者。