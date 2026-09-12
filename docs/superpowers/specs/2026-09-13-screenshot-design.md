# 截图发送（截屏 → 标注 → 发送）设计

**日期：** 2026-09-13
**状态：** 设计已确认，待实现计划
**范围：** 新增截图子系统（抓屏后端 / 遮罩窗口 / 标注 / 热键）。不改动 IPMsg 协议、不改动图片发送链路、不改动图片查看器。

## 1. 背景与目标

当前客户端只有「粘贴图片」链路：用户用系统截图工具截图 → 在输入区 Ctrl+V → `pendingOf(activeKey)` 出现待发送图片 → Enter 发送。README 的 Roadmap 里「截图发送」仍是未完成项。

本设计补齐微信式闭环：

```text
工具栏截图按钮 / 全局热键
  → 抓屏
  → 全屏遮罩 + 拖拽选区
  → 标注（矩形/椭圆/箭头/画笔/文字/马赛克）
  → 确认
  → 图片进入当前会话的「待发送列表」（Enter 发送）+ 可选自动复制到剪贴板
```

**复用而非新建**（这是本设计最重要的约束）：发送侧一行不改。确认后的图片走已有的 `kind:'img'` 待发送条目 + `ipc.sendClipboardImage`，与粘贴截图完全同一条路径。

## 2. 平台现状实测（2026-09-13，本机 KDE Plasma 6 / Wayland，双 2K 屏）

以下结论均为本机实测，不是文档推断；实现时不得基于相反假设：

| 项目 | 实测结果 |
| --- | --- |
| `org.freedesktop.portal.Screenshot` 版本 | `2`（支持 `interactive`，**不支持** v3 的 `target` 选区；`AvailableTargets = 0`） |
| 非交互抓屏（`interactive=false`） | **成功且无任何授权弹窗**，`Response code 0`，耗时约 1s |
| 抓到的内容 | **整个工作区、原生物理像素**：本机 5120×1440（两块 2560×1440 横向拼接） |
| 返回形式 | PNG 落盘到 `XDG_PICTURES_DIR`（本机为 `~/图片/Screenshot_<时间戳>.png`），经 `uri` 返回 |
| Wayland 窗口定位 | **不可定位**；但 `WebviewWindow::gtk_window()` + `gtk_window_fullscreen_on_monitor()` 可指定显示器全屏（xdg-shell `set_fullscreen` 带 `output`） |
| Wayland 显示器几何 | GDK 给出全局布局：`(0,0,2048×1152)`、`(2048,0,2048×1152)` |
| Wayland 缩放因子 | GDK `scale_factor` 报整数 `2`，而真实比例是 `1.25`（5120 ÷ 4096）→ **不可用 `scale_factor` 换算** |
| `org.freedesktop.portal.GlobalShortcuts` | 接口存在（`kde.portal` 实现了 impl 侧接口），可用于 Wayland 全局热键 |

复现非交互抓屏的最短路径（排查时用）：

```bash
gdbus call --session --dest org.freedesktop.portal.Desktop \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.portal.Screenshot.Screenshot "" \
  "{'interactive': <false>, 'handle_token': <'probe1'>}"
# 随后在 /org/freedesktop/portal/desktop/request/<sender>/probe1 上等 Response 信号
```

## 3. 平台能力矩阵

| 能力 | Windows | macOS | Linux / X11 | Linux / Wayland |
| --- | --- | --- | --- | --- |
| 抓屏实现 | `windows-sys` BitBlt 虚拟桌面 | `core-graphics` 全屏合成 | xdg-desktop-portal（同 Wayland） | xdg-desktop-portal |
| 抓屏授权 | 无 | 首次需「屏幕录制」权限（TCC） | 无（KDE 实测） | 无（KDE 实测） |
| 遮罩窗口布局 | 单窗口覆盖整个虚拟桌面 | 同左 | 同左 | **每显示器一个窗口**（协议不允许跨屏定位） |
| 跨屏拖拽选区 | ✅ | ✅ | ✅ | ❌ 最多在本屏内选区（协议限制，非实现取舍） |
| 全局热键 | `tauri-plugin-global-shortcut` | 同左 | 同左（仅 X11 有效） | portal `GlobalShortcuts` |
| 兜底 | — | — | — | `open-ipmsg --screenshot`（DE 自定义快捷键） |

## 4. 总体架构

