# Open IPMsg

基于 **Tauri v2** 的跨平台 [IP Messenger](https://ipmsg.org/)（飞鸽传书）局域网即时通讯客户端，
界面参考微信 PC 版：左侧功能栏 / 会话列表 / 聊天窗口三栏布局，绿色气泡，自定义无边框标题栏。

- Rust 后端：完整实现 IPMsg UDP/TCP 协议（端口 2425），不依赖任何第三方协议库
- Vue 3 前端：微信 PC 风格 UI，全部矢量图标与程序化头像，零外部图片资源
- 支持 Windows / macOS / Linux

## 功能

| 模块 | 说明 |
| --- | --- |
| 用户发现 | BR_ENTRY 广播上线、ANSENTRY 应答、BR_EXIT 下线、45s 周期刷新、30min 超时清理 |
| 即时消息 | SENDMSG 文本收发、陌生来源自动注册、UDP 去重 |
| 文件传输 | 多文件附件（FILEATTACHOPT）、TCP GETFILEDATA 流式传输、断点偏移、进度事件、断点重试 |
| 已读回执 | 发送自动携带 READCHECKOPT；对端查看后回 READMSG，气泡显示「已读/未读」 |
| 在线状态 | 会话列表 / 通讯录头像绿点·灰点，聊天窗口顶部在线/离线标签 |
| 系统托盘 | 关闭窗口最小化到托盘（微信式），左键唤起主窗口，菜单：显示/刷新/退出 |
| 消息通知 | 未聚焦时来消息弹系统通知（tauri-plugin-notification） |
| 聊天记录 | 每个会话一个 JSONL 文件，按会话加载最近记录；下载/已读状态回写 |
| 群组 | 上线时广播所在群组，通讯录按群组分栏展示 |
| 编码 | 发送可选 UTF-8 / GBK（兼容老版中文飞鸽）；接收自动识别 UTF-8/GBK |
| 其他 | 表情面板、未读角标、搜索、自定义接收目录、文件管理器定位 |

## 协议实现范围

```
报文格式   "1:包编号:用户名:主机名:命令字:附加数据"
命令       BR_ENTRY / ANSENTRY / BR_EXIT / BR_ABSENCE / SENDMSG / READMSG
           GETINFO→SENDINFO / RELEASEFILES / GETFILEDATA
           （基本命令按低 8 位匹配，选项标志位于 bit8 以上）
选项       FILEATTACHOPT / READCHECKOPT / AUTORETOPT / NOADDLISTOPT 等
已读流程   发送端置 READCHECKOPT → 接收端用户查看后回 READMSG(原包号) →
           发送端标记「已读」；对方离线时仅本地标记
文件项     id:name:size:mtime:attr（ID 与 attr 十六进制、size/mtime 十进制，
           解析端对两种进制均宽容兼容）
```

暂未实现（收到时安全忽略）：加密协商（GETPUBKEY/ANSPUBKEY，对端会回退明文）、
目录递归传输（GETDIRFILES，发送目录会提示暂不支持）、保密消息。

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
cargo test                 # 协议编解码 / 状态管理单元测试（11 项）
cargo run -- --selftest    # 无头互通自检（16 项 PASS）
```

`--selftest` 会在本机回环地址启动完整网络栈，并内置一个"假对端"完成：
发现注册 → 双向文本 → 发送附件（对端经 TCP 取回逐字节比对）→ 接收附件
（从对端 TCP 服务下载比对）→ 已读回执双向闭环（READMSG 收发与状态落库）→
BR_EXIT 下线广播，全部通过后退出码为 0。

## 使用说明

1. 启动后首次运行弹出设置：填写**昵称**（必填）与**群组**（选填），保存即向局域网广播上线
2. 局域网内其他 IPMsg 客户端（官方 Windows 版、Mac 版、iptux 等）会出现在「通讯录」，头像右下角绿点=在线
3. 点击联系人发起会话；输入框 **Enter 发送 / Ctrl+Enter 换行**；自己发出的消息下方显示「已读/未读」
4. 工具栏 📎 选择附件发送；对方发来的附件点「下载」，完成后可「打开」或定位文件夹
5. 关闭窗口 = 最小化到托盘（微信式），托盘左键唤起窗口，托盘菜单「退出」才真正下线
6. 窗口未聚焦时收到消息会弹系统通知并累计未读角标
7. 接收目录默认为应用数据目录下的 `接收文件`，可在设置中修改

## 兼容性说明

- 与官方 IPMsg v2/v3 及主流开源实现（golang-ipmsg、iptux 等）在同一网段可直接互discover、互发消息与文件
- 老版中文客户端默认 GBK 编码：接收方向自动识别无需设置；若对方显示乱码，将「设置 → 发送编码」切换为 GBK
- 未启用协议加密：如需与强制加密的客户端互通，需后续实现 RSA/AES 密钥协商（见 Roadmap）

## Roadmap

- [ ] RSA-2048/AES 密钥协商与加密消息（GETPUBKEY/ANSPUBKEY）
- [ ] 文件夹递归传输（GETDIRFILES）
- [ ] 开机自启、截图发送、广播群发模式
