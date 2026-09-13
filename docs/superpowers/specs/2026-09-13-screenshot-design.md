# 截图发送（截屏 → 标注 → 发送）设计

**日期：** 2026-09-13
**状态：** 已实现（本机 KDE Wayland 实测；Windows/macOS 待真机验收——差异、限制与验收清单见 §15 实现记录）
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
| 画笔 | 自由折线（`lineTo`；实测平滑曲线在手感上没有差别，实现更简单），线宽可调 |
| 文字 | 点击选区内落点 → DOM 输入框 → Enter/失焦烧录到标注层，Esc 取消 |
| 马赛克 | 拖拽刷子，对**底图**像素做块状采样后回填（块边长可调 6/10/16） |
| 撤销 | 标注层 `ImageData` 快照栈，上限 20 步；工具栏「撤销」按钮 + Ctrl+Z |

颜色 6 色（红/橙/黄/绿/蓝/黑）+ 线宽 3 档；不选工具时默认「移动/调整选区」（`move`）——先调好选区再选工具标注，避免误画。

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

---

## 15. 实现记录（as-built，2026-09-13）

本节记录实现与本设计的差异、本机实测结论、交付时已知的限制，以及必须由维护者在真机上完成的验收清单。正文 §1–§14 保持设计原样，便于对照。

### 15.1 与设计的偏离

