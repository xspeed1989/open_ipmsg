# ADR-0009：关窗到托盘的提示要「看得见、查得着、能再看一次」

状态：已落地（v0.1.19 开发分支）。

## 问题

用户报「现在关闭窗口，好像没有最小化那个提示了」。真机复现（0.1.17 二进制 + hyprctl 关窗 +
截图视觉复核）确认行为是：

- 全新进程第一次关窗 → 通知正常弹出（标题 Open IPMsg，正文「已最小化到托盘：双击托盘图标
  可打开主窗口，右键菜单可退出」）；
- 同一进程第二次关窗 → 什么都不弹。

原因不是回归，而是从第一天就写在那里的守卫：`src-tauri/src/lib.rs` 的 `on_window_event`
CloseRequested 分支里 `if !HIDE_NOTIFIED.swap(true, Relaxed) { … show() }`，
`static HIDE_NOTIFIED: AtomicBool` 的注释写「首次隐藏到托盘时提示一次」。对一个常驻后台的 IM
来说，这等价于「每天最多见一次，之后永远静默」—— 用户关窗后屏幕上什么都不剩，最容易的结论
是「程序退出了」。

第二个问题更隐蔽：这条提示走 `tauri_plugin_notification` 的
`window.notification().builder().show()`，结果被 `let _ = ` 吞掉。它既没有日志，也没有和
v0.1.18 刚建的 `notify_message` 通道（notify-rust 直发 DBus、注册 default 动作）共用。于是
用户报「没提示」时，无法区分两种完全不同的原因：守卫已经消耗过，还是发送本身失败。

## 决定

**1. 提示的三种结局各留一行 `logs/notify.log`。**
关窗提示的决策收进两个可测函数：`claim_hide_hint(&AtomicBool) -> bool`（守卫的唯一判定处，
返回「已经提示过」）与 `log_hint_send(st, make_sender)` / `log_hint_skipped(st)`
（`src-tauri/src/lib.rs`）。日志行固定为 `hint shown` / `hint failed:<err>` /
`hint skipped:already-notified`，与 ADR-0008 的 scope 日志同机制（`diag_scope("notify", …)`）。
失败不许静默：这是「没提示」时唯一的现场。

`log_hint_skipped` 刻意不接受发送器 —— 「跳过时不碰发送器」因此是类型层面的事实，不靠测试
去数调用次数：跑测试的那台机器上绝不能弹出真通知。

**2. 通道统一到 `notify_message` 那条已证明可用的路，但只统一发送与日志。**
新加 `send_tray_hint_notification` / `new_hint_sender`：Linux 用 notify-rust 直发 DBus 并注册
`default`/`open` 动作，Windows 用 tauri-winrt-notification（点击回调唤起主窗口），与
`notify_message` 共用 `send_with_retry`（5s 上限、失败重试一次、每次尝试一行日志）。
**不复用它的 key 语义**：那条路点击后要「打开某个会话」，关窗提示没有会话可跳，所以用专用
key `tray-hint`，点击只把主窗口拉起来。没有引入新依赖。

**3. 系统通知仍是「每进程一次」，但每次关窗都有不依赖通知守护进程的反馈。**
- **托盘 tooltip 分档**：`tray_tip_text(lang, total, hidden)` 是纯函数。隐藏态为
  「Open IPMsg（已最小化，双击打开）」，有未读时追加「· N 条未读」；可见态恢复
  「Open IPMsg vX[· N 条未读]」。措辞跟着**窗口可见性**走（`emit_visibility_frames` 收尾处
  按最后一帧调 `set_tray_hidden`），不挂在某个具体 show 调用点上 —— 唤起窗口有三条路
  （托盘/通知点击、全局热键、前端自己 show），漏一条就会留下「窗口开着、tooltip 却说已最小化」。
  `set_unread` 也改走同一个纯函数，否则一条新消息会把「已最小化」措辞覆盖回应用名。
- **托盘图标提示态**：`blink_tray_hint` 复用未读的帧切换接口 `set_tray_frame`（tray.png ↔
  tray_blank.png）闪 `TRAY_HINT_BLINKS = 2` 次，不另造第二套托盘绘制；`FLASHING` 为真时直接
  返回（未读闪烁已经在跑，再叠一个循环只会互相打断帧）；`TRAY_HINT_GEN` 序号让新一轮关窗
  立刻接管旧循环，收尾一定回到正常帧（停在透明帧上等于图标消失）。

