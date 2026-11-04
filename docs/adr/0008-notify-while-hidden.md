# ADR-0008：隐藏到托盘时不许吞通知

状态：已落地。

## 问题

用户报「隐藏到托盘后偶发几条消息完全不弹系统通知」，消息在聊天里能看，只是没提醒。
通知决策在前端：会话打开着且窗口聚焦就当作已读、不弹通知。而「窗口聚焦」只由 Tauri 的
focus/blur 事件维护，应用里却有多处直接 hide（关窗到托盘、Wayland 的 hide→show 重映射）。
hide 不保证产生 blur —— 于是状态停在「窗口藏着、却自称聚焦」，这个会话的消息被整条
吞掉：既不进未读也不弹通知。

另有一条同源的通道问题：主窗口没有设置 `backgroundThrottling`，Tauri 默认在窗口隐藏后
节流/挂起 WebView，msg-in 回调会被推迟到用户下次打开窗口，那时焦点事件先把它标成已读，
通知永远不会发生。这解释了「偶发、无规律、只在托盘后台」。

两条都不稳定复现，所以除了修，还要让下次出现时留下证据。

## 决定

- 窗口可见性改由后端告知：主窗口每次 hide/show 都 emit `window-visibility` `{visible}`
  （关窗到托盘、Wayland 重映射、托盘唤起、快捷键唤起全部走同一处 emit）。前端只据这个
  事实更新 `windowVisible`，判定「可以不打扰用户」要求 **打开着 + 可见 + 有焦点** 三条同时成立。
  宁可多弹一条通知，也不能把该提醒的消息吞掉。
- 隐藏时把 `windowFocused` 一并清掉：隐藏不产生 blur，两个字段各自独立更新就会漏掉
  「藏着但自称聚焦」这种组合。
- Wayland 重映射是 hide→show 两连击，**必须**以 `visible=true` 收尾 —— 停在 false 会把之后
  所有通知都吞掉，这比不做可见性同步更糟。该不变量由测试盯着。
- 主窗口配置 `"backgroundThrottling": "disabled"`，隐藏后 msg-in 仍按时处理。
  已知局限：wry 的 WebKitGTK 实现没有消费这个配置（Linux 上实际不生效），它是为
  macOS 14+/Windows 保留的；本次 Linux 上的修复靠的是后端事实 + 未被节流的 hide 路径。
  该注释不能写进 `tauri.conf.json`：本仓未启用 `config-json5`，加 `//` 会让整份配置解析失败
  （实测报 `key must be a string`），所以理由记在这里。
- 新增 `diag_log(scope, detail)` 命令，追加写 `<data_dir>/logs/<scope>.log`
  （256KB × 2 份轮转、单次 append、失败一律吞掉）。**默认开启、不受 `--log` 影响** ——
  现场不会恰好开着调试参数。前端在每次通知决策写一行：`notified` /
  `skipped:chat-visible` / `skipped:rebroadcast` / `skipped:no-msg`，带 key、包号、时间与
  正文长度；后端 `notify_message` 记 `shown id=…` / `failed err=…` / `timeout`。
  **不记标题与正文**（隐私）。`scope` 名按白名单收敛成文件名段，写不穿数据目录。
- `notify_message` 不再阻塞调用方：`show()` 是阻塞的 DBus/WinRT 调用，守护进程慢时会把
  命令线程占住、后面的通知全排队。改成 `async` + `spawn_blocking`，每次尝试 5s 上限，
  超时按失败处理并记日志；失败重试一次（这一类的失败常是瞬时的：守护进程刚拉起、会话总线抖动）。

## 验证

`scripts/notify-visibility.test.mjs` 用真 store + 真 ipc + mockIPC 覆盖：隐藏时不算可见
（不标已读、计未读、要通知）、可见且聚焦才算可见、可见但不聚焦仍要通知、hide→show 最终停在
true 且焦点只由焦点事件恢复、四种决策分支都写一行日志且正文不进日志、通知 IPC 失败重试一次
并留痕且不影响未读记账。

`cargo test --lib` 覆盖：分 scope 日志默认开启（`--log` 关着也写）、一行一条（换行被压平）、
超 256KB 轮转且只留两份、超长 detail 截断、scope 名写不穿目录；窗口可见性的两个不变量
**不用 `tauri/test` 特性**验证（该 feature 在 Windows 上会让测试二进制因 0xc0000139
STATUS_ENTRYPOINT_NOT_FOUND 起不来，见 `src-tauri/Cargo.toml` 的说明）：帧序列由纯函数
`visibility_frames` 单测钉住（was-visible → `[false, true]`，now-visible → `[true]`，
收尾恒为 `true`），发射点 `push_visibility_frames` 用注入的收集器断言「帧真的被 emit、
事件名等于 `wire::evt::WINDOW_VISIBILITY`、payload 键名等于 `wire::fields::WINDOW_VISIBILITY`」；
发送超时不阻塞调用方（假发送器睡 300ms、上限 50ms，断言耗时可测地短）；失败重试一次并把
两次 attempt 都写进 `logs/notify.log`。

没覆盖到的：真机 GUI 行为（隐藏到托盘时是否真的不产生 blur、后台节流是否真的曾推迟回调、
通知在 quickshell/GNOME 下是否真的弹出）无法在测试里验证 —— 这些只能靠装上之后翻
`<data_dir>/logs/notify.log` 与 `logs/window.log` 对账。