| 设计里的写法 | 实际实现 | 原因（含证据） |
| --- | --- | --- |
| §6.2 遮罩窗口 `resizable: false` | Wayland 分支**不再设** `resizable(false)`，改由 `request_fullscreen` 在窗口 map 之后（`connect_map` / `idle_add_local_once`）请求全屏；非 Wayland 的单窗路径仍保留 `resizable(false)` | Wayland 下 GDK 把「不可缩放」翻译成 xdg_toplevel 的 min = max = 当前占位尺寸，KWin 以 max_size 压过 fullscreen configure，遮罩只剩 410×290 的小窗。WAYLAND_DEBUG 显示 `set_fullscreen` 已下发且 `configure(2048,1152,[FULLSCREEN])` 已批准——即请求本身没问题，是 `resizable(false)` 把它按了回去 |
| §4 / §6.3 遮罩窗口取「本屏切片」 | Wayland 仍按屏返回切片；**非 Wayland 改为返回整幅图**，`scale` 用各屏逻辑矩形并集的宽度 | 非 Wayland 只有一个覆盖整个虚拟桌面的窗口，只给 0 号屏切片会让双屏下窗口里只画出主屏内容（缺半屏 + 错位） |
| §7.3 变暗由 `.dim` 的 `box-shadow` 挖洞实现 | 尚未选出选区时先铺一层整窗变暗层；选定后仍用 box-shadow 挖洞 | 无选区时没有任何变暗，截图模式与实时桌面在视觉上无法区分 |
| §7.3 标注层 canvas（未规定后备分辨率） | 标注层按**设备像素**建后备存储 + `ctx.setTransform(k,0,0,k,0,0)`，绘制坐标仍是 CSS 像素 | 后备存储若按 CSS 尺寸，k=1.25 时笔画会被放大 1.25 倍、比底图糊；「确认后的图 = 所见」要求同分辨率合成。代价见 §15.3 |
| §7.2 马赛克「对底图采样后回填」 | 采样与回填都吸附到设备像素（`round(css × k)`） | CSS 块网格在分数缩放下落在半像素上，相邻两块各盖一半 → 每道块缝漏出一条 25% 透光的原图细线（打码失效）。实测半透明像素 3890 → 0 |
| §7.4 工具栏几何（未规定宽度来源） | 工具栏宽度运行时实测（`el.offsetWidth`），切换工具后再重量测 | 实测 556px，马赛克工具多三个按钮时 624px；按硬编码 420 估算会把「确认/取消」推出屏幕 |
| §9.2 portal 三步流程 | `CreateSession` / `BindShortcuts` / `Activated` 监听复用**同一条长生命周期连接**；`Activated` 的第一个参数按 `OwnedObjectPath` 解析 | portal 把 `Activated` 定向发给 session 拥有者的唯一名，另开连接永远收不到；创建连接的局部变量析构会触发 `close_sessions_for_sender`，把 session 与 KDE 绑定一起拆掉 |
| §5.4 macOS 未规定 `scale` 的分母 | 分母取该显示器自己的 `CGDisplay::bounds()` 逻辑宽；权限预检改用 `core_graphics::access::ScreenCaptureAccess::preflight()` | `core-graphics 0.24` 没有 `CGImage::bounds()`；若用图像自身尺寸，比值恒为 1.0 → 画布按点建、图是像素，Retina 上 PNG 只剩左上角 1/4。手写 `extern "C" ... -> bool` 与真实返回类型 `boolean_t`（`c_uint`）ABI 不符 |
| §3 / §9 未涉及桌面安装包的 app id | 桌面文件改名为 `io.github.open-ipmsg.app.desktop`（`packaging/linux/`、`packaging/arch/PKGBUILD`、`scripts/build-arch.sh`），文件内容（`Exec=` / `Icon=` / `StartupWMClass=`）不变 | KDE 从 systemd scope（`app-<appid>-<随机>.scope`）反推 portal app id，scope 名取自已安装 `.desktop` 的 basename。旧名（`open-ipmsg` 加 `.desktop` 后缀）被 portal 的正则当成「启动器前缀 `open-` + id `ipmsg`」，找不到 `ipmsg` 的桌面文件 → 没有 app id → `org.freedesktop.portal.GlobalShortcuts.CreateSession` 返回 `NotAllowed: An app id is required` → **安装版**的 Wayland 热键降级为禁用（`tauri dev` 直接启动没有这个 scope，所以开发时反而正常） |
| §10「Wayland 下显示 portal 绑定状态」 | **设计有、未实现**：设置页只在 UA 是 Wayland 时渲染一句静态提示（`settings.shotHotkeyWayland`），没有任何查询绑定状态的 IPC；`Backend`（Portal / Disabled / 插件）只在启动时写一行日志，前端拿不到 | 要显示真实状态（已绑定 / 被拒 / 具体原因）需要新增一个读后端状态的命令。当前只能给通用指引：本轮把该文案改成「首次启动会弹系统绑定确认，改完快捷键需重启应用才生效」——因为 `shortcut::register` 全项目只有一个调用点（`lib.rs` 的 `setup`），`save_config` 只存值、没有重新绑定的命令，原文案的「首次保存时弹确认」与事实不符 |
| §11「`PORTAL_MISSING` → 设置页给安装提示」 | **设计有、未实现**：`PORTAL_MISSING` 只在**工具栏**路径以 `alert` 弹出（文案 `系统未提供截图服务（xdg-desktop-portal）：…`），设置页没有任何安装提示 | 设置页没有截图能力探测的数据来源；热键与 `--screenshot` 路径只在日志里记录（`[shot] 热键触发失败…` / `[shot] 命令行触发失败…`）。本轮把 README 与设置页文案按这个实际行为改准，没有新增探测命令 |
| §5.4 macOS 权限只「查」不「申请」 | `preflight()` 失败后再调用 `ScreenCaptureAccess::request()`（同一个 crate 对 `CGRequestScreenCaptureAccess()` 的封装）：首次运行会把应用登记进「系统设置 → 屏幕录制」，仍未授权时照旧返回 `MAC_PERMISSION`，文案改为「勾选本应用后重启应用生效」 | 只 preflight 不 request 是死循环：系统设置里根本找不到这个应用，用户无从授权，而 preflight 又永远失败。来自最终整支评审的 I2（commit `2d6f981`）；macOS 运行时仍未实机验证（§15.4） |
| §6.2 遮罩窗口 label `shot-overlay-<i>` | 改为 `shot-overlay-<会话号>-<显示器序号>`（`overlay_label()`） | 只按序号命名时，上一次截图残留的窗口会被新会话原样「复用」，而它的 URL 与 `window.__OIM_SHOT__` 绑的是旧会话 → 新图进不去、只能报「截图会话已失效」。`close_shot_overlays`、Destroyed 处理与 capabilities 的 `shot-overlay-*` 通配都按前缀工作，未受影响（commit `2d6f981`） |
| §13 风险表「抓屏期间显示『正在截屏…』轻提示」 | **设计有、未实现**：抓屏那 1~15 秒里界面没有任何提示；只有后端在第二次触发时返回「正在截屏，请稍候」，前端把它当良性忽略（不再弹窗） | 轻提示需要前端在等待期有可渲染的状态，而触发是「后端先阻塞抓屏、再开遮罩」，前端在这段时间拿不到任何事件。最终整支评审的诚实性检查点出此项 |
| §5.1「缓存超过 5 分钟即释放」 | **设计有、未实现**：`ShotState` 只有 `cache` 与 `capturing` 两个字段，没有时间戳也没有 TTL；`put` 直接替换 | 实际释放时机是「最后一个遮罩窗口销毁」（Destroyed 处理）或 `close_shot_overlays`；会话被新会话替换时旧缓存随之丢弃，所以没有 TTL 也不会读到陈旧图。最终整支评审的诚实性检查点出此项 |