**4. 「再看一次提示」入口。**
两条托盘路径（Linux 自实现 SNI 的 `impl Tray for OimTray { fn menu() }`、非 Linux 的
`TrayIconBuilder` + `MenuBuilder`）都加菜单项「显示托盘提示 / Show tray hint」（走既有
`ui_str` 本地化），共用 `show_tray_hint_again` → `tray_hint_from_menu(&HIDE_NOTIFIED, resend)`：
**先复位守卫、再重弹**（顺序反过来，这一发通知就会撞上刚置 true 的守卫，用户点了菜单却什么
都没有 —— 正是本次要修的静默），并记一行 `hint requested:from-menu`。不加设置页控件。

## 验证

`cargo test --lib` 新增 6 个用例（都用 `AppState::new(tempdir)` + 读 `logs/notify.log` 的既有
模式，发送器注入假实现，不弹真通知）：

- `hide_hint_is_claimed_only_once`：`claim_hide_hint` 第一次返回 false，之后恒 true。
- `hint_first_close_logs_shown`：首发记 `hint shown` + `shown id=17 key=tray-hint`，
  且不出现 skipped/failed。
- `hint_second_close_logs_skipped_without_sending`：只记 `hint skipped:already-notified`，
  日志里没有 `key=tray-hint`（一次发送尝试都没有）。
- `hint_failure_is_logged_not_swallowed`：两次 `Err("dbus down")` → `hint failed:dbus down`，
  两次 attempt 都留痕，不 panic。
- `tray_menu_hint_resets_guard_then_resends`：菜单点一次重弹一次、重弹那一刻守卫已复位、
  复位后下一次 `claim_hide_hint` 重新为 false。
- `tray_tip_mentions_minimized_only_when_hidden`：隐藏态含「已最小化/双击打开」且不含版本号
  默认措辞、隐藏+未读两信息都在、英文同义；`TRAY_HINT_BLINKS` 在 1..=10。

`npm test` 300 pass 不退化；`cargo build` 与 `cargo build --release` 干净。

真机自验（release 二进制 + `hyprctl dispatch 'hl.dsp.window.close()'` 两次 + `grim` 截图 +
翻 `logs/notify.log`/`logs/window.log`）：结果见下方「真机复现记录」。

没覆盖到的：tooltip 在具体 SNI 宿主（quickshell / waybar）里是否真的显示了新文案、图标闪烁
是否被用户看见 —— 这两条只能靠装上之后肉眼看，tooltip 文本本身由 `tray_tip_text` 的单测钉住。

### 真机复现记录

release 二进制（env `WAYLAND_DISPLAY=wayland-1 … GDK_BACKEND=wayland`）跑起来，工具脚本
`/tmp/verify-tray-hint.sh`：起进程 → focus → `hyprctl dispatch 'hl.dsp.window.close()'` →
`grim` 截图 → 经 SNI 菜单项 id=1 唤起 → 再关一次。tooltip 用
`busctl --user get-property $SVC /StatusNotifierItem org.kde.StatusNotifierItem ToolTip` 读。

```
关窗前 tooltip: Open IPMsg v0.1.18
关窗#1 tooltip: Open IPMsg（已最小化，双击打开）
唤起后 tooltip: Open IPMsg v0.1.18
关窗#2 tooltip: Open IPMsg（已最小化，双击打开）
== notify.log 本轮新增 ==
1793845158 shown id=211 key=tray-hint attempt=1
1793845158 hint shown
1793845166 hint skipped:already-notified
== window.log 本轮新增 ==
1793845120 visibility visible=true
1793845158 visibility visible=false
1793845164 visibility visible=true
1793845166 visibility visible=false
```

两次关窗的差异与单测完全一致；tooltip 两档措辞和「唤起即复位」都生效。

托盘菜单（读 `/MenuBar` 的 GetLayout）：id=1 显示主窗口 / id=2 刷新在线用户 /
**id=3 显示托盘提示** / id=4 separator / id=5 退出。对 id=3 发 `Event(3, "clicked")` 后
notify.log 依次出现 `hint requested:from-menu`、`shown id=20x key=tray-hint`、`hint shown`。

托盘图标提示态：轮询 `IconPixmap` 的 md5，看到 59136 字节（正常帧）与 32790 字节（透明帧）
交替两轮后回到正常帧 —— `TRAY_HINT_BLINKS = 2` 确实在跑。

通知本身真弹：`dbus-monitor` 抓到 `destination=org.freedesktop.Notifications … member=Notify`
与 `string "Open IPMsg"`；`grim` 两张截图的差异只在通知气泡区域（`compare -metric AE`
= 19435.7，整图 mean diff 0.004 —— 第一次有气泡、第二次没有），与日志互相印证。

印证不了的：弹出来的正文是中文，而本机 tesseract 只有 `eng` 语言包，OCR 只能认出标题
`Open IPMsg`；正文靠 `tray_tip_text` 之外的发送参数与 dbus 抓包确认。