```text
触发源
  ├─ 工具栏按钮（ChatWindow toolbar）        → invoke start_screenshot
  ├─ 全局热键（shortcut.rs：插件 / portal）   → 同一入口
  └─ 命令行 --screenshot（single-instance）   → 同一入口
                        │
                        ▼
        screenshot::capture()  ── 平台分支 ──→ PNG 字节 + 每屏物理矩形
                        │
                 存入后端单例缓存 ShotCache{session, png_b64, monitors}
                        │
                        ▼
        按显示器创建遮罩窗口 shot-overlay-<i>
        （查询串 ?viewer=shot&session=..&i=.. + window.__OIM_SHOT__ 注入）
                        │
     遮罩窗口 invoke shot_image(session, i) → 拿到整幅图 b64 + 本屏 slice
                        │
        canvas 渲染 → 拖拽选区 → 标注（纯函数在 src/lib/shot.js）
                        │
                     ✓ 确认
                        │
        canvas 合成 PNG → base64
                        ├─ emit "screenshot-done" → 主窗口 pendingOf(activeKey).push({kind:'img',...})
                        └─ invoke copy_image_to_clipboard（配置开启时）
                        │
        invoke close_shot_overlays(session) → 全部遮罩窗口关闭、缓存释放
```

发送侧沿用现有链路：`kind:'img'` 待发送条目 → Enter → `ipc.sendClipboardImage` → 后端 `stage_clipboard_image` 落盘 → 按附件公告。

## 5. 抓屏后端（`src-tauri/src/screenshot.rs`）

### 5.1 进程内接口与数据结构

```rust
pub struct ShotMonitor {
    pub index: usize,
    pub name: String,                 // "DP-1" / "\\\\.\\DISPLAY1" / "Built-in Retina"
    /// 本屏在整幅抓屏图中的物理像素矩形（前端只认这个坐标）
    pub px: Rect,                     // { x, y, w, h }
    /// 本屏逻辑(CSS)矩形，用于开窗与自查
    pub logical: Rect,
}

pub struct ShotCapture {
    pub session: String,              // 一次性 id（时间戳 + 计数器）
    pub width: u32,
    pub height: u32,                  // 物理像素，= 整幅 PNG 尺寸
    pub monitors: Vec<ShotMonitor>,
}
```

命令：

| 命令 | 入参 | 返回 | 说明 |
| --- | --- | --- | --- |
| `start_screenshot` | — | `ShotCapture` | 抓屏 + 建遮罩窗口；**不返回图像本体**（避免无谓的几 MB IPC） |
| `shot_image` | `session, index` | `{ b64, mime, slice, scale }` | 遮罩窗口取图；`slice` 为本屏在整幅图中的物理矩形 |
| `close_shot_overlays` | `session` | — | 关闭所有遮罩窗口并清缓存（幂等） |
| `copy_image_to_clipboard` | `b64` | — | 写系统剪贴板（Linux 走 GTK，见 §8.2） |
| `save_shot_png` | `b64, path` | — | 遮罩窗口「保存」按钮落盘 |

同一时刻只允许一个截图会话：`start_screenshot` 时若已有活跃 `session`，直接复用（返回当前 capture）并聚焦已有遮罩窗口，**不新建**。缓存超过 5 分钟或被新会话替换即释放。

### 5.2 Linux：xdg-desktop-portal

```toml
# 关键：必须 default-features = false，否则会打开 zbus 的 tokio 特性。
# Cargo 会做特性合并 —— 一旦 zbus 打开 tokio，notify-rust 的 show() 就走
# 「新建运行时再 block_on」的实现，从 Tauri 命令所在的 tokio worker 调用会 panic。
# （Cargo.toml 里已有同源注释，见 ksni / notify-rust 段落）
zbus = { version = "5", default-features = false, features = ["async-io", "blocking-api"] }
```

流程（在 `tauri::async_runtime::spawn_blocking` 里执行，不阻塞命令所在的 tokio worker）：

1. `zbus::blocking::Connection::session()`
2. 先算好 handle 路径并把 `Response` 信号订阅挂在**调用之前**（否则有竞态）
3. 调 `org.freedesktop.portal.Screenshot.Screenshot("", {"interactive": false, "handle_token": token, "modal": false})`
4. 等 `Response`（超时 15s）：`code 0` = 成功，取 `results["uri"]`
5. 读该 PNG → **删除临时文件**（不留垃圾在用户图片目录）
6. 用 `image` crate 解码拿宽高（`image 0.25` 已在依赖树里，由 tauri 的 `image-png` 特性引入）