另有两处属于「实现期发现并修正的缺陷」，不改变设计：Windows BitBlt 的失败路径原先在位图仍被选进内存 DC 时删除它（每次失败泄漏一张全屏 HBITMAP），且 `GetDIBits` 也在位图仍被选中时调用（MSDN 明确禁止）——现已先选回旧对象、并给两个 GDI 句柄加空值检查；Windows/macOS 的剪贴板兜底原先把 `Image.fromBytes(...)` 的 Promise 直接传给 `writeImage`（必然 reject），Linux 走后端 GTK 路径所以本机看不见，已补 `await`。

### 15.2 本机实测（KDE Plasma 6 / Wayland，双 2K 屏，缩放 1.25）

| 项目 | 结果 |
| --- | --- |
| 抓屏 | `--shot-test` / `--screenshot` 实测抓到 5120×1440 的整个工作区（两块 2560×1440 拼接），与 §2 一致；portal 非交互路径无授权弹窗。抓屏耗时沿用设计期实测（约 1s），本次未重新逐帧计时 |
| 遮罩 | 两块屏各一个遮罩窗口，GTK 几何实测 `2048×1152`（= 该屏逻辑尺寸）；两屏变暗比例均 ≈0.55（mon0 117.8→64.8、mon1 34.0→18.8），整窗变暗 + box-shadow 挖洞生效 |
| 会话 / 缓存 | 重复触发不叠第二层遮罩：抓屏在途时第二次触发被直接拒绝（§15.6 的 I1），遮罩已存在时复用该 session 并聚焦；关闭遮罩后两屏亮度比回到 1.000/1.001（无残留），再次触发是新 session |
| Wayland 热键 | KDE 首次弹绑定确认框，接受后日志 `Wayland 热键已绑定：Alt+A → ALT+a`，重启不再弹框；`dbus-monitor` 观察到 1 次 `Activated`，随后两块遮罩立即出现 |
| 命令行诊断 | §9.3 的两个入口都可用：`open-ipmsg --screenshot` 走 single-instance 回调触发同一入口；`--shot-test` 打印 `抓屏成功: <宽>x<高> → /tmp/oim-shot-test.png` |
| 工具栏 | 实测宽度 556px；切到马赛克（多三个按钮）后 624px，重新夹取后「确认/取消」仍在屏内 |
| 撤销栈 | 64MiB **字节预算上限** + **至少保留 3 步**的下限（`UNDO_BYTES`，commit `a6d25a2`）。实测：2560×1440（14.06MiB/张）保留 4 步 = 56.25MiB（预算内）；4K 等效 3840×2160（31.64MiB/张）保留 3 步 = 94.92MiB（下限压过预算，是有意为之） |
| 交互路径（Chromium 夹具） | drag → 标注 → ✓ → 待发送列表 → 剪贴板：在 Chromium 里用真实鼠标事件跑通（底图裁剪原点、标注 ±1px 对齐、✓ 只 emit 一次、无会话时只复制并提示），IPC 为桩 |
| **本机端到端（本轮验收，提交后补测）** | 用 X11 后端（`GDK_BACKEND=x11`，让 XTEST 能驱动指针）在**真实 WebKitGTK 遮罩**上跑通：真实拖拽得到绿框选区、两屏变暗生效，`Enter` 确认，独立客户端从剪贴板读回 **750×400** 的 PNG（= CSS 600×320 选区 × k 1.25）；两个遮罩按屏几何出现（0,0 与 2560,0，各 2560×1440） |
| 单元测试 | `pnpm test` 147/147；`cargo test` 226 passed / 1 ignored（共 227 项；比计划里写的 223 多 3 项：Task 12 的 `bgra_to_rgba_handles_odd_width_rows`，以及最终修复波的 `in_flight_capture_claim_refuses_second_trigger_and_releases`、`overlay_labels_are_session_scoped_but_keep_the_prefix`） |
| 修复波实机复验（详见 §15.6） | `e2f1474` 后两屏遮罩 + 变暗 0.583/0.548、杀进程后屏幕干净；`2d6f981` 后两次触发相隔 46 ms 只建一组遮罩、无「会话已失效」；`--shot-test` 对只读目录如实报错并 exit 1 |