错误映射见 §11。

### 5.3 Windows：BitBlt

- `windows-sys` 追加 features：`Win32_Graphics_Gdi`、`Win32_UI_WindowsAndMessaging`（已有依赖，不新增 crate）
- 取 `SM_XVIRTUALSCREEN/SM_YVIRTUALSCREEN/SM_CXVIRTUALSCREEN/SM_CYVIRTUALSCREEN` 得虚拟桌面矩形
- `GetWindowDC(NULL)` → 内存 DC + `CreateCompatibleBitmap` → `BitBlt(SRCCOPY | CAPTUREBLT)`（`CAPTUREBLT` 才能抓到分层窗口）
- 取 32 位 BGRA 缓冲 → **纯函数 `bgra_to_rgba` 转 RGBA** → `image` crate 编码 PNG
- 显示器矩形：`tao::monitor::Monitor::position()/size()` 在 Windows 上就是物理像素 → 直接可用

### 5.4 macOS：CoreGraphics

- `core-graphics` crate：`CGDisplayCreateImage(CGMainDisplayID())` 逐屏抓取，按 `CGDisplayBounds` 拼进整幅图（Retina 用 `CGImageGetWidth` 的原生像素）
- 抓屏前 `CGPreflightScreenCaptureAccess()`；未授权时不尝试抓屏，直接返回 `MAC_PERMISSION` 错误（macOS 未授权时会静默返回一张只有桌面的图，是最难排查的失败形态）
- 显示器矩形：tao 给的是逻辑点，乘各自 `scale_factor` 得物理像素
- 已知限制：`CGDisplayCreateImage` 在 macOS 14+ 标记 deprecated（仍可用）。迁移 ScreenCaptureKit 列入后续，不在本次范围

### 5.5 显示器几何与缩放换算

统一规则（前端只做一次除法）：

```text
整幅图宽 image_w（物理）
各屏逻辑宽之和 logical_total_w
全局比例 k = image_w / logical_total_w        ← 本机 5120 / 4096 = 1.25
某屏物理矩形 px = round(logical_rect × k)
```

**明确禁止的取值来源**：`tao::Monitor::position()/size()/scale_factor()` 在 Linux 上是
「GDK 逻辑几何 × GDK 整数 scale_factor」（见 tao `platform_impl/linux/monitor.rs`）。
本机 GDK 报 scale = 2 而真实比例是 1.25，照它换算会得到 `(0,0,4096×2304)` 这种既不是逻辑、
也不是物理的第三套坐标，遮罩必然错位。**唯一可信的是 GDK 逻辑几何 + 从图像反推的 k**。

- 后端负责把平台差异（Windows 物理像素 / macOS 点 / X11 逻辑 / Wayland 合成布局）全部归一成**上面这套物理矩形**，前端不做任何平台判断。
- 遮罩窗口内换算只信任运行时实测：`k_win = slice.w / window.innerWidth`（不信任 `devicePixelRatio`，本机 GDK 就报错成 2）。
- **已知限制**：混合 DPI 多屏（一屏 100% + 一屏 200%）下全局单一 k 会有偏差。本次按整幅图统一比例处理，偏差场景列入后续按屏换算。

## 6. 遮罩窗口

### 6.1 布局策略

| 平台 | 窗口数 | 几何 |
| --- | --- | --- |
| Windows / macOS / X11 | 1（`shot-overlay-0`） | 定位到虚拟桌面左上角，尺寸 = 各屏逻辑矩形并集 |
| Wayland | N（`shot-overlay-<i>`） | 每屏一个，`fullscreen(true)` + `fullscreen_on_monitor()` 指定该屏 |

Wayland 分支的 `fullscreen_on_monitor` 通过 `WebviewWindow::gtk_window()`（Tauri 2.11 已暴露）拿到 `gtk::ApplicationWindow`，再调 `gtk::prelude::GtkWindowExt::fullscreen_on_monitor(&gdk_monitor)`。GTK 调用必须在主线程：`app.run_on_main_thread(...)`（与现有 `clipboard_image` 的 GTK 用法一致）。