### 15.3 交付时已知的限制

1. **混合 DPI / 混合缩放比的多屏共用一个全局比例 k**（§5.5、§13 已列为已知限制）。这是**全平台同一套逻辑**（Windows / macOS / Linux 都按整幅图的单一 k 换算），不是某个后端独有的毛病；本机两屏比例相同（都是 1.25），偏差没有暴露，而「Retina + 外接 1080p」这类混合缩放组合可能出现副屏黑边或错位。按屏精确换算要改抓屏拼接与遮罩裁剪模型，不在本次范围。
2. **标注层按设备像素合成 → 撤销快照很大**：每步是一张整窗设备像素快照（2560×1440 约 14MiB，4K 约 31.6MiB，8K 约 132MiB）。栈按 **64MiB 字节预算**封顶，同时**保底 3 步**：实测常规屏 4 步 / 56.25MiB，4K 3 步 / 94.92MiB（下限压过预算，故意的——按 64MiB 硬切在 4K 上只剩 1 步，撤销等于没有），8K 按保底会到 ≈398MiB。要压这块内存，方向是给快照做降采样/压缩，而不是再调预算常量。
3. （附）Wayland 协议不允许跨屏定位窗口，因此每屏一个遮罩、选区不能跨屏（§3 已记录，是协议限制而非实现取舍）。

### 15.4 本机无法执行 / 尚无现场证据的部分

以下项在本机无法执行，或截至本节写入时尚无现场证据（若后续在本机补齐，请直接更新本节与 §15.5）：
- **Windows / macOS 运行时行为完全未在本机执行**（本机是 Linux）。代码状态：Windows 分支 `cargo check --target x86_64-pc-windows-gnu` 通过（exit 0）；macOS 分支的**整棵依赖树** `cargo check --target aarch64-apple-darwin` 会在无关依赖 `objc2-exception-helper`（需要 macOS SDK）处失败，因此只用「把该分支原样抽到独立夹具」的方式对真实 crate 做过类型检查——macOS arm 从未在完整应用里编译过，更没有运行过。
- **Windows / macOS 剪贴板兜底**（前端 `writeImage(Image.fromBytes(...))`）只做过桩验证，未在真机粘贴过。
- **物理按键 → 热键触发**：本机无法注入全局按键（KWin 未导出 FakeInput，XTEST 对 Wayland 客户端无效）。验证只做到 KGlobalAccel 的同源事件（`setForeignShortcut` + `Component.invokeShortcut`）与 `Activated` 已到达监听器；真人在键盘上按一次 Alt+A 这一跳留给维护者。
- **原生 X11 会话**（计划里「整个虚拟桌面单窗遮罩 + 跨屏拖拽」那一项）没有现场记录：本轮的真实交互是在 Wayland 会话内用 `GDK_BACKEND=x11` 跑的（应用仍走 Wayland 的按屏遮罩分支，两个窗口），非 Wayland 的单窗路径证据仍是构建 + 分支推理 + 单元测试。
- **发送到对端那一跳**：端到端只验证到「拖拽 → 确认 → 剪贴板」（独立客户端读回 750×400）；「图片进入待发送列表 → `Enter` 发送 → 对端收到同样的像素」没有在本机跑过。
- **主窗口收进托盘后触发热键**（计划 Step 4 的第 5 项：事件不依赖主窗口可见）没有现场记录。
- **通知回归**：本轮没有现场验证；不过最终整支评审用依赖分析证明它在机制上不可能发生——`notify-rust` 的 zbus 依赖是 `default-features = false`，全仓库没有任何特性打开 `zbus/tokio`，所以我们新增的直连依赖只加了 `async-io` + `blocking-api`。剩下的只是一次「点一下确认」的现场动作。
- **安装包内的热键**：本机只验证到「旧桌面文件名被 portal 拒绝 / 换成 app id 命名后可解析」这一层诊断；改名后的文件要重新打包安装才能确认端到端绑定。
- **Tauri bundler 生成的 .deb / .rpm / AppImage 桌面文件名未核对**：本仓库 `packaging/linux/` 下的桌面文件只被 Arch 打包路径（`packaging/arch/PKGBUILD`、`scripts/build-arch.sh`）使用。