**显示器序号对齐（已查证源码，无需额外匹配）**：tao 的 Linux `MonitorHandle` 内部就是一个 `gdk::Monitor`（`MonitorHandle::new(display, number) → display.monitor(number)`），因此 Tauri `available_monitors()` 的序号与 `gdk::Display::monitors()` 序号**逐个对应** —— 后端第 i 块屏就是 `display.monitor(i)`，直接喂给 `fullscreen_on_monitor` 即可。

**显示器名不可作主键**：tao 在 Linux 上 `name()` 取的是 `monitor.model()`（本机两块屏都返回 `"Mi monitor"`），同名是常态。序号才是主键，名称只用于日志与提示。

### 6.2 窗口参数与身份

```text
label:        shot-overlay-<i>
url:          index.html?viewer=shot&session=<id>&i=<i>
注入:          window.__OIM_SHOT__ = { session, index }   // 同 ImageViewer 的注入模式
decorations:  false
always_on_top: true
skip_taskbar: true
resizable:    false
transparent:  false   // 不依赖窗口透明：变暗用 box-shadow 挖洞实现，规避 Linux 透明窗的坑
```

`src-tauri/capabilities/default.json`：`windows` 增加 `"shot-overlay-*"`；权限沿用现有集合（`dialog:default` 用于「保存」；`clipboard-manager:allow-write-image` 用于 Win/mac 剪贴板图片，见 §8.2）。

### 6.3 图像传输

`shot_image` 返回 base64 PNG（沿用 `read_image_data` 的既有模式，上限 32MB，与粘贴图片一致）。本机实测整幅 PNG 约 1.4MB → base64 约 1.9MB，2 个遮罩窗口各取一份，可接受。

裁剪**在 canvas 里做**（`drawImage` 带源矩形），理由：后端裁剪要额外一次 PNG 编解码（数百毫秒，用户能感觉到），而 canvas 裁剪是即时的。

### 6.4 多窗口协同与生命周期

- 任一遮罩窗口确认/取消 → 先 `close_shot_overlays(session)`，其余窗口随之销毁
- 已存在活跃会话时再次触发（热键/按钮）→ 忽略并聚焦已有遮罩（不叠第二层遮罩）
- 遮罩窗口被系统意外关闭（用户 Alt+F4）→ 主窗口收到 `tauri://destroyed`；后端在最后一个窗口销毁时释放缓存
- 主窗口在截图期间不做隐藏/最小化（与微信一致：截图包含主窗口）

## 7. 交互与标注

### 7.1 选区状态机

```text
idle ──按下──→ dragging ──松开(≥最小尺寸)──→ selected
  ↑                                          │
  └── Esc / 右键 / 点击空处 ─────────────────┘
selected ──拖拽内部──→ moving ──松开──→ selected
selected ──拖手柄──→ resizing ──松开──→ selected
selected ──选择工具后按下──→ annotating（选区冻结）
```

- 最小选区 3×3 物理像素；小于该值松开视为「清除选区」回到 idle
- `selected` 后方向键微调 1px，`Shift+方向键` 10px
- 快捷键：`Esc` 取消、`Enter` 确认、`Ctrl+Z` 撤销、右键取消
- 光标：十字准星（idle/dragging）、移动（选区内部）、对应方向箭头（手柄）

### 7.2 工具与行为

| 工具 | 行为 |
| --- | --- |
| 矩形 / 椭圆 | 拖拽出形状，描边色 + 线宽 |
| 箭头 | 起点拖到终点，带实心箭头头部 |
| 画笔 | 自由曲线（`quadraticCurveTo` 平滑），线宽可调 |
| 文字 | 点击选区内落点 → DOM 输入框 → Enter/失焦烧录到标注层，Esc 取消 |
| 马赛克 | 拖拽刷子，对**底图**像素做块状采样后回填（块边长可调 6/10/16） |
| 撤销 | 标注层 `ImageData` 快照栈，上限 20 步；工具栏「撤销」按钮 + Ctrl+Z |

颜色 6 色（红/橙/黄/绿/蓝/黑）+ 线宽 3 档；不选工具时默认「矩形」（微信习惯）。

### 7.3 渲染分层

```html
<div class="shot-root">
  <canvas class="base" />      <!-- 底图，只在初始与窗口尺寸变化时重绘 -->
  <div class="dim" />          <!-- 变暗：选区外 box-shadow: 0 0 0 9999px rgba(0,0,0,.45) -->
  <div class="sel" />          <!-- 选区边框 + 8 手柄 + 尺寸提示 -->
  <canvas class="anno" />      <!-- 标注层，透明，落在选区裁剪层内 -->
  <div class="toolbar" />      <!-- 贴选区下方，越界自动翻转到上方/内侧 -->
</div>
```

变暗用 `box-shadow` 挖洞而不是「每帧重绘底图」：GPU 合成、零重绘开销，拖拽 100% 跟手。

### 7.4 纯函数边界（`src/lib/shot.js`）

组件只做渲染与事件绑定，全部几何/状态数学抽成纯函数，进 `node --test`：

```text
rectFromDrag(a, b, bounds)            // 两点拖拽 → 归一化并夹取到边界
clampRect(rect, bounds)
hitTestHandle(rect, pt, tol)          // 'nw'…'se' | 'inside' | 'outside'
resizeRect(rect, handle, pt, bounds)
moveRect(rect, dx, dy, bounds)
nudgeRect(rect, key, step, bounds)
cssRectToImageRect(cssRect, slice, cssWidth)   // 核心换算，k = slice.w / cssWidth
canConfirm(rect)                      // 最小尺寸校验
mosaicBlocks(rect, block)             // 马赛克分块矩形
arrowHead(from, to, size)             // 箭头几何
pushUndo(stack, snapshot, limit)      // 撤销栈
toolbarPlacement(selRect, winRect)    // 工具栏贴合与越界翻转
```

## 8. 发送与剪贴板

### 8.1 进待发送列表

遮罩窗口确认时 `emit("screenshot-done", { b64, mime:"image/png", size, width, height })`；主窗口 `ChatWindow.vue` 监听：

```js
pendingOf(store.activeKey).push({
  kind: 'img', b64, mime, size,
  url: URL.createObjectURL(pendingImgFromB64(b64, mime, size).blob),
  name: imgItemName(mime),
})
```

与粘贴截图**完全同构**的条目，预览/发送/移除都直接复用现有代码。发送仍由 Enter 或「发送」按钮触发，不自动发出（与现有粘贴行为一致）。

### 8.2 剪贴板

| 平台 | 实现 |
| --- | --- |
| Linux | 后端 `copy_image_to_clipboard` 用 GTK `Clipboard::set_image(&Pixbuf)`（与现有 `clipboard_image` 读路径对称，三平台里唯一可靠的写法） |
| Windows / macOS | 前端 `writeImage(Image.fromBytes(...))`（`tauri-plugin-clipboard-manager` 已依赖，arboard 后端） |

配置 `shot_copy_clipboard` 默认 `true`（微信习惯：确认即已复制）。工具栏另有独立「复制」按钮 = 只复制、不进待发送。

### 8.3 边界

| 场景 | 行为 |
| --- | --- |
| 未打开任何会话 | 只复制到剪贴板 + 轻提示「请先打开一个会话」；不静默丢弃 |
| 图片 > 32MB | 拒绝并在遮罩窗口内提示（与粘贴图片上限一致） |
| 截图中途主窗口被最小化到托盘 | 允许，确认后仍进对应会话待发送列表 |
| 重复触发 | 忽略，聚焦已有遮罩（§6.4） |

## 9. 全局热键（`src-tauri/src/shortcut.rs`）

### 9.1 平台分支

```text
Windows / macOS / X11 : tauri-plugin-global-shortcut（新增依赖 + 插件注册）
Wayland               : org.freedesktop.portal.GlobalShortcuts（zbus，同 §5.2 的特性约束）
不支持 portal 的环境   : 自动降级 + 设置页说明 + --screenshot 兜底
```

判定：`XDG_SESSION_TYPE == "wayland"` 或 `WAYLAND_DISPLAY` 非空。**Wayland 下绝不注册插件**（插件在 Wayland 无效，注册只会产生误导性的"成功"）。

### 9.2 Wayland portal 流程

```text
CreateSession({ handle_token, session_handle_token }) → Response{ session_handle }
BindShortcuts(session_handle, [("screenshot", { description, preferred_trigger })], "", {})
   → 首次会弹一次系统绑定确认（KDE/GNOME 自行处理）
   → Response code 0 = 已绑定
监听 Activated(session_handle, shortcut_id, timestamp, options) → 触发截图
```

- 会话与绑定关系由后端持有；进程退出时 `session.Close()`
- portal 缺失或绑定被拒 → 记日志 + 设置页显示「当前桌面环境不支持全局热键，可用命令行方式绑定」
- 已知限制：GNOME < 48 未实现该 portal → 走降级路径