### 15.5 维护者验收清单（需真机或人工按键）

§12.4 是设计期的草稿清单，本节是交付时的正式清单（在其基础上补充了打包、物理按键、发送链路、托盘触发、通知回归与 macOS 字节序/刷写检查）。

- [ ] Windows：抓屏在 100% / 125% / 150% 缩放下像素正确；双显示器（含跨屏拖拽选区）正常。
- [ ] Windows：应用失焦时 `Alt+A` 生效；在设置页改键后重启应用生效。
- [ ] macOS：首次抓屏弹出「屏幕录制」授权；拒绝后给出提示文案，而不是一张空白/只有壁纸的图。
- [ ] macOS：多显示器拼接正确（Retina 缩放已处理；混合缩放比见 §15.3 限制 1）。
- [ ] macOS：确认 PNG 通道顺序没有 R/B 互换（`kCGImageAlphaPremultipliedLast` 的字节序），以及创建 bitmap context 后是否需要显式 `ctx.flush()`——若出现全黑或未填充的图，先查这两点。
- [ ] Windows / macOS：确认后的截图能在其他应用里粘贴。
- [ ] macOS：**透明遮罩尚未启用**（Task 15 只落到了 Linux/Windows）。`transparent()` 在 macOS 上要求 tauri 的 `macos-private-api`：本仓库已有 app 级转发特性 `macos-private-api = ["tauri/macos-private-api"]`（`src-tauri/Cargo.toml`，`src-tauri/src/screenshot.rs` 的建窗 cfg 门与它同源），另需在 `src-tauri/tauri.conf.json` 设 `macOSPrivateApi: true`（tauri-build 会校验 Cargo.toml 里 tauri 依赖的特性与配置一致）。**在打开之前，macOS 的遮罩是不透明窗，「未绘制的白窗」那一帧仍然存在**；打开后即与 Linux/Windows 一样靠透明窗消除。
- [ ] Windows / macOS：混合 DPI 多屏下遮罩与图像对齐（这是全平台共用的单一 k，不是平台特例）。
- [ ] 任意平台：确认后在待发送列表里出现正确的缩略图，`Enter` 发送后对端收到的像素与所见一致（本机目前只验证到「确认 → 剪贴板」）。
- [ ] 本机：主窗口收进托盘后触发 `Alt+A` → 选区 → 确认，图片仍进入当前会话的待发送列表（事件不依赖主窗口可见）。
- [ ] 本机人工按键：按一次物理 `Alt+A`，确认 KGlobalAccel → portal `Activated` → 触发截图这一跳（§15.4）。
- [ ] 打包安装后（Arch 包 / .deb / .rpm / AppImage）：Wayland 全局热键能绑定成功，不再出现 `NotAllowed: An app id is required`；并确认安装的桌面文件名是 `io.github.open-ipmsg.app.desktop`。
- [ ] 真实 X11 会话：整个虚拟桌面用一个遮罩窗口、可跨屏拖拽（本轮的 X11 后端验证走的仍是 Wayland 分支）。
- [ ] 通知回归：zbus 特性变更后在应用失焦时收到消息仍能弹系统通知且可点击（机制上已由依赖分析排除，这里只做一次现场确认）。

**复评留下的已接受风险（PARKED，不阻塞交付，但维护者应知悉）**