### 9.3 命令行兜底与诊断

- 复用现有 `tauri_plugin_single_instance`：`open-ipmsg --screenshot` 在 `single-instance` 回调里触发同一入口（用户可在 KDE/GNOME 系统设置里把该命令绑成自定义快捷键）
- `--shot-test` 诊断（对齐现有 `--clipboard-test` 模式）：抓一次屏、落盘到临时目录并打印尺寸/显示器清单，用于在用户机器上定位抓屏问题

### 9.4 快捷键串格式（`src/lib/hotkey.js`）

```text
内部标准形:  "Ctrl+Alt+A" / "Alt+A" / "CmdOrCtrl+Shift+S"
Tauri 插件:  直接使用标准形
portal:      toPortalTrigger("Alt+A") → "ALT+a"   // 修饰键大写，主键小写
设置页录制:  comboFromEvent(KeyboardEvent) → 标准形（拒绝纯修饰键、拒绝无修饰键的单键）
```

## 10. 配置与设置页

| 配置项 | 默认 | 说明 |
| --- | --- | --- |
| `shot_hotkey` | `"Alt+A"` | 设置页「按下组合键」录制式输入；Wayland 下显示 portal 绑定状态 |
| `shot_copy_clipboard` | `true` | 确认后是否自动复制到剪贴板 |

设置页新增「截图」分区（`SettingsModal.vue`），文案走 i18n（`zh-CN` / `en` 双份，缺一即测试失败）。

## 11. 错误处理与诊断

| 错误码 | 触发条件 | 用户文案（中文） | 行为 |
| --- | --- | --- | --- |
| `PORTAL_MISSING` | 无 `org.freedesktop.portal.Desktop` | 系统未提供截图服务（xdg-desktop-portal） | 设置页给安装提示 |
| `PORTAL_DENIED` | Response code ≠ 0 | 截图请求被系统拒绝 | 不重试 |
| `PORTAL_TIMEOUT` | 15s 无 Response | 截图超时（系统无响应） | 保留原会话可重试 |
| `CAPTURE_FAILED` | BitBlt/CG 失败 | 抓屏失败 | 记日志 |
| `MAC_PERMISSION` | 未授予屏幕录制 | 需要「屏幕录制」权限：系统设置 → 隐私与安全性 → 屏幕录制 | 不抓屏，直接提示 |
| `TOO_LARGE` | > 32MB | 截图过大，无法发送 | 保留遮罩可重选 |
| `NO_ACTIVE_CHAT` | 无会话 | 请先打开一个会话（图片已复制） | 仅复制剪贴板 |

日志统一走 `oim_log!`，前缀 `[shot]`（抓屏耗时、显示器清单、portal 返回码、窗口创建/销毁）。

## 12. 测试设计

### 12.1 Rust 单测（模块内 `#[cfg(test)]`）

- `bgra_to_rgba` / 行跨距（stride）处理
- `virtual_bounds(monitors)`、`scale_for(image_w, logical_w)`、`slice_for_monitor`
- `to_portal_trigger("Alt+A") == "ALT+a"`、`combo` 合法性校验
- portal options 构造与 Response 解析（喂假 `zbus::Message`）
- `ShotCache`：同 session 二次取图、新会话替换旧会话、过期释放、`close` 幂等
- 错误映射：portal 返回码 → `PORTAL_*` 错误码

### 12.2 Node 单测

- `scripts/shot.test.mjs`：§7.4 全部导出（几何边界、夹取、手柄命中、换算、马赛克、箭头、撤销栈、工具栏翻转）
- `scripts/hotkey.test.mjs`：规范形解析/校验/录制转换。portal 触发器串的转换不在 JS 侧（绑定发生在启动期的 Rust 里），由 `shortcut.rs` 的单测覆盖。
- 契约：`scripts/sfc-bindings.test.mjs` **需把 `ScreenshotOverlay.vue` 加进显式文件列表**（该测试是白名单式，不自动遍历目录）
- i18n：双语文案齐备

### 12.3 手工验证（本机）