- [ ] 触发落在「认领已释放、遮罩尚未建好」的窗口期里时，第二次 `open_overlays` 可能撞上同 label 建窗失败（报「创建遮罩窗口失败: …」而不是静默忽略）。会话级 label 把这个窗口收窄了（不再复用旧会话窗口），但没有关闭；复现时重新触发即可。
- [ ] 上一次会话残留的遮罩窗口若还在，它的 `Esc` 会按 `shot-overlay-` 前缀把**当前**会话的遮罩一起销毁（`close_shot_overlays` 是前缀级销毁）。重新触发即可恢复。
- [ ] `ChatWindow.vue` 的监听器注册顺序调整后，若 `ipc.listenEvent` 抛错，拖放（drag-drop）注册也会被跳过——事件桥断掉时应用本身已降级，故未再调整顺序，仅记录。

### 15.6 最终修复波（整支评审之后，2026-09-13）

最终整支评审（`ea8e6d5..HEAD`）给出 1 Critical + 4 Important + ~14 Minor，修复波分四个提交落地：`e2f1474`（前端）、`a6d25a2`（撤销步数下限）、`2d6f981`（后端）、`3f3f152`（按钮重入守卫）。

| 发现 | 性质 | 关闭方式与证据 |
| --- | --- | --- |
| C1 缩放的来源不唯一 | Critical | `compositeB64` 用取整后的 `k = r.w / sel.w`，而 `paintAnnoSize` / `applyMosaic` 用 `k0 = slice.w / winRect.w`，导出图里标注按 `sel.x × (k − k0)` 偏移（分数缩放下实测最大 ~50 设备像素，属静默错图）。修复后 `kWin` 是唯一缩放来源：评审复现用例的偏移 +4.17/+2.67 → +0.17/+0.67，第二个用例 −9.57/−3.07 → −0.57/−0.07；复评独立重算原始探针，残差 ≤0.5 设备像素（`Math.round` 过的裁剪原点带来的取整项，不再是比例误差） |
| I1 连续触发的竞态 | Important | 抓屏那 1~15 秒里第二次触发会孤儿化会话（`capture_and_cache` 的 active_session 检查有 TOCTOU），而 `open_overlays` 会复用任意 `shot-overlay-*` 旧窗口。修复：`ShotState` 增加 `capturing` 认领（RAII guard，且**排在「已有会话就短路返回」之前**）+ 会话级 label（§15.1）+ 前端同步重入守卫（`shotBusy`、`finally` 复位、按钮置灰、把后端的「正在截屏，请稍候」当良性忽略）。实机：两次触发相隔 46 ms → 第二次被拒，遮罩只有一组（WebKitWebProcess = 3），无「会话已失效」，无残留窗口 |
| I2 macOS 权限只查不申请 | Important | 见 §15.1 的 `ScreenCaptureAccess::request()` 一行 |
| I3 撤销栈内存没有上限 | Important | 见 §15.2 / §15.3：64MiB 字节预算 + 保底 3 步 |
| I4 文字工具不是所见即所得 | Important | 输入框字号与实际烧录公式统一（20 / 24 / 32px），只留一处 7px 垂直残差并已记录 |
| ~14 Minor | Minor | 陈旧横幅清除、保存按钮 busy 守卫、标注层按选区裁剪、马赛克每次 move 只读一次底图（与旧算法逐像素等价）、拖拽的 pointercancel 恢复、监听器注册顺序、`.anno` 与 Tab 的注释、删除死键 `chat.screenshotSoon`、`--shot-test` 写盘失败如实报错并 exit 1、i18n 重启提示等 |

复评结论：所有 finding ADDRESSED，无新增 Critical/Important。复评新提的三个 Minor 的裁决是：本文档陈旧（就是本节与 §15.1–§15.3 的更新）、`listenEvent` 失败会连带跳过拖放注册（PARKED，记入 §15.5）、旧会话残留遮罩的 `Esc` 会误销毁在用的遮罩（PARKED，记入 §15.5）。评审还用依赖分析关闭了「通知回归」的机制风险（§15.4）。

详细证据在工作区的临时报告里（**未纳入 git**）：`.superpowers/sdd/2026-09-13-screenshot/final-fix-A-report.md`、`final-fix-B-report.md` 以及同目录的 `progress.md`。