1. `pnpm test` + `cargo test`（`src-tauri`）全绿
2. KDE Wayland 双屏：工具栏按钮 → 两屏均出现遮罩，内容与屏幕逐像素对齐（重点看 1.25 缩放下有无偏移）
3. 副屏拖选 → 尺寸提示正确 → 六种工具可用 → Ctrl+Z 撤销 → ✓ 确认
4. 图片出现在输入区待发送列表（缩略图正确）→ 剪贴板可直接粘贴 → Enter 发送 → 对端显示一致
5. Esc / 右键取消 → 遮罩全部关闭、无残窗、缓存释放（再触发一次仍正常）
6. 热键 Alt+A → 首次弹 portal 绑定确认 → 之后直接触发
7. `open-ipmsg --screenshot` 与 `--shot-test` 正常
8. 切到 X11 会话重复 2–7（此时应为单窗口跨屏遮罩，可跨屏拖拽）

### 12.4 真机验收（Windows / macOS，需维护者执行）

本机无法验证，交付时附 checklist：

- [ ] 抓屏内容与屏幕一致（含缩放 125%/150% 与多屏）
- [ ] 遮罩覆盖全部显示器、可跨屏拖拽
- [ ] 标注、撤销、确认、取消行为正确
- [ ] 剪贴板可粘贴、Enter 发送后对端显示一致
- [ ] 全局热键（默认 Alt+A）在应用失焦时仍生效；设置页改键后即时生效
- [ ] macOS 首次抓屏弹出「屏幕录制」授权，拒绝后给出正确提示

## 13. 兼容性与非目标

**必须保持**

- 粘贴图片链路（`onPaste` / `onPasteHotkey` / `pendingImgFromB64`）行为不变
- 图片查看器窗口、附件发送、`read_image_data` 不变
- `zbus` 特性组合不得打开 tokio（否则 Linux 通知会 panic，见 §5.2）
- 现有 `capabilities/default.json` 中 `main` 与 `image-viewer-*` 权限不变

**本次不做（YAGNI）**

- 窗口/控件级智能吸附（微信的"自动识别窗口"）
- 滚动长截图、录屏、GIF
- X11 直读 root window 的备用抓屏路径（portal 在 X11 同样可用，先只留一条 Linux 路径）
- Wayland 跨屏拖拽（协议限制，见 §3）
- macOS ScreenCaptureKit 迁移（`CGDisplayCreateImage` 虽 deprecated 仍可用）
- 混合 DPI 多屏的按屏精确换算（§5.5 已知限制）

**风险**

| 风险 | 缓解 |
| --- | --- |
| zbus 特性合并把 notify-rust 推进 panic 路径 | 显式 `default-features = false` + 单测/手工验证 Linux 通知 |
| Wayland 下 `fullscreen_on_monitor` 在某些合成器不生效 | 失败则退回 `fullscreen(true)`（落在主窗口所在屏），并在日志标记 |
| portal 抓屏约 1s 延迟，热键触发时手感偏慢 | 抓屏期间显示「正在截屏…」轻提示；不引入额外编码开销 |
| Windows 混合 DPI 下遮罩与图像错位 | 统一使用物理像素矩形；真机 checklist 覆盖 125%/150% |
| `tauri-plugin-global-shortcut` 在部分桌面失效 | 仅 Win/mac/X11 注册；Wayland 走 portal；再加命令行兜底 |

## 14. 涉及文件

**新增**

```text
src-tauri/src/screenshot.rs                    抓屏后端 + 遮罩窗口 + 缓存 + 命令
src-tauri/src/shortcut.rs                      全局热键（插件 / portal）+ 触发入口
src/components/ScreenshotOverlay.vue           遮罩窗口 UI
src/lib/shot.js                                几何与状态纯函数
src/lib/hotkey.js                              快捷键串纯函数
scripts/shot.test.mjs / scripts/hotkey.test.mjs
```

**修改**

```text
src-tauri/Cargo.toml                   zbus / tauri-plugin-global-shortcut / image / core-graphics / windows-sys features
src-tauri/src/lib.rs                   模块注册、命令注册、插件初始化、--screenshot 与 --shot-test
src-tauri/capabilities/default.json    shot-overlay-* 窗口 + 剪贴板图片权限
src/components/ChatWindow.vue          工具栏按钮 + screenshot-done 监听 + 进待发送列表
src/components/SettingsModal.vue       截图设置分区
src/main.js                            ?viewer=shot 分支
src/lib/ipc.js                         新命令封装 + EVT.screenshotDone
src/lib/i18n.js                        双语文案
scripts/sfc-bindings.test.mjs          文件列表加入 ScreenshotOverlay.vue
README.md / README.zh-CN.md            功能表与 Roadmap 勾选
```
