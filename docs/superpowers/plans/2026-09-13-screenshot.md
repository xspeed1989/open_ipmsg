# Screenshot Capture & Send Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a WeChat-style screenshot flow — trigger by toolbar button / global hotkey, capture the whole workspace, select a region on a dimmed overlay, annotate it, and drop the result into the existing pending-attachment list so Enter sends it.

**Architecture:** A new Rust module captures pixels per platform (Linux via `xdg-desktop-portal.Screenshot`, Windows via GDI `BitBlt`, macOS via CoreGraphics) and caches the PNG once per session. Tauri then opens borderless overlay windows (one spanning the virtual desktop on Windows/macOS/X11; one fullscreen per monitor on Wayland, which cannot position windows). A Vue overlay component draws the captured slice on a canvas, does selection + annotation with pure geometry helpers, composites on confirm, and emits the PNG to the main window, which pushes an ordinary `kind:'img'` pending item — reusing the existing paste-image send path unchanged. Global hotkeys use `tauri-plugin-global-shortcut` on Windows/macOS/X11 and `org.freedesktop.portal.GlobalShortcuts` on Wayland, with an `--screenshot` CLI fallback.

**Tech Stack:** Rust 2021, Tauri v2 (2.11), `zbus` 5 (async-io, blocking API), `image` 0.25 (png), `tauri-plugin-global-shortcut` 2, Vue 3, HTML canvas, Node test runner.

**Spec:** `docs/superpowers/specs/2026-09-13-screenshot-design.md`

## Global Constraints

- `zbus` must be declared `default-features = false, features = ["async-io", "blocking-api"]`. Enabling its `tokio` feature makes `notify-rust` build a runtime inside the Tauri runtime and panic (already documented in `src-tauri/Cargo.toml`).
- Never use `tao::Monitor::position()/size()/scale_factor()` on Linux as if it were logical geometry: it is `GDK logical × GDK integer scale` (measured: scale reported `2` while the true ratio is `1.25`). Linux logical geometry comes from `gdk::Display` monitors; image-pixel geometry is derived by `k = image_w / logical_union_w`.
- Wayland cannot position windows; overlays there are one fullscreen window per monitor via `gtk_window_fullscreen_on_monitor`. Cross-monitor drag selection is not supported on Wayland (protocol limit, not a shortcut).
- The existing paste-image path (`onPaste`, `onPasteHotkey`, `pendingImgFromB64`, `sendClipboardImage`, `stage_clipboard_image`) must keep working unchanged. The screenshot result reuses it verbatim.
- Do not hide or minimize the main window during capture, and do not add window transparency (`transparent: false`); dimming is done by a `box-shadow` hole.
- Physical-pixel coordinates are the only coordinates crossing the Rust→JS boundary for images; the frontend never reads `devicePixelRatio` to convert.
- TDD for every production behavior: watch the focused test fail for the intended reason before implementing it.
- Keep changes inside the existing module structure; do not reformat or restructure unrelated Rust/Vue code.
- Chinese comments in code, matching the surrounding style; bilingual i18n keys for every user-visible string.

---

## File Structure

- Create `src/lib/shot.js` — pure geometry/state helpers for selection, resize, image mapping, mosaic, arrows, undo stack, toolbar placement. No DOM, no Tauri.
- Create `src/lib/hotkey.js` — pure hotkey string parsing/formatting and portal-trigger conversion. No DOM, no Tauri.
- Create `src/components/ScreenshotOverlay.vue` — the overlay window UI (canvas, selection, annotation toolbar, actions).
- Create `src-tauri/src/screenshot.rs` — platform capture backends, monitor geometry, session cache, overlay window creation, and the screenshot commands.
- Create `src-tauri/src/shortcut.rs` — hotkey registration (plugin on Win/mac/X11, portal on Wayland) and the single trigger entry point.
- Create `scripts/shot.test.mjs`, `scripts/hotkey.test.mjs` — Node tests for the two pure JS modules.
- Modify `src-tauri/Cargo.toml` — add `zbus`, `image`, `tauri-plugin-global-shortcut`, macOS `core-graphics`, extra `windows-sys` features.
- Modify `src-tauri/src/lib.rs` — module declarations, command registration, plugin init, `--screenshot` / `--shot-test`, `file_uri_to_path` visibility.
- Modify `src-tauri/capabilities/default.json` — add `shot-overlay-*` windows and clipboard image write permission.
- Modify `src/main.js` — `?viewer=shot` entry branch.
- Modify `src/lib/ipc.js` — command wrappers and the `screenshot-done` / `screenshot-copy` event constants.
- Modify `src/components/ChatWindow.vue` — toolbar screenshot button, overlay trigger, result listener, pending-item push, clipboard copy.
- Modify `src/components/SettingsModal.vue` — screenshot settings section (hotkey recorder, copy toggle).
- Modify `src/lib/i18n.js` — bilingual strings.
- Modify `scripts/sfc-bindings.test.mjs` — add `ScreenshotOverlay.vue` to the explicit file list.
- Modify `README.md`, `README.zh-CN.md` — feature table and roadmap.

---

### Task 1: Selection geometry helpers (`shot.js` part A)

**Files:**
- Create: `src/lib/shot.js`
- Test: `scripts/shot.test.mjs`

**Interfaces:**
- Consumes: nothing.
- Produces: `rectFromDrag(a, b, bounds) -> {x,y,w,h}`, `clampRect(r, b) -> {x,y,w,h}`, `canConfirm(r, min=3) -> boolean`, `hitTestHandle(r, pt, tol=6) -> 'nw'|'ne'|'se'|'sw'|'n'|'e'|'s'|'w'|'inside'|'outside'`, `resizeRect(r, handle, pt, bounds) -> rect`, `moveRect(r, dx, dy, bounds) -> rect`, `nudgeRect(r, key, step, bounds) -> rect`. All rects are `{x, y, w, h}` in CSS pixels.

- [ ] **Step 1: Write the failing test**

Create `scripts/shot.test.mjs`:

```js
// node --test scripts/ —— 截图选区几何纯函数单测（不依赖 Tauri / 浏览器）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle,
  resizeRect, moveRect, nudgeRect,
} from '../src/lib/shot.js'

const B = { x: 0, y: 0, w: 100, h: 80 }

test('拖拽两个方向都得到归一化矩形', () => {
  assert.deepEqual(rectFromDrag({ x: 10, y: 20 }, { x: 40, y: 60 }, B), { x: 10, y: 20, w: 30, h: 40 })
  // 反向拖（从右下往左上）结果相同
  assert.deepEqual(rectFromDrag({ x: 40, y: 60 }, { x: 10, y: 20 }, B), { x: 10, y: 20, w: 30, h: 40 })
})

test('拖出边界被夹回窗口内', () => {
  assert.deepEqual(rectFromDrag({ x: -20, y: -20 }, { x: 200, y: 200 }, B), B)
  assert.deepEqual(clampRect({ x: 90, y: 70, w: 40, h: 40 }, B), { x: 90, y: 70, w: 10, h: 10 })
})

test('最小可确认尺寸是 3×3', () => {
  assert.equal(canConfirm({ x: 0, y: 0, w: 2, h: 9 }), false)
  assert.equal(canConfirm({ x: 0, y: 0, w: 3, h: 3 }), true)
  assert.equal(canConfirm(null), false)
})

test('手柄命中：角优先于边，边优先于内部', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.equal(hitTestHandle(r, { x: 11, y: 11 }), 'nw')
  assert.equal(hitTestHandle(r, { x: 59, y: 11 }), 'ne')
  assert.equal(hitTestHandle(r, { x: 59, y: 49 }), 'se')
  assert.equal(hitTestHandle(r, { x: 11, y: 49 }), 'sw')
  assert.equal(hitTestHandle(r, { x: 35, y: 10 }), 'n')
  assert.equal(hitTestHandle(r, { x: 60, y: 30 }), 'e')
  assert.equal(hitTestHandle(r, { x: 35, y: 30 }), 'inside')
  assert.equal(hitTestHandle(r, { x: 90, y: 30 }), 'outside')
})

test('小选区（3×3）内部仍能命中 inside，不被手柄吃光', () => {
  // 回归：容差若写成固定 tol=6 的半径，3×3 选区的任何内部点都会落到手柄上，
  // 于是小选区只能被拉伸、无法整体移动（Task 7 的 move 分支变成死代码）
  const small = { x: 10, y: 10, w: 3, h: 3 }
  assert.equal(hitTestHandle(small, { x: 11.5, y: 11.5 }), 'inside')
  assert.equal(hitTestHandle(small, { x: 10, y: 10 }), 'nw')
  assert.equal(hitTestHandle(small, { x: 11.5, y: 13 }), 's')
})

test('拖手柄只动被拖的边，且夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(resizeRect(r, 'se', { x: 80, y: 70 }, B), { x: 10, y: 10, w: 70, h: 60 })
  assert.deepEqual(resizeRect(r, 'nw', { x: 0, y: 0 }, B), { x: 0, y: 0, w: 60, h: 50 })
  // 拖过头：夹回边界，不允许负宽
  assert.deepEqual(resizeRect(r, 'se', { x: -50, y: -50 }, B), { x: 0, y: 0, w: 10, h: 10 })
})

test('移动与方向键微调都夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(moveRect(r, 5, -5, B), { x: 15, y: 5, w: 50, h: 40 })
  assert.deepEqual(moveRect(r, 999, 999, B), { x: 50, y: 40, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowRight', 1, B), { x: 11, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowLeft', 10, B), { x: 0, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'KeyQ', 1, B), r)
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test scripts/shot.test.mjs`
Expected: FAIL — `Cannot find module '../src/lib/shot.js'`.

- [ ] **Step 3: Write minimal implementation**

Create `src/lib/shot.js`:

```js
/**
 * 截图遮罩的几何纯函数（无 DOM、无 Tauri 依赖，node --test 直接跑）。
 * 坐标系：一律 CSS 像素；与整幅抓屏图像的换算只发生在 cssRectToImageRect。
 */

/** 拖拽两点 → 归一化矩形，并夹取到 bounds 内 */
export function rectFromDrag(a, b, bounds) {
  return clampRect(
    {
      x: Math.min(a.x, b.x),
      y: Math.min(a.y, b.y),
      w: Math.abs(a.x - b.x),
      h: Math.abs(a.y - b.y),
    },
    bounds,
  )
}

/** 把矩形夹进 bounds：先夹左上再夹右下，宽高不为负 */
export function clampRect(r, b) {
  const x1 = Math.min(Math.max(r.x, b.x), b.x + b.w)
  const y1 = Math.min(Math.max(r.y, b.y), b.y + b.h)
  const x2 = Math.min(Math.max(r.x + r.w, b.x), b.x + b.w)
  const y2 = Math.min(Math.max(r.y + r.h, b.y), b.y + b.h)
  return { x: x1, y: y1, w: Math.max(0, x2 - x1), h: Math.max(0, y2 - y1) }
}

/** 选区是否达到可确认的最小尺寸（默认 3×3） */
export function canConfirm(r, min = 3) {
  return !!r && r.w >= min && r.h >= min
}

const CORNERS = ['nw', 'ne', 'se', 'sw']

/** 命中测试：角优先于边，边优先于内部。
 *
 *  容差按轴收紧到 min(tol, 边长/3)：canConfirm 允许 3×3 的小选区，如果容差
 *  固定成 tol=6 的「半径」，那么任何 ≤2×tol（12px）的选区内部点都必然落在
 *  手柄窗口里，'inside' 永远不可达 —— 小选区就只能被拉伸、无法整体移动。
 *  收紧后 50×40 的选区行为与固定 tol 完全一致，小选区则始终留有内部命中区。 */
export function hitTestHandle(r, pt, tol = 6) {
  const tx = Math.min(tol, r.w / 3)
  const ty = Math.min(tol, r.h / 3)
  const nearL = Math.abs(pt.x - r.x) <= tx
  const nearR = Math.abs(pt.x - (r.x + r.w)) <= tx
  const nearT = Math.abs(pt.y - r.y) <= ty
  const nearB = Math.abs(pt.y - (r.y + r.h)) <= ty
  const spanX = pt.x >= r.x - tx && pt.x <= r.x + r.w + tx
  const spanY = pt.y >= r.y - ty && pt.y <= r.y + r.h + ty
  const corners = { nw: nearL && nearT, ne: nearR && nearT, se: nearR && nearB, sw: nearL && nearB }
  for (const h of CORNERS) {
    if (corners[h]) return h
  }
  if (spanY && nearL) return 'w'
  if (spanY && nearR) return 'e'
  if (spanX && nearT) return 'n'
  if (spanX && nearB) return 's'
  if (pt.x > r.x && pt.x < r.x + r.w && pt.y > r.y && pt.y < r.y + r.h) return 'inside'
  return 'outside'
}

/** 拖动某个手柄：被拖的边跟随指针，其余边不动 */
export function resizeRect(r, handle, pt, bounds) {
  let x1 = r.x
  let y1 = r.y
  let x2 = r.x + r.w
  let y2 = r.y + r.h
  if (handle.includes('w')) x1 = pt.x
  if (handle.includes('e')) x2 = pt.x
  if (handle.includes('n')) y1 = pt.y
  if (handle.includes('s')) y2 = pt.y
  return clampRect(
    { x: Math.min(x1, x2), y: Math.min(y1, y2), w: Math.abs(x2 - x1), h: Math.abs(y2 - y1) },
    bounds,
  )
}

/** 平移整个选区。
 *  这里是「整体位移 + 位置夹取」，不是 clampRect 的裁剪语义：
 *  拖到边界时选区必须保持尺寸被挡住，而不是被压扁（否则用户一拖到边就丢选区）。 */
export function moveRect(r, dx, dy, bounds) {
  const maxX = Math.max(bounds.x, bounds.x + bounds.w - r.w)
  const maxY = Math.max(bounds.y, bounds.y + bounds.h - r.h)
  const x = Math.min(Math.max(r.x + dx, bounds.x), maxX)
  const y = Math.min(Math.max(r.y + dy, bounds.y), maxY)
  return { ...r, x, y }
}

const NUDGE = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }

/** 方向键微调（1px；按住 Shift 由调用方把 step 换成 10） */
export function nudgeRect(r, key, step, bounds) {
  const d = NUDGE[key]
  return d ? moveRect(r, d[0] * step, d[1] * step, bounds) : r
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test scripts/shot.test.mjs`
Expected: PASS — all 7 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/lib/shot.js scripts/shot.test.mjs
git commit -m "feat(shot): 选区几何纯函数（拖拽/夹取/手柄/微调）"
```

---

### Task 2: Image mapping and annotation helpers (`shot.js` part B)

**Files:**
- Modify: `src/lib/shot.js` (append)
- Test: `scripts/shot.test.mjs` (append)

**Interfaces:**
- Consumes: Task 1's exports (unchanged).
- Produces: `cssRectToImageRect(css, slice, cssWidth) -> {x,y,w,h}`, `mosaicBlocks(rect, block) -> Array<{x,y,w,h}>`, `arrowHead(from, to, size=12) -> {p1:{x,y}, p2:{x,y}}`, `pushUndo(stack, snapshot, limit=20) -> Array`, `toolbarPlacement(sel, win, bar, gap=8) -> {x, y, flip}` where `flip` is `'below'|'above'|'inside'`.

- [ ] **Step 1: Write the failing test**

Append to `scripts/shot.test.mjs`:

```js
import {
  cssRectToImageRect, mosaicBlocks, arrowHead, pushUndo, toolbarPlacement,
} from '../src/lib/shot.js'

test('CSS 选区换算成整幅图里的物理像素矩形', () => {
  // 本机实测：副屏 slice 起点 x=2560（物理），窗口 CSS 宽 2048，比例 k=1.25
  const slice = { x: 2560, y: 0, w: 2560, h: 1440 }
  assert.deepEqual(
    cssRectToImageRect({ x: 100, y: 200, w: 400, h: 300 }, slice, 2048),
    { x: 2560 + 125, y: 250, w: 500, h: 375 },
  )
  // 宽高至少 1 像素，避免拖出 0 尺寸导致 toBlob 失败
  assert.deepEqual(
    cssRectToImageRect({ x: 0, y: 0, w: 0, h: 0 }, slice, 2048),
    { x: 2560, y: 0, w: 1, h: 1 },
  )
})

test('马赛克块按网格对齐且覆盖整个矩形', () => {
  const blocks = mosaicBlocks({ x: 10, y: 10, w: 20, h: 20 }, 8)
  // x: -6? 不 —— 对齐到 8 的网格：起点 8、16、24，覆盖到 30
  assert.deepEqual(blocks[0], { x: 8, y: 8, w: 8, h: 8 })
  assert.equal(blocks.length, 9) // 3×3 块覆盖 10..30
})

test('箭头两翼对称分布在线段两侧', () => {
  const { p1, p2 } = arrowHead({ x: 0, y: 0 }, { x: 100, y: 0 }, 10)
  assert.deepEqual(p1, { x: 90, y: 5 })
  assert.deepEqual(p2, { x: 90, y: -5 })
  // 零长度线段不产生 NaN
  const z = arrowHead({ x: 5, y: 5 }, { x: 5, y: 5 }, 10)
  assert.ok(Number.isFinite(z.p1.x) && Number.isFinite(z.p1.y))
})

test('撤销栈保留最近 limit 步', () => {
  let s = []
  for (let i = 0; i < 25; i++) s = pushUndo(s, `snap${i}`, 20)
  assert.equal(s.length, 20)
  assert.equal(s[0], 'snap5')
  assert.equal(s[19], 'snap24')
})

test('工具栏优先贴选区下方，越界翻到上方，再越界贴进窗口', () => {
  const win = { x: 0, y: 0, w: 800, h: 600 }
  const bar = { w: 300, h: 40 }
  assert.deepEqual(toolbarPlacement({ x: 100, y: 100, w: 200, h: 150 }, win, bar), {
    x: 100, y: 258, flip: 'below',
  })
  assert.deepEqual(toolbarPlacement({ x: 100, y: 500, w: 200, h: 90 }, win, bar), {
    x: 100, y: 452, flip: 'above',
  })
  // 选区贴满整屏：上下都放不下 → 塞进窗口内
  assert.deepEqual(toolbarPlacement({ x: 0, y: 0, w: 800, h: 600 }, win, bar), {
    x: 0, y: 552, flip: 'inside',
  })
  // 右边界对齐不外溢
  assert.equal(toolbarPlacement({ x: 700, y: 100, w: 100, h: 100 }, win, bar).x, 500)
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test scripts/shot.test.mjs`
Expected: FAIL — `cssRectToImageRect is not a function` (or an import syntax error for the missing exports).

- [ ] **Step 3: Write minimal implementation**

Append to `src/lib/shot.js`:

```js
/**
 * CSS 像素矩形 → 整幅抓屏图像里的物理像素矩形。
 * k 用「本屏 slice 宽 ÷ 窗口 CSS 宽」实时算出，不信任 devicePixelRatio：
 * 本机 KDE Wayland 的 GDK scale_factor 报 2，而真实比例是 1.25。
 */
export function cssRectToImageRect(css, slice, cssWidth) {
  const k = slice.w / cssWidth
  return {
    x: slice.x + Math.round(css.x * k),
    y: slice.y + Math.round(css.y * k),
    w: Math.max(1, Math.round(css.w * k)),
    h: Math.max(1, Math.round(css.h * k)),
  }
}

/** 马赛克块：对齐到 block 网格，返回覆盖 rect 的块矩形列表 */
export function mosaicBlocks(rect, block) {
  const out = []
  const x0 = Math.floor(rect.x / block) * block
  const y0 = Math.floor(rect.y / block) * block
  for (let y = y0; y < rect.y + rect.h; y += block) {
    for (let x = x0; x < rect.x + rect.w; x += block) {
      out.push({ x, y, w: block, h: block })
    }
  }
  return out
}

/** 箭头两翼端点（配合实心三角头部） */
export function arrowHead(from, to, size = 12) {
  const dx = to.x - from.x
  const dy = to.y - from.y
  const len = Math.hypot(dx, dy) || 1
  const ux = dx / len
  const uy = dy / len
  const bx = to.x - ux * size
  const by = to.y - uy * size
  const hx = -uy * size * 0.5
  const hy = ux * size * 0.5
  return { p1: { x: bx + hx, y: by + hy }, p2: { x: bx - hx, y: by - hy } }
}

/** 撤销栈：压入快照并裁到上限（返回新数组，保持不可变便于单测） */
export function pushUndo(stack, snapshot, limit = 20) {
  const next = (stack || []).concat([snapshot])
  return next.length > limit ? next.slice(next.length - limit) : next
}

/** 工具栏贴合：优先选区下方 → 上方 → 窗口内，横向夹在窗口内 */
export function toolbarPlacement(sel, win, bar, gap = 8) {
  let y = sel.y + sel.h + gap
  let flip = 'below'
  if (y + bar.h > win.h) {
    y = sel.y - bar.h - gap
    flip = 'above'
  }
  if (y < 0) {
    y = Math.min(Math.max(0, sel.y + sel.h - bar.h - gap), Math.max(0, win.h - bar.h))
    flip = 'inside'
  }
  const x = Math.min(Math.max(0, sel.x), Math.max(0, win.w - bar.w))
  return { x, y, flip }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test scripts/shot.test.mjs`
Expected: PASS — 12 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/lib/shot.js scripts/shot.test.mjs
git commit -m "feat(shot): 图像换算/马赛克/箭头/撤销栈/工具栏定位纯函数"
```

---

### Task 3: Hotkey string helpers (`hotkey.js`)

**Files:**
- Create: `src/lib/hotkey.js`
- Test: `scripts/hotkey.test.mjs`

**Interfaces:**
- Consumes: nothing.
- Produces: `normalizeCombo(input) -> string|null` (canonical, modifier order `CmdOrCtrl,Ctrl,Alt,Shift`), `isValidCombo(combo) -> boolean` (requires ≥1 modifier), `comboFromEvent(e) -> string|null`.

The modifier order is **CmdOrCtrl first**: it is the platform-primary modifier (⌘ on macOS, Ctrl elsewhere) and matches how both consumers render it. The Rust side parses by name in any order, so only the string form is affected.

Two things are deliberately **not** implemented here:
- The XDG shortcuts trigger format (`CTRL+ALT+a`) — the Wayland portal binding happens in Rust at startup, before any webview exists (Task 11 implements and tests `to_portal_trigger` there); a second JS copy would be dead code.
- A JS accelerator-string converter for the Tauri plugin — the plugin parses the canonical string itself in Rust (`Shortcut::from_str`, Task 11), and the settings page stores exactly what `comboFromEvent` produced. A JS converter would have no caller.

- [ ] **Step 1: Write the failing test**

Create `scripts/hotkey.test.mjs`:

```js
// node --test scripts/ —— 快捷键串纯函数单测
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  normalizeCombo, isValidCombo, comboFromEvent,
} from '../src/lib/hotkey.js'

test('归一化：大小写/别名/修饰键顺序', () => {
  assert.equal(normalizeCombo('alt+a'), 'Alt+A')
  assert.equal(normalizeCombo('shift+ctrl+s'), 'Ctrl+Shift+S')
  assert.equal(normalizeCombo('cmd+shift+a'), 'CmdOrCtrl+Shift+A')
  assert.equal(normalizeCombo('super+A'), 'CmdOrCtrl+A')
  assert.equal(normalizeCombo('  Alt + A '), 'Alt+A')
  assert.equal(normalizeCombo('enter'), 'Enter')
  assert.equal(normalizeCombo(''), null)
  assert.equal(normalizeCombo('a+b'), null) // 两个非修饰键非法
  assert.equal(normalizeCombo(null), null)
})

test('必须是「修饰键 + 主键」，纯修饰键或单键不算有效全局热键', () => {
  assert.equal(isValidCombo('Alt+A'), true)
  assert.equal(isValidCombo('A'), false)
  assert.equal(isValidCombo('Shift'), false)
  assert.equal(isValidCombo('Ctrl+Shift'), false)
  // 设置页用它标出「配置里存着一个不可用的快捷键」（手改配置 / 跨平台拷配置）
  assert.equal(isValidCombo(''), false)
  assert.equal(isValidCombo(undefined), false)
})

test('从键盘事件录制组合键', () => {
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: true, shiftKey: false, metaKey: false }), 'Alt+A')
  assert.equal(comboFromEvent({ key: 'S', ctrlKey: true, altKey: false, shiftKey: true, metaKey: false }), 'Ctrl+Shift+S')
  assert.equal(comboFromEvent({ key: 'Meta', ctrlKey: false, altKey: false, shiftKey: false, metaKey: true }), null)
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false }), null)
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test scripts/hotkey.test.mjs`
Expected: FAIL — `Cannot find module '../src/lib/hotkey.js'`.

- [ ] **Step 3: Write minimal implementation**

Create `src/lib/hotkey.js`:

```js
/**
 * 快捷键串纯函数：规范化 + 设置页录制。
 *
 * 规范形：修饰键（CmdOrCtrl/Ctrl/Alt/Shift，输出时按此顺序）+ 主键，用 '+' 连接，如 "Ctrl+Alt+A"。
 * 这个字符串直接存进配置，启动时交给 Tauri 的 global-shortcut 插件
 * （Windows/macOS/X11）或由 Rust 转成 portal 触发器（Wayland，见 shortcut.rs）；
 * 两侧都不需要 JS 再转换，所以这里只负责「录入即规范形」。
 */

/** 输出顺序：平台主修饰键在前（CmdOrCtrl → Ctrl → Alt → Shift） */
const MOD_ORDER = ['CmdOrCtrl', 'Ctrl', 'Alt', 'Shift']

const MOD_ALIASES = {
  ctrl: 'Ctrl', control: 'Ctrl',
  alt: 'Alt', option: 'Alt',
  shift: 'Shift',
  cmd: 'CmdOrCtrl', command: 'CmdOrCtrl', meta: 'CmdOrCtrl', super: 'CmdOrCtrl', win: 'CmdOrCtrl',
  cmdorctrl: 'CmdOrCtrl',
}

/** 主键别名 → 规范名 */
const KEY_ALIASES = {
  esc: 'Escape',
  escape: 'Escape',
  space: 'Space',
  spacebar: 'Space',
  enter: 'Enter',
  return: 'Enter',
  tab: 'Tab',
  backspace: 'Backspace',
  delete: 'Delete',
  del: 'Delete',
  insert: 'Insert',
  home: 'Home',
  end: 'End',
  pageup: 'PageUp',
  pagedown: 'PageDown',
  up: 'ArrowUp',
  down: 'ArrowDown',
  left: 'ArrowLeft',
  right: 'ArrowRight',
  // 规范名自身也要能解析：否则 normalizeCombo('Alt+ArrowUp') 返回 null，
  // 而 normalizeCombo('Alt+Up') 返回 'Alt+ArrowUp' —— 规范化不幂等，
  // 设置页会把一个 Rust 其实能注册的串标成「不可用」。
  arrowup: 'ArrowUp',
  arrowdown: 'ArrowDown',
  arrowleft: 'ArrowLeft',
  arrowright: 'ArrowRight',
  printscreen: 'PrintScreen',
}

/** 规范化组合键；非法返回 null */
export function normalizeCombo(input) {
  if (typeof input !== 'string') return null
  const parts = input.split('+').map((p) => p.trim()).filter(Boolean)
  if (!parts.length) return null
  const mods = new Set()
  let key = ''
  for (const p of parts) {
    const alias = MOD_ALIASES[p.toLowerCase()]
    if (alias) {
      mods.add(alias)
      continue
    }
    if (key) return null // 出现第二个非修饰键
    const named = KEY_ALIASES[p.toLowerCase()]
    if (named) key = named
    else if (/^f([1-9]|1\d|2[0-4])$/i.test(p)) key = p.toUpperCase()
    else if (/^[a-z0-9]$/i.test(p)) key = p.toUpperCase()
    // 未知键名不猜（"Foobar" / "F0" / "F99" / "Å" 一律判非法）：
    // isValidCombo 的职责就是「标出配置里存着的不可用快捷键」，
    // 放行未知键名会让设置页给一个 Rust 根本注册不了的串打绿灯。
    else return null
  }
  if (!key) return null
  return [...MOD_ORDER.filter((m) => mods.has(m)), key].join('+')
}

/** 有效全局热键 = 至少一个修饰键 + 主键（设置页用它标出配置里存着的非法值） */
export function isValidCombo(combo) {
  const n = normalizeCombo(combo)
  return !!n && n.split('+').length >= 2
}

/**
 * 从键盘事件取出规范主键名；取不到返回 null。
 *
 * 优先用 e.code（物理键位）：macOS 下按住 Option 再按字母，e.key 是合成字符
 * （Option+A → 'å'），只有 e.code（'KeyA'）才能还原出 Rust 侧可解析的组合键。
 * 没有 code 时退回 e.key，并拒绝一切非 ASCII 单字符（合成字符、'+' 等）。
 */
function keyFromEvent(e) {
  const code = typeof e?.code === 'string' ? e.code : ''
  if (/^Key[A-Z]$/.test(code)) return code.slice(3)
  if (/^Digit[0-9]$/.test(code)) return code.slice(5)
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code
  if (code === 'Space') return 'Space'

  const raw = typeof e?.key === 'string' ? e.key : ''
  if (raw === ' ') return 'Space' // 空格键的 e.key 就是 ' '
  const named = KEY_ALIASES[raw.toLowerCase()]
  if (named) return named
  if (/^[a-z0-9]$/i.test(raw)) return raw.toUpperCase()
  return null
}

/** 设置页录制：从键盘事件得到规范形；纯修饰键、无修饰键或取不到主键都返回 null */
export function comboFromEvent(e) {
  const raw = e?.key
  if (!raw) return null
  if (['Control', 'Alt', 'Shift', 'Meta', 'CapsLock', 'Dead', 'Unidentified'].includes(raw)) return null
  const key = keyFromEvent(e)
  if (!key) return null
  const mods = []
  if (e.ctrlKey) mods.push('Ctrl')
  if (e.altKey) mods.push('Alt')
  if (e.shiftKey) mods.push('Shift')
  if (e.metaKey) mods.push('CmdOrCtrl')
  if (!mods.length) return null
  return normalizeCombo([...mods, key].join('+'))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test scripts/hotkey.test.mjs`
Expected: PASS — 7 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/lib/hotkey.js scripts/hotkey.test.mjs
git commit -m "feat(shot): 快捷键串纯函数（规范形校验/录制）"
```

**Review round 1 amendments (plan-mandated defects the first review found in the block above — apply these too):**

Add to `scripts/hotkey.test.mjs`:

```js
test('空格键能录成 Space，无法表达的键被拒绝', () => {
  assert.equal(comboFromEvent({ key: ' ', code: 'Space', ctrlKey: true }), 'Ctrl+Space')
  assert.equal(normalizeCombo('ctrl+space'), 'Ctrl+Space')
  // '+' 在 '+' 分隔的规范形里无法表达，直接拒绝而不是产出坏串
  assert.equal(comboFromEvent({ key: '+', ctrlKey: true }), null)
})

test('macOS 的 Option 合成字符不会产出不可解析的组合键', () => {
  // 按住 Option 再按 A：e.key 是 'å'，e.code 仍是 'KeyA'
  assert.equal(comboFromEvent({ key: 'å', code: 'KeyA', altKey: true }), 'Alt+A')
  // 拿不到 code 时拒绝合成字符，不猜
  assert.equal(comboFromEvent({ key: 'å', altKey: true }), null)
})

test('未知键名与越界 F 键不算合法热键', () => {
  assert.equal(isValidCombo('Ctrl+Foobar'), false)
  assert.equal(isValidCombo('Ctrl+F0'), false)
  assert.equal(isValidCombo('Ctrl+F99'), false)
  assert.equal(isValidCombo('Ctrl+F24'), true)
  assert.equal(normalizeCombo('Alt+Å'), null)
})

test('规范名可往返：normalizeCombo 幂等', () => {
  assert.equal(normalizeCombo('Alt+Up'), 'Alt+ArrowUp')
  assert.equal(normalizeCombo('Alt+ArrowUp'), 'Alt+ArrowUp')
  assert.equal(isValidCombo('Alt+ArrowUp'), true)
  assert.equal(comboFromEvent({ key: 'ArrowUp', code: 'ArrowUp', altKey: true }), 'Alt+ArrowUp')
})
```

---

### Task 4: Rust geometry and pixel-conversion foundations

**Files:**
- Create: `src-tauri/src/screenshot.rs`
- Modify: `src-tauri/src/lib.rs` (declare `mod screenshot;` next to the existing module declarations)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `Rect { x: i32, y: i32, w: u32, h: u32 }` (Clone, Copy, Debug, PartialEq, Serialize), `virtual_bounds(&[Rect]) -> Rect`, `scale_for(image_w: u32, logical_w: u32) -> f64`, `slice_for_monitor(logical: Rect, bounds: Rect, k: f64) -> Rect`, `bgra_to_rgba(src: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8>`, and `fn file_uri_to_path` reuse.

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/screenshot.rs` with only the test module and the type stubs the tests need — but per TDD, write the tests first inside the new file:

```rust
//! 截图子系统：抓屏后端 / 会话缓存 / 遮罩窗口 / 命令。
//!
//! 坐标约定：`logical` 是窗口系统逻辑像素（开窗与遮罩定位用），
//! `px` 是整幅抓屏图像里的物理像素（裁剪用）。两者的换算只在这里做一次，
//! 前端拿到的永远是物理像素矩形。

use serde::Serialize;

/// 矩形（x/y 允许为负 —— 多屏时副屏可以排在主屏左侧或上方）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_bounds_covers_negative_and_positive_positions() {
        let m = [
            Rect { x: -1920, y: 0, w: 1920, h: 1080 },
            Rect { x: 0, y: 0, w: 2560, h: 1440 },
        ];
        assert_eq!(virtual_bounds(&m), Rect { x: -1920, y: 0, w: 4480, h: 1440 });
        assert_eq!(virtual_bounds(&[]), Rect { x: 0, y: 0, w: 0, h: 0 });
    }

    #[test]
    fn scale_for_uses_image_width_over_logical_width() {
        // 本机实测：两屏逻辑宽 2048+2048=4096，整幅图 5120 → 1.25
        assert_eq!(scale_for(5120, 4096), 1.25);
        assert_eq!(scale_for(1920, 1920), 1.0);
        // 逻辑宽为 0 时退化为 1，绝不产生 inf/NaN
        assert_eq!(scale_for(5120, 0), 1.0);
    }

    #[test]
    fn slice_for_monitor_offsets_by_virtual_bounds() {
        let bounds = Rect { x: -1920, y: 0, w: 4480, h: 1440 };
        let k = 1.0;
        assert_eq!(
            slice_for_monitor(Rect { x: -1920, y: 0, w: 1920, h: 1080 }, bounds, k),
            Rect { x: 0, y: 0, w: 1920, h: 1080 },
        );
        assert_eq!(
            slice_for_monitor(Rect { x: 0, y: 0, w: 2560, h: 1440 }, bounds, k),
            Rect { x: 1920, y: 0, w: 2560, h: 1440 },
        );
        // 1.25 倍：副屏 2048 逻辑宽 → 2560 物理宽，起点 0 + (2048×1.25)
        assert_eq!(
            slice_for_monitor(
                Rect { x: 2048, y: 0, w: 2048, h: 1152 },
                Rect { x: 0, y: 0, w: 4096, h: 1152 },
                1.25
            ),
            Rect { x: 2560, y: 0, w: 2560, h: 1440 },
        );
    }

    #[test]
    fn converts_bgra_rows_honouring_stride() {
        // 2×2，stride 比行宽多 4 字节填充；源是 BGRA，目标是 RGBA
        let src: Vec<u8> = vec![
            1, 2, 3, 255, 4, 5, 6, 255, 9, 9, 9, 9, // 第一行 + 填充
            7, 8, 9, 255, 10, 11, 12, 255, 9, 9, 9, 9,
        ];
        assert_eq!(
            bgra_to_rgba(&src, 2, 2, 12),
            vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255],
        );
        // 源数据不足时不 panic，缺的部分补全透明黑
        assert_eq!(bgra_to_rgba(&src[..14], 2, 2, 12).len(), 16);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test screenshot::`
Expected: FAIL — compile error: `cannot find function virtual_bounds in this scope` (and `Rect` unused warnings).

- [ ] **Step 3: Write minimal implementation**

Add above the test module in `src-tauri/src/screenshot.rs`:

```rust
/// 所有显示的并集（虚拟桌面），坐标为逻辑像素
pub fn virtual_bounds(rects: &[Rect]) -> Rect {
    if rects.is_empty() {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    }
    let x1 = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.y).min().unwrap_or(0);
    let x2 = rects.iter().map(|r| r.x + r.w as i32).max().unwrap_or(0);
    let y2 = rects.iter().map(|r| r.y + r.h as i32).max().unwrap_or(0);
    Rect { x: x1, y: y1, w: (x2 - x1).max(0) as u32, h: (y2 - y1).max(0) as u32 }
}

/// 全局比例 k = 整幅图像宽 ÷ 逻辑总宽。
///
/// 这是唯一可信的换算来源：tao 在 Linux 上给的 `scale_factor` 是 GDK 的整数
/// 缩放（本机报 2），与真实比例（本机 1.25）不符，用它换算必然错位。
pub fn scale_for(image_w: u32, logical_w: u32) -> f64 {
    if logical_w == 0 {
        return 1.0;
    }
    image_w as f64 / logical_w as f64
}

/// 某块屏在整幅图像里的物理像素矩形
pub fn slice_for_monitor(logical: Rect, bounds: Rect, k: f64) -> Rect {
    let x = ((logical.x - bounds.x) as f64 * k).round() as i32;
    let y = ((logical.y - bounds.y) as f64 * k).round() as i32;
    let w = (logical.w as f64 * k).round().max(1.0) as u32;
    let h = (logical.h as f64 * k).round().max(1.0) as u32;
    Rect { x, y, w, h }
}

/// Windows BitBlt 的 32 位 BGRA 缓冲 → RGBA（stride 可能大于行宽）
pub fn bgra_to_rgba(src: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let s = y * stride + x * 4;
            let d = (y * w as usize + x) * 4;
            if s + 3 < src.len() {
                out[d] = src[s + 2];
                out[d + 1] = src[s + 1];
                out[d + 2] = src[s];
                out[d + 3] = src[s + 3];
            }
        }
    }
    out
}
```

Add `mod screenshot;` to `src-tauri/src/lib.rs` alongside the other `mod` declarations (search for `mod state;` and put it next to it). Also change `fn file_uri_to_path` to `pub(crate) fn file_uri_to_path` so `screenshot.rs` can reuse the percent-decoding of portal `uri` results (Task 5).

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test screenshot::`
Expected: PASS — 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/screenshot.rs src-tauri/src/lib.rs
git commit -m "feat(shot): 抓屏几何与像素转换地基（虚拟桌面/比例/切片/BGRA）"
```

---

### Task 5: Linux capture via xdg-desktop-portal

**Files:**
- Modify: `src-tauri/Cargo.toml` (add `zbus`, `image`)
- Modify: `src-tauri/src/screenshot.rs` (portal client + `capture_png`)
- Modify: `src-tauri/src/lib.rs` (`--shot-test` diagnostic)

**Interfaces:**
- Consumes: Task 4's `Rect` helpers.
- Produces: `pub enum ShotErr { PortalMissing(String), PortalDenied(u32), Timeout, Decode(String), MacPermission, CaptureFailed(String) }` with `pub fn code(&self) -> &'static str` and `pub fn message(&self) -> String`; `pub fn capture_png(timeout: std::time::Duration) -> Result<Captured, ShotErr>` where `pub struct Captured { pub png: Vec<u8>, pub width: u32, pub height: u32 }`.

Verified behavior this task relies on (measured on this machine, KDE Plasma 6 / Wayland): `Screenshot(interactive=false)` returns `Response` code `0` with a `uri` to a PNG of the whole workspace in native pixels, and shows no permission dialog.

- [ ] **Step 1: Add dependencies**

In `src-tauri/Cargo.toml`, under `[dependencies]`, add:

```toml
# 截图：Linux 抓屏走 xdg-desktop-portal（Wayland 下唯一可行路径）。
# 必须 default-features = false —— 一旦打开 zbus 的 tokio 特性，Cargo 的特性合并
# 会把 notify-rust 推回「新建运行时再 block_on」的实现，从 Tauri 命令所在的
# tokio worker 调用会 panic（见本文件 ksni / notify-rust 段落的同源注释）。
zbus = { version = "5", default-features = false, features = ["async-io", "blocking-api"] }
# 抓屏结果的编解码：Linux 拿到的已是 PNG（只读尺寸），Windows/macOS 需要编码
image = { version = "0.25", default-features = false, features = ["png"] }
```

- [ ] **Step 2: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `src-tauri/src/screenshot.rs`:

```rust
    #[test]
    fn error_codes_and_messages_are_stable() {
        assert_eq!(ShotErr::Timeout.code(), "PORTAL_TIMEOUT");
        assert_eq!(ShotErr::PortalDenied(2).code(), "PORTAL_DENIED");
        assert_eq!(ShotErr::MacPermission.code(), "MAC_PERMISSION");
        assert!(ShotErr::PortalDenied(2).message().contains('2'));
        // 错误码是给前端做分支判断用的，必须是稳定的大写常量
        for e in [
            ShotErr::PortalMissing("x".into()),
            ShotErr::PortalDenied(1),
            ShotErr::Timeout,
            ShotErr::Decode("x".into()),
            ShotErr::MacPermission,
            ShotErr::CaptureFailed("x".into()),
        ] {
            assert!(e.code().chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }

    #[test]
    fn png_dimensions_are_read_without_decoding_failure() {
        // 用 image 现场编码一张 1×1 再解回来：不依赖手写 PNG 字节常量
        // （手写常量一旦 IDAT 长度写错，测试失败会指向错误的方向）
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[0u8, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode");
        let c = decode_captured(png).expect("decode");
        assert_eq!((c.width, c.height), (1, 1));
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd src-tauri && cargo test screenshot::tests::error_codes`
Expected: FAIL — `cannot find type ShotErr in this scope`.

- [ ] **Step 4: Write the implementation**

Add to `src-tauri/src/screenshot.rs`:

```rust
use std::time::Duration;

/// 抓屏失败分类：错误码给前端做分支，文案给用户看
#[derive(Debug)]
pub enum ShotErr {
    /// 系统没有可用的截图服务（未安装/未运行 xdg-desktop-portal）
    PortalMissing(String),
    /// portal 返回了非 0 响应码（用户拒绝 / 后端出错）
    PortalDenied(u32),
    /// portal 在规定时间内没有回响应
    Timeout,
    /// 图像解码失败
    Decode(String),
    /// macOS 未授予「屏幕录制」权限
    MacPermission,
    /// 平台抓屏 API 失败
    CaptureFailed(String),
}

impl ShotErr {
    pub fn code(&self) -> &'static str {
        match self {
            ShotErr::PortalMissing(_) => "PORTAL_MISSING",
            ShotErr::PortalDenied(_) => "PORTAL_DENIED",
            ShotErr::Timeout => "PORTAL_TIMEOUT",
            ShotErr::Decode(_) => "DECODE_FAILED",
            ShotErr::MacPermission => "MAC_PERMISSION",
            ShotErr::CaptureFailed(_) => "CAPTURE_FAILED",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ShotErr::PortalMissing(e) => format!("系统未提供截图服务（xdg-desktop-portal）：{e}"),
            ShotErr::PortalDenied(c) => format!("截图请求被系统拒绝（响应码 {c}）"),
            ShotErr::Timeout => "截图超时：系统未在 15 秒内响应".into(),
            ShotErr::Decode(e) => format!("截图数据解码失败：{e}"),
            ShotErr::MacPermission => {
                "需要「屏幕录制」权限：系统设置 → 隐私与安全性 → 屏幕录制".into()
            }
            ShotErr::CaptureFailed(e) => format!("抓屏失败：{e}"),
        }
    }
}

/// 一次抓屏的结果
pub struct Captured {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// PNG 字节 → 尺寸（Linux 下 portal 直接给 PNG，无需再编码）
pub fn decode_captured(png: Vec<u8>) -> Result<Captured, ShotErr> {
    let img = image::load_from_memory(&png).map_err(|e| ShotErr::Decode(e.to_string()))?;
    let (width, height) = (img.width(), img.height());
    Ok(Captured { png, width, height })
}

/// 抓取整个工作区（原生物理像素）。
///
/// 放到独立线程并带超时：portal 的 Response 信号是阻塞等待的，不能占住
/// Tauri 命令所在的 tokio worker，也不能无限期挂起。
pub fn capture_png(timeout: Duration) -> Result<Captured, ShotErr> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(capture_png_inner());
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(ShotErr::Timeout),
    }
}

#[cfg(target_os = "linux")]
fn capture_png_inner() -> Result<Captured, ShotErr> {
    let raw = portal::screenshot_png()?;
    decode_captured(raw)
}

#[cfg(not(target_os = "linux"))]
fn capture_png_inner() -> Result<Captured, ShotErr> {
    platform::capture()
}

/// xdg-desktop-portal 客户端（Linux：X11 与 Wayland 同一条路径）
#[cfg(target_os = "linux")]
mod portal {
    use super::ShotErr;
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";

    /// 非交互抓屏 → PNG 字节
    pub fn screenshot_png() -> Result<Vec<u8>, ShotErr> {
        let conn = Connection::session()
            .map_err(|e| ShotErr::PortalMissing(format!("无法连接会话总线: {e}")))?;
        // handle 路径可预测：/org/freedesktop/portal/desktop/request/<sender>/<token>
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .ok_or_else(|| ShotErr::PortalMissing("会话总线没有唯一名".into()))?;
        let token = format!("oimshot{}", std::process::id());
        let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

        // 必须先订阅再调用：portal 的响应可能早于调用返回
        let req = Proxy::new(&conn, DEST, handle.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        let mut signals = req
            .receive_signal("Response")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;

        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("interactive", Value::from(false));
        options.insert("modal", Value::from(false));

        let shot = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.Screenshot")
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        let returned: OwnedObjectPath = shot
            .call("Screenshot", &("", options))
            .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        if returned.as_str() != handle {
            // portal 用了别的 handle（罕见）：改挂到实际路径上再等
            let req2 = Proxy::new(&conn, DEST, returned.as_str(), "org.freedesktop.portal.Request")
                .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
            signals = req2
                .receive_signal("Response")
                .map_err(|e| ShotErr::PortalMissing(e.to_string()))?;
        }

        let msg = signals
            .next()
            .ok_or_else(|| ShotErr::PortalMissing("portal 未返回响应".into()))?;
        let (code, results): (u32, HashMap<String, OwnedValue>) = msg
            .body()
            .deserialize()
            .map_err(|e| ShotErr::Decode(e.to_string()))?;
        if code != 0 {
            return Err(ShotErr::PortalDenied(code));
        }
        let uri: String = results
            .get("uri")
            .ok_or_else(|| ShotErr::Decode("响应里没有 uri".into()))?
            .try_into()
            .map_err(|_| ShotErr::Decode("uri 不是字符串".into()))?;
        let path = crate::file_uri_to_path(&uri)
            .ok_or_else(|| ShotErr::Decode(format!("无法解析 uri: {uri}")))?;
        let bytes = std::fs::read(&path)
            .map_err(|e| ShotErr::CaptureFailed(format!("读取截图文件失败: {e}")))?;
        // portal 把 PNG 落在用户图片目录：读完即删，不留垃圾
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }
}

/// 非 Linux 平台的抓屏后端（Windows / macOS，见 Task 12 / Task 13）
#[cfg(not(target_os = "linux"))]
mod platform {
    use super::ShotErr;

    pub fn capture() -> Result<super::Captured, ShotErr> {
        Err(ShotErr::CaptureFailed("当前平台尚未实现抓屏".into()))
    }
}
```

- [ ] **Step 5: Update the `file_uri_to_path` doc comment**

Task 4 changed only its visibility. Its doc comment still says it serves *only* the GTK clipboard `text/uri-list`; it now also serves the portal `Screenshot` `uri`. Update that comment in `src-tauri/src/lib.rs` (comment text only — do not touch its body):

```
/// file:///home/a%20b.txt → /home/a b.txt；非 file 协议返回 None。
/// 服务两处：Linux 下读 GTK 剪贴板的 text/uri-list，以及 xdg-desktop-portal
/// 截图返回的 uri（两者都可能带百分号编码）。
```

- [ ] **Step 6: Add the diagnostic CLI**

In `src-tauri/src/lib.rs`, next to the `--clipboard-test` block, add:

```rust
    // 诊断模式：--shot-test 抓一次屏并落盘，打印尺寸（不启动界面）。
    // 用于在用户机器上定位抓屏问题：能出图说明后端没问题，问题在遮罩/前端。
    if std::env::args().any(|a| a == "--shot-test") {
        match screenshot::capture_png(std::time::Duration::from_secs(15)) {
            Ok(c) => {
                let out = std::env::temp_dir().join("oim-shot-test.png");
                std::fs::write(&out, &c.png).ok();
                println!("抓屏成功: {}x{} → {}", c.width, c.height, out.display());
            }
            Err(e) => {
                eprintln!("抓屏失败 [{}]: {}", e.code(), e.message());
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }
```

- [ ] **Step 7: Run tests and the real diagnostic**

Run: `cd src-tauri && cargo test screenshot::`
Expected: PASS — 7 tests green.

Run: `cargo build 2>&1 | tail -5 && ./target/debug/open-ipmsg --shot-test`
Expected: `抓屏成功: 5120x1440 → /tmp/oim-shot-test.png` (dimensions match this machine's dual-2K workspace; on a single-monitor machine one screen's size). Then verify the file is a real screenshot: open it and confirm it shows the desktop, and confirm no new file was left in the pictures directory.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/screenshot.rs src-tauri/src/lib.rs
git commit -m "feat(shot): Linux 走 xdg-desktop-portal 抓屏 + --shot-test 诊断"
```

---

### Task 6: Session cache, commands, and overlay windows

**Files:**
- Modify: `src-tauri/src/screenshot.rs` (state, commands, window creation)
- Modify: `src-tauri/src/lib.rs` (manage state, register commands)
- Modify: `src-tauri/capabilities/default.json`

**Interfaces:**
- Consumes: Task 5's `capture_png`, `Captured`, `ShotErr`.
- Produces: Tauri commands `start_screenshot() -> ShotCapture`, `shot_image(session, index) -> { b64, mime, slice, scale }`, `close_shot_overlays(session)`, `save_shot_png(b64, path)`, `copy_image_to_clipboard(b64)`; struct `ShotCapture { session, width, height, monitors: Vec<ShotMonitor> }`, `ShotMonitor { index, name, px, logical }`; managed state type `ShotState`.

Window labels are `shot-overlay-<i>`; the overlay URL is `index.html?viewer=shot&session=<id>&i=<i>` with `window.__OIM_SHOT__ = {session, index}` injected (same pattern as `open_image_viewer`).

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn shot_cache_expires_and_is_single_slot() {
        let cache = ShotState::default();
        assert!(cache.get("s1").is_err(), "空缓存应取不到");
        cache.put(CachedShot {
            session: "s1".into(),
            png_b64: "AAA".into(),
            width: 10,
            height: 10,
            monitors: vec![],
        });
        assert_eq!(cache.get("s1").unwrap().png_b64, "AAA");
        // 会话 id 不匹配（旧遮罩窗口）取不到
        assert!(cache.get("s0").is_err());
        // 新会话替换旧会话：旧 id 立即失效
        cache.put(CachedShot {
            session: "s2".into(),
            png_b64: "BBB".into(),
            width: 10,
            height: 10,
            monitors: vec![],
        });
        assert!(cache.get("s1").is_err());
        assert_eq!(cache.get("s2").unwrap().png_b64, "BBB");
        // close 幂等
        cache.clear();
        cache.clear();
        assert!(cache.get("s2").is_err());
    }

    #[test]
    fn monitor_index_out_of_range_is_rejected() {
        let mons = vec![ShotMonitor {
            index: 0,
            name: "DP-1".into(),
            px: Rect { x: 0, y: 0, w: 8, h: 8 },
            logical: Rect { x: 0, y: 0, w: 8, h: 8 },
        }];
        assert!(monitor_at(&mons, 0).is_ok());
        assert_eq!(monitor_at(&mons, 3).unwrap_err().code(), "CAPTURE_FAILED");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test screenshot::tests::shot_cache`
Expected: FAIL — `cannot find type ShotState in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `src-tauri/src/screenshot.rs`:

```rust
use std::sync::Mutex;

use base64::Engine as _;
use serde_json::{json, Value};
use tauri::Manager;

/// 单块显示器（index 是主键：Linux 上两块同型号屏的 name 会重名）
#[derive(Clone, Debug, Serialize)]
pub struct ShotMonitor {
    pub index: usize,
    pub name: String,
    /// 在整幅图像里的物理像素矩形（前端裁剪用）
    pub px: Rect,
    /// 逻辑像素矩形（开窗定位用）
    pub logical: Rect,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShotCapture {
    pub session: String,
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<ShotMonitor>,
}

pub struct CachedShot {
    pub session: String,
    pub png_b64: String,
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<ShotMonitor>,
}

/// 同一时刻只保留一个截图会话：遮罩窗口凭 session 取图，旧会话立即失效
#[derive(Default)]
pub struct ShotState(Mutex<Option<CachedShot>>);

impl ShotState {
    pub fn put(&self, shot: CachedShot) {
        *self.0.lock().unwrap() = Some(shot);
    }

    pub fn get(&self, session: &str) -> Result<CachedShot, ShotErr> {
        let guard = self.0.lock().unwrap();
        match guard.as_ref() {
            Some(s) if s.session == session => Ok(CachedShot {
                session: s.session.clone(),
                png_b64: s.png_b64.clone(),
                width: s.width,
                height: s.height,
                monitors: s.monitors.clone(),
            }),
            _ => Err(ShotErr::CaptureFailed("截图会话已失效，请重新截图".into())),
        }
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }

    pub fn active_session(&self) -> Option<String> {
        self.0.lock().unwrap().as_ref().map(|s| s.session.clone())
    }
}

pub fn monitor_at(monitors: &[ShotMonitor], index: usize) -> Result<ShotMonitor, ShotErr> {
    monitors
        .get(index)
        .cloned()
        .ok_or_else(|| ShotErr::CaptureFailed(format!("显示器序号越界: {index}")))
}

fn session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms:x}-{n}")
}

/// 抓屏并写缓存（**阻塞**，最长 15s）—— 只做重活，不开窗。
///
/// 必须从「非 tokio worker」的上下文调用（命令用 `spawn_blocking`，热键/命令行
/// 用自己的线程）：portal 的 Response 是阻塞等待，直接放在 async 命令里会占住
/// 一个 tokio worker 最长 15 秒。
pub fn capture_and_cache(
    app: &tauri::AppHandle,
    state: &ShotState,
) -> Result<ShotCapture, ShotErr> {
    // 已有一个会话：直接把现有会话还回去（不重复抓屏）
    if let Some(session) = state.active_session() {
        if let Ok(cached) = state.get(&session) {
            return Ok(ShotCapture {
                session: cached.session,
                width: cached.width,
                height: cached.height,
                monitors: cached.monitors,
            });
        }
        state.clear();
    }

    let cap = capture_png(Duration::from_secs(15))?;
    let monitors = collect_monitors(app, cap.width)?;
    let capture = ShotCapture {
        session: session_id(),
        width: cap.width,
        height: cap.height,
        monitors,
    };
    state.put(CachedShot {
        session: capture.session.clone(),
        png_b64: base64::engine::general_purpose::STANDARD.encode(&cap.png),
        width: capture.width,
        height: capture.height,
        monitors: capture.monitors.clone(),
    });
    Ok(capture)
}

/// 真正的入口（工具栏 / 热键 / 命令行都汇到这里）：抓屏 + 建遮罩窗口。
///
/// 抓屏在调用线程上阻塞完成（调用方保证这不是 tokio worker）；开窗沿用
/// `open_image_viewer` 已验证的写法 —— 直接在命令/事件线程上 build。
/// 会话已存在时 `capture_and_cache` 会直接返回旧会话，`open_overlays` 发现
/// 对应 label 的窗口已在，只做聚焦 —— 两层遮罩不会叠加。
pub fn begin_blocking(app: &tauri::AppHandle, state: &ShotState) -> Result<ShotCapture, ShotErr> {
    let capture = capture_and_cache(app, state)?;
    open_overlays(app, &capture)?;
    Ok(capture)
}

/// 显示器清单：逻辑矩形来自窗口系统的真实布局，物理矩形由 k 推得
fn collect_monitors(app: &tauri::AppHandle, image_w: u32) -> Result<Vec<ShotMonitor>, ShotErr> {
    let logical = logical_monitors(app)?;
    let bounds = virtual_bounds(&logical.iter().map(|(_, r)| *r).collect::<Vec<_>>());
    let k = scale_for(image_w, bounds.w.max(1));
    Ok(logical
        .into_iter()
        .enumerate()
        .map(|(i, (name, r))| ShotMonitor {
            index: i,
            name,
            px: slice_for_monitor(r, bounds, k),
            logical: r,
        })
        .collect())
}

/// Linux：逻辑几何必须直接问 GDK。
///
/// tao 的 `Monitor::position()/size()` 是「GDK 逻辑 × GDK 整数缩放」，本机
/// 真实比例 1.25 却按 2 乘，会得到第三套坐标，遮罩必然错位。
#[cfg(target_os = "linux")]
fn logical_monitors(app: &tauri::AppHandle) -> Result<Vec<(String, Rect)>, ShotErr> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        use gtk::prelude::*;
        let list = gtk::gdk::Display::default()
            .map(|d| {
                // GDK3（gtk 0.18）没有 `display.monitors()`：按序号逐个取，
                // 这样 index 与 `fullscreen_on_monitor` 用的序号同源
                (0..d.n_monitors())
                    .filter_map(|i| {
                        let m = d.monitor(i)?;
                        let g = m.geometry();
                        let name = m
                            .model()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("monitor-{i}"));
                        Some((
                            name,
                            Rect { x: g.x(), y: g.y(), w: g.width() as u32, h: g.height() as u32 },
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = tx.send(list);
    })
    .map_err(|e| ShotErr::CaptureFailed(format!("枚举显示器失败: {e}")))?;
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| ShotErr::CaptureFailed("枚举显示器超时".into()))
}

#[cfg(not(target_os = "linux"))]
fn logical_monitors(app: &tauri::AppHandle) -> Result<Vec<(String, Rect)>, ShotErr> {
    let mons = app
        .available_monitors()
        .map_err(|e| ShotErr::CaptureFailed(format!("枚举显示器失败: {e}")))?;
    Ok(mons
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let scale = m.scale_factor().max(0.1);
            let pos = m.position();
            let size = m.size();
            (
                m.name().cloned().unwrap_or_else(|| format!("monitor-{i}")),
                Rect {
                    x: (pos.x as f64 / scale).round() as i32,
                    y: (pos.y as f64 / scale).round() as i32,
                    w: (size.width as f64 / scale).round() as u32,
                    h: (size.height as f64 / scale).round() as u32,
                },
            )
        })
        .collect())
}

/// 是否 Wayland 会话（决定遮罩窗口是「每屏一个全屏」还是「一个跨虚拟桌面」）
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false)
        || std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
}

/// 建立遮罩窗口
fn open_overlays(app: &tauri::AppHandle, cap: &ShotCapture) -> Result<(), ShotErr> {
    let wayland = is_wayland();
    let count = if wayland { cap.monitors.len().max(1) } else { 1 };
    for i in 0..count {
        let label = format!("shot-overlay-{i}");
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.set_focus();
            continue;
        }
        let url = format!("index.html?viewer=shot&session={}&i={i}", cap.session);
        let boot = format!(
            "window.__OIM_SHOT__ = {};",
            json!({ "session": cap.session, "index": i })
        );
        let mut builder =
            tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App(url.into()))
                .initialization_script(boot)
                .title("Screenshot")
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .shadow(false)
                .focused(true);
        if wayland {
            // Wayland 不允许客户端定位窗口：先建小窗，再在 GTK 主线程上指定显示器全屏
            builder = builder.inner_size(320.0, 200.0);
        } else {
            let bounds = virtual_bounds(&cap.monitors.iter().map(|m| m.logical).collect::<Vec<_>>());
            builder = builder
                .position(bounds.x as f64, bounds.y as f64)
                .inner_size(bounds.w.max(1) as f64, bounds.h.max(1) as f64);
        }
        let win = builder
            .build()
            .map_err(|e| ShotErr::CaptureFailed(format!("创建遮罩窗口失败: {e}")))?;
        // 用户 Alt+F4 关掉遮罩时也要释放会话缓存，否则下次触发会拿到陈旧会话
        let watcher = app.clone();
        win.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                let remaining = watcher
                    .webview_windows()
                    .keys()
                    .any(|l| l.starts_with("shot-overlay-"));
                if !remaining {
                    watcher.state::<ShotState>().clear();
                    oim_log!("[shot] 遮罩全部关闭，会话缓存已释放");
                }
            }
        });
        if wayland {
            fullscreen_on_monitor(&win, i)?;
        } else {
            let _ = win.set_focus();
        }
    }
    Ok(())
}

/// Wayland：请求在指定显示器上全屏（xdg-shell 的 set_fullscreen 支持 output）
#[cfg(target_os = "linux")]
fn fullscreen_on_monitor(win: &tauri::WebviewWindow, index: usize) -> Result<(), ShotErr> {
    let w = win.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    win.app_handle()
        .run_on_main_thread(move || {
            use gtk::prelude::*;
            if let Ok(gw) = w.gtk_window() {
                // GDK3 的签名是 `fullscreen_on_monitor(&Screen, monitor 序号)`，
                // 屏幕取窗口自身所在的那块（取不到再退默认屏）；
                // `screen` 在 GtkWindowExt 与 WidgetExt 上都有，必须写全路径
                let screen = gtk::prelude::GtkWindowExt::screen(&gw)
                    .or_else(gtk::gdk::Screen::default);
                let exists = gtk::gdk::Display::default()
                    .and_then(|d| d.monitor(index as i32))
                    .is_some();
                match (screen, exists) {
                    (Some(s), true) => gw.fullscreen_on_monitor(&s, index as i32),
                    // 取不到该显示器就退化为普通全屏（落在窗口当前所在屏）
                    _ => gw.fullscreen(),
                }
                gw.show_all();
            }
            let _ = tx.send(());
        })
        .map_err(|e| ShotErr::CaptureFailed(format!("请求全屏失败: {e}")))?;
    let _ = rx.recv_timeout(Duration::from_secs(3));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn fullscreen_on_monitor(win: &tauri::WebviewWindow, _index: usize) -> Result<(), ShotErr> {
    win.set_fullscreen(true)
        .map_err(|e| ShotErr::CaptureFailed(format!("请求全屏失败: {e}")))
}

/* ---------------- Tauri 命令 ---------------- */

#[tauri::command]
pub async fn start_screenshot(app: tauri::AppHandle) -> Result<ShotCapture, String> {
    // 抓屏最长阻塞 15s：放到 blocking 线程，绝不占住 tokio worker
    let app2 = app.clone();
    let capture = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<ShotState>();
        capture_and_cache(&app2, &state)
    })
    .await
    .map_err(|e| format!("CAPTURE_FAILED|抓屏任务失败: {e}"))?
    .map_err(|e| format!("{}|{}", e.code(), e.message()))?;

    // 开窗回到命令线程：与 open_image_viewer 同一写法（已在本仓库验证过）
    if let Err(e) = open_overlays(&app, &capture) {
        return Err(format!("{}|{}", e.code(), e.message()));
    }
    oim_log!(
        "[shot] 抓屏成功 {}x{}，{} 块屏，会话 {}",
        capture.width,
        capture.height,
        capture.monitors.len(),
        capture.session
    );
    Ok(capture)
}

#[tauri::command]
pub async fn shot_image(
    session: String,
    index: usize,
    state: tauri::State<'_, ShotState>,
) -> Result<Value, String> {
    let cached = state.get(&session).map_err(|e| e.message())?;
    let mon = monitor_at(&cached.monitors, index).map_err(|e| e.message())?;
    let scale = mon.px.w as f64 / mon.logical.w.max(1) as f64;
    Ok(json!({
        "b64": cached.png_b64,
        "mime": "image/png",
        "slice": mon.px,
        "scale": scale,
        "total": { "w": cached.width, "h": cached.height },
    }))
}

#[tauri::command]
pub async fn close_shot_overlays(
    app: tauri::AppHandle,
    session: String,
    state: tauri::State<'_, ShotState>,
) -> Result<(), String> {
    for (label, w) in app.webview_windows() {
        if label.starts_with("shot-overlay-") {
            let _ = w.destroy();
        }
    }
    if state.active_session().as_deref() == Some(session.as_str()) {
        state.clear();
    }
    oim_log!("[shot] 遮罩已关闭 session={session}");
    Ok(())
}

#[tauri::command]
pub async fn save_shot_png(b64: String, path: String) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("图片数据非法: {e}"))?;
    std::fs::write(&path, bytes).map_err(|e| format!("保存失败: {e}"))
}

/// 把 PNG 写进系统剪贴板（Linux 走 GTK：与现有 clipboard_image 读路径对称）
#[tauri::command]
pub async fn copy_image_to_clipboard(app: tauri::AppHandle, b64: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| format!("图片数据非法: {e}"))?;
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        app.run_on_main_thread(move || {
            use gtk::prelude::*;
            let loader = gtk::gdk_pixbuf::PixbufLoader::new();
            let ok = loader.write(&bytes).is_ok()
                && loader.close().is_ok()
                && loader.pixbuf().is_some_and(|pb| {
                    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_image(&pb);
                    true
                });
            let _ = tx.send(ok);
        })
        .map_err(|e| format!("写剪贴板失败: {e}"))?;
        return match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(true) => Ok(()),
            Ok(false) => Err("剪贴板写入失败（图片解码失败）".into()),
            Err(_) => Err("写剪贴板超时".into()),
        };
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows/macOS 由前端用 clipboard-manager 插件写图片
        let _ = (app, b64);
        Err("PLUGIN".into())
    }
}

/// 供 shortcut.rs / 命令行调用：抓屏并建遮罩。
///
/// 这两个调用点都在主线程上（插件回调 / setup / 单实例回调），而抓屏要阻塞
/// 十几秒 —— 所以自己起线程做完整流程（Tauri 的建窗可以从任意线程发起，
/// 与 `open_image_viewer` 同一个机制），主线程立刻返回。
pub fn trigger(app: &tauri::AppHandle) -> Result<(), ShotErr> {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let state = app2.state::<ShotState>();
        if let Err(e) = begin_blocking(&app2, &state) {
            oim_log!("[shot] 触发失败 [{}]：{}", e.code(), e.message());
        }
    });
    Ok(())
}
```

- [ ] **Step 4: Wire up state, commands, and capabilities**

In `src-tauri/src/lib.rs`:

1. In `.setup(...)`, next to the other `.manage(...)` calls, add `app.manage(screenshot::ShotState::default());`
2. In `tauri::generate_handler![...]`, add:

```rust
            screenshot::start_screenshot,
            screenshot::shot_image,
            screenshot::close_shot_overlays,
            screenshot::save_shot_png,
            screenshot::copy_image_to_clipboard,
```

In `src-tauri/capabilities/default.json`, add the overlay windows and the clipboard image permission:

```json
  "windows": [
    "main",
    "image-viewer-*",
    "shot-overlay-*"
  ],
```

```json
    "clipboard-manager:allow-write-text",
    "clipboard-manager:allow-write-image",
```

- [ ] **Step 5: Run tests**

Run: `cd src-tauri && cargo test screenshot::`
Expected: PASS — 8 tests green.

- [ ] **Step 6: Verify overlay windows appear (temporary trigger)**

Run: `cd src-tauri && cargo build 2>&1 | tail -3`, then add a temporary line to the `--shot-test` branch that also starts the app… instead, verify through the UI in Task 7. For now confirm compilation and that `cargo test` passes; the visible check happens in Task 7 Step 6.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/screenshot.rs src-tauri/src/lib.rs src-tauri/capabilities/default.json
git commit -m "feat(shot): 截图会话缓存、命令与遮罩窗口（Wayland 每屏一个）"
```

---

### Task 7: Overlay component — image, dim hole, selection

**Files:**
- Create: `src/components/ScreenshotOverlay.vue`
- Modify: `src/main.js` (viewer branch)
- Modify: `src/lib/ipc.js` (command wrappers + event names)

**Interfaces:**
- Consumes: `shot_image` / `close_shot_overlays` (Task 6); `rectFromDrag`, `clampRect`, `canConfirm`, `hitTestHandle`, `resizeRect`, `moveRect`, `nudgeRect`, `cssRectToImageRect`, `toolbarPlacement` (Tasks 1–2).
- Produces: mounted overlay window; on confirm it emits `screenshot-done` with `{ b64, mime, size, width, height }`; on copy it emits `screenshot-copy` with `{ b64 }` (wired in Task 9).

- [ ] **Step 1: Add IPC wrappers**

In `src/lib/ipc.js`, add:

```js
/* ---------------- 截图 ---------------- */

/** 开始截图：后端抓屏并打开遮罩窗口，返回 { session, width, height, monitors } */
export const startScreenshot = () => invoke('start_screenshot')

/** 遮罩窗口取图：返回 { b64, mime, slice, scale, total } */
export const shotImage = (session, index) => invoke('shot_image', { session, index })

/** 关闭全部遮罩窗口并释放会话缓存（幂等） */
export const closeShotOverlays = (session) => invoke('close_shot_overlays', { session })

/** 把确认后的 PNG 另存为文件 */
export const saveShotPng = (b64, path) => invoke('save_shot_png', { b64, path })

/** 把 PNG 写进系统剪贴板（Linux 后端 GTK；其他平台返回 PLUGIN 由前端插件兜底） */
export const copyShotImage = (b64) => invoke('copy_image_to_clipboard', { b64 })
```

And extend `EVT`:

```js
  /** 截图确认：遮罩窗口 → 主窗口，进入待发送列表 */
  screenshotDone: 'screenshot-done',
  /** 截图「复制」按钮：只写剪贴板、不进待发送列表 */
  screenshotCopy: 'screenshot-copy',
```

- [ ] **Step 2: Add the overlay entry branch**

In `src/main.js`, add next to the image-viewer branch:

```js
import ScreenshotOverlay from './components/ScreenshotOverlay.vue'

// 截图遮罩是同一份前端的第三个入口（由 start_screenshot 打开的独立窗口）
const isShot =
  new URLSearchParams(location.search).get('viewer') === 'shot' || !!window.__OIM_SHOT__

if (isShot) {
  createApp(ScreenshotOverlay).mount('#app')
  // 遮罩窗口也要有正确的主题变量，但不启动网络栈
  refreshConfig().catch((e) => console.error('shot config failed', e))
} else if (isViewer) { /* 既有分支不变 */ }
```

- [ ] **Step 3: Write the component**

Create `src/components/ScreenshotOverlay.vue`:

```vue
<script setup>
// 截图遮罩窗口：底图 + 变暗挖洞 + 拖拽选区。
// 标注层与工具栏在 Task 8 接入；本任务先打通「取图 → 选区 → 确认/取消」。
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import * as ipc from '../lib/ipc'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle, resizeRect, moveRect, nudgeRect,
  cssRectToImageRect,
} from '../lib/shot'
import { t } from '../lib/i18n'

const boot = window.__OIM_SHOT__ || {}
const qs = new URLSearchParams(location.search)
const session = boot.session || qs.get('session') || ''
const index = Number(boot.index ?? qs.get('i') ?? 0)

const root = ref(null)
const baseCanvas = ref(null)
const img = ref(null)          // 已解码的整幅图像（Image 对象）
const slice = ref({ x: 0, y: 0, w: 1, h: 1 })
const sel = ref(null)          // 当前选区（CSS 像素），null = 未选
const busy = ref(false)
const errMsg = ref('')
const hint = ref('')

let drag = null                // { mode:'new'|'move'|'resize', handle, start, origin }

const winRect = computed(() => ({
  x: 0, y: 0,
  w: root.value?.clientWidth || window.innerWidth,
  h: root.value?.clientHeight || window.innerHeight,
}))

const selStyle = computed(() => {
  const s = sel.value
  if (!s) return { display: 'none' }
  return { left: s.x + 'px', top: s.y + 'px', width: s.w + 'px', height: s.h + 'px' }
})

const sizeLabel = computed(() => {
  const s = sel.value
  if (!s) return ''
  const r = cssRectToImageRect(s, slice.value, winRect.value.w)
  return `${r.w} × ${r.h}`
})

const hover = ref('')
const cursor = computed(() => {
  if (!sel.value) return 'crosshair'
  const map = {
    nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize',
    n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
    inside: 'move', outside: 'crosshair',
  }
  return map[hover.value] || 'crosshair'
})

const canOk = computed(() => canConfirm(sel.value))

/** 底图：把本屏 slice 画满整窗（canvas 后备像素 = slice 物理像素，1:1 清晰） */
function paintBase() {
  const c = baseCanvas.value
  const source = img.value
  if (!c || !source) return
  const w = winRect.value.w
  const h = winRect.value.h
  c.width = slice.value.w
  c.height = slice.value.h
  c.style.width = w + 'px'
  c.style.height = h + 'px'
  const ctx = c.getContext('2d')
  ctx.clearRect(0, 0, c.width, c.height)
  ctx.drawImage(source, slice.value.x, slice.value.y, slice.value.w, slice.value.h, 0, 0, c.width, c.height)
}

async function load() {
  try {
    const r = await ipc.shotImage(session, index)
    slice.value = r.slice
    const image = new Image()
    await new Promise((res, rej) => {
      image.onload = res
      image.onerror = () => rej(new Error('image decode failed'))
      image.src = 'data:' + (r.mime || 'image/png') + ';base64,' + r.b64
    })
    img.value = image
    await nextTick()
    paintBase()
  } catch (e) {
    errMsg.value = String(e?.message || e)
  }
}

function localPoint(ev) {
  const r = root.value.getBoundingClientRect()
  return { x: ev.clientX - r.left, y: ev.clientY - r.top }
}

function onPointerDown(ev) {
  if (ev.button !== 0 || busy.value) return
  const p = localPoint(ev)
  const hit = sel.value ? hitTestHandle(sel.value, p) : 'outside'
  if (hit === 'inside') {
    drag = { mode: 'move', start: p, origin: { ...sel.value } }
  } else if (hit !== 'outside') {
    drag = { mode: 'resize', handle: hit, origin: { ...sel.value } }
  } else {
    sel.value = { x: p.x, y: p.y, w: 0, h: 0 }
    drag = { mode: 'new', start: p }
  }
  root.value.setPointerCapture?.(ev.pointerId)
}

function onPointerMove(ev) {
  const p = localPoint(ev)
  if (!drag) {
    hover.value = sel.value ? hitTestHandle(sel.value, p) : ''
    return
  }
  if (drag.mode === 'new') {
    sel.value = rectFromDrag(drag.start, p, winRect.value)
  } else if (drag.mode === 'move') {
    sel.value = moveRect(drag.origin, p.x - drag.start.x, p.y - drag.start.y, winRect.value)
  } else {
    sel.value = resizeRect(drag.origin, drag.handle, p, winRect.value)
  }
}

function onPointerUp() {
  if (drag && !canConfirm(sel.value)) sel.value = null
  drag = null
}

function onKeydown(ev) {
  if (ev.key === 'Escape') return cancel()
  if (ev.key === 'Enter') return confirm()
  if (ev.key.startsWith('Arrow') && sel.value) {
    ev.preventDefault()
    sel.value = nudgeRect(sel.value, ev.key, ev.shiftKey ? 10 : 1, winRect.value)
  }
}

async function confirm() {
  if (!canOk.value || busy.value) return
  hint.value = t('shot.todoConfirm')
}

async function cancel() {
  if (busy.value) return
  busy.value = true
  try {
    await ipc.closeShotOverlays(session)
  } finally {
    busy.value = false
  }
}

function onResize() {
  paintBase()
}

onMounted(() => {
  load()
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('resize', onResize)
})
onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('resize', onResize)
})
</script>

<template>
  <div
    ref="root"
    class="shot-root"
    :style="{ cursor }"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @contextmenu.prevent="cancel"
  >
    <canvas ref="baseCanvas" class="base"></canvas>
    <div class="dim" :style="selStyle">
      <div class="frame"></div>
      <div class="size" v-if="sel">{{ sizeLabel }}</div>
      <span v-for="h in ['nw','n','ne','e','se','s','sw','w']" :key="h" :class="['handle', h]"></span>
    </div>
    <div v-if="!sel && !errMsg" class="tip">{{ t('shot.tip') }}</div>
    <div v-if="errMsg" class="error">{{ errMsg }}</div>
    <div v-if="hint" class="tip bottom">{{ hint }}</div>
  </div>
</template>

<style scoped>
.shot-root {
  position: fixed;
  inset: 0;
  overflow: hidden;
  background: #000;
  user-select: none;
}
.base {
  position: absolute;
  left: 0;
  top: 0;
  image-rendering: pixelated;
}
/* 变暗用「挖洞」实现：选区那一块不盖黑罩，靠超大 box-shadow 覆盖其余区域。
   比每帧重绘底图便宜得多（GPU 合成），拖拽 100% 跟手。 */
.dim {
  position: absolute;
  box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.45);
  outline: 1px solid rgba(255, 255, 255, 0.9);
}
.frame {
  position: absolute;
  inset: 0;
  border: 1px solid #1aad19;
}
.size {
  position: absolute;
  left: 0;
  top: -22px;
  padding: 1px 6px;
  font-size: 12px;
  color: #fff;
  background: rgba(0, 0, 0, 0.6);
  border-radius: 3px;
  white-space: nowrap;
}
.handle {
  position: absolute;
  width: 8px;
  height: 8px;
  margin: -4px;
  background: #fff;
  border: 1px solid #1aad19;
}
.handle.nw { left: 0; top: 0; }
.handle.n { left: 50%; top: 0; }
.handle.ne { left: 100%; top: 0; }
.handle.e { left: 100%; top: 50%; }
.handle.se { left: 100%; top: 100%; }
.handle.s { left: 50%; top: 100%; }
.handle.sw { left: 0; top: 100%; }
.handle.w { left: 0; top: 50%; }
.tip {
  position: absolute;
  left: 50%;
  top: 24px;
  transform: translateX(-50%);
  padding: 6px 12px;
  font-size: 13px;
  color: #fff;
  background: rgba(0, 0, 0, 0.65);
  border-radius: 4px;
  pointer-events: none;
}
.tip.bottom { top: auto; bottom: 24px; }
.error {
  position: absolute;
  left: 50%;
  top: 50%;
  transform: translate(-50%, -50%);
  padding: 16px 20px;
  color: #fff;
  background: rgba(180, 40, 40, 0.92);
  border-radius: 6px;
  max-width: 60%;
}
</style>
```

Note: `toolbarPlacement` is imported now and used in Task 8 — if the SFC-binding check flags an unused import it does not (it only checks that template names exist in script). Keep the import; Task 8 uses it in the same file.

- [ ] **Step 4: Add the i18n strings used here**

In `src/lib/i18n.js`, add to **both** `zh` and `en`:

```js
  // zh
  'shot.tip': '拖拽选择截图区域，Enter 确认，Esc 取消',
  'shot.todoConfirm': '确认链路将在下一步接通',
  // en
  'shot.tip': 'Drag to select an area. Enter to confirm, Esc to cancel',
  'shot.todoConfirm': 'Confirm pipeline lands in the next step',
```

- [ ] **Step 5: Add the component to the SFC binding check**

In `scripts/sfc-bindings.test.mjs`, add to the explicit list:

```js
  '../src/components/ScreenshotOverlay.vue',
```

- [ ] **Step 6: Verify the overlay actually appears and selects**

Run: `pnpm test` (expect PASS) and then `pnpm tauri dev`.
In the running app, temporarily trigger a capture from the devtools console of the main window:
`window.__TAURI__.core.invoke('start_screenshot')`.
Expected: on this KDE Wayland dual-monitor machine **two** overlay windows appear, each fullscreen on one monitor, each showing that monitor's frozen content pixel-aligned; dragging draws a green-bordered selection with a live `W × H` label in physical pixels; `Esc` closes both overlays; a second trigger works again.
Remove the temporary console trigger afterwards (no code change was made for it).

- [ ] **Step 7: Commit**

```bash
git add src/components/ScreenshotOverlay.vue src/main.js src/lib/ipc.js src/lib/i18n.js scripts/sfc-bindings.test.mjs
git commit -m "feat(shot): 遮罩窗口组件（取图/变暗挖洞/拖拽选区/微调/取消）"
```

---

### Task 8: Overlay annotations and undo

**Files:**
- Modify: `src/components/ScreenshotOverlay.vue`
- Modify: `src/lib/i18n.js`

**Interfaces:**
- Consumes: Task 2's `mosaicBlocks`, `arrowHead`, `pushUndo`, `toolbarPlacement`; Task 7's component state.
- Produces: annotation tools `rect | ellipse | arrow | pen | text | mosaic`, undo via `Ctrl+Z`, toolbar rendered from `toolbarPlacement`, and a `composite()` function returning a PNG data URL of the cropped, annotated selection (consumed by Task 9).

- [ ] **Step 1: Add annotation state and drawing**

Extend the `<script setup>` of `src/components/ScreenshotOverlay.vue`. First widen the Task 7 import:

```js
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle, resizeRect, moveRect, nudgeRect,
  cssRectToImageRect, toolbarPlacement, mosaicBlocks, arrowHead, pushUndo,
} from '../lib/shot'
```

Then add the annotation state:

```js
const annoCanvas = ref(null)
const tool = ref('move')    // 'move' = 拖拽/移动选区；其余值为标注工具
const color = ref('#e64340')
const width = ref(3)
const blockSize = ref(10)
const undoStack = ref([])
let drawing = null          // { tool, from, to, points }
const textAt = ref(null)    // { x, y, value } 文字工具的输入框位置
const textInput = ref(null)

const TOOLS = ['move', 'rect', 'ellipse', 'arrow', 'pen', 'text', 'mosaic']
const COLORS = ['#e64340', '#ff8c00', '#ffd400', '#1aad19', '#1e6fff', '#000000']
const WIDTHS = [2, 3, 5]
const BLOCKS = [6, 10, 16]

/** 标注 canvas 与窗口同尺寸（CSS 像素 1:1，导出时再乘 k） */
function paintAnnoSize() {
  const c = annoCanvas.value
  if (!c) return
  const w = winRect.value.w
  const h = winRect.value.h
  if (c.width === w && c.height === h) return
  const keep = c.width && c.height ? c.toDataURL() : ''
  c.width = w
  c.height = h
  if (keep) {
    const im = new Image()
    im.onload = () => c.getContext('2d').drawImage(im, 0, 0)
    im.src = keep
  }
}

function snapshot() {
  const c = annoCanvas.value
  if (!c) return null
  return c.getContext('2d').getImageData(0, 0, c.width, c.height)
}

function pushSnapshot() {
  undoStack.value = pushUndo(undoStack.value, snapshot(), 20)
}

function undo() {
  const c = annoCanvas.value
  const stack = undoStack.value.slice()
  const last = stack.pop()
  if (!last || !c) return
  undoStack.value = stack
  c.getContext('2d').putImageData(last, 0, 0)
}

function drawShape(ctx, d) {
  ctx.strokeStyle = color.value
  ctx.fillStyle = color.value
  ctx.lineWidth = width.value
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  if (d.tool === 'rect') {
    ctx.strokeRect(d.from.x, d.from.y, d.to.x - d.from.x, d.to.y - d.from.y)
  } else if (d.tool === 'ellipse') {
    const cx = (d.from.x + d.to.x) / 2
    const cy = (d.from.y + d.to.y) / 2
    ctx.beginPath()
    ctx.ellipse(cx, cy, Math.abs(d.to.x - d.from.x) / 2, Math.abs(d.to.y - d.from.y) / 2, 0, 0, Math.PI * 2)
    ctx.stroke()
  } else if (d.tool === 'arrow') {
    ctx.beginPath()
    ctx.moveTo(d.from.x, d.from.y)
    ctx.lineTo(d.to.x, d.to.y)
    ctx.stroke()
    const { p1, p2 } = arrowHead(d.from, d.to, 8 + width.value * 2)
    ctx.beginPath()
    ctx.moveTo(d.to.x, d.to.y)
    ctx.lineTo(p1.x, p1.y)
    ctx.lineTo(p2.x, p2.y)
    ctx.closePath()
    ctx.fill()
  } else if (d.tool === 'pen') {
    ctx.beginPath()
    d.points.forEach((pt, i) => (i ? ctx.lineTo(pt.x, pt.y) : ctx.moveTo(pt.x, pt.y)))
    ctx.stroke()
  }
}

/** 马赛克：读底图对应区域的像素，按块平均后回填 */
function applyMosaic(ctx, rect) {
  const base = baseCanvas.value
  if (!base) return
  const bctx = base.getContext('2d')
  for (const b of mosaicBlocks(rect, blockSize.value)) {
    const data = bctx.getImageData(b.x, b.y, b.w, b.h).data
    let r = 0, g = 0, bl = 0, n = 0
    for (let i = 0; i < data.length; i += 4) {
      r += data[i]; g += data[i + 1]; bl += data[i + 2]; n++
    }
    if (!n) continue
    ctx.fillStyle = `rgb(${Math.round(r / n)},${Math.round(g / n)},${Math.round(bl / n)})`
    ctx.fillRect(b.x, b.y, b.w, b.h)
  }
}
```

Extend `onPointerDown` / `onPointerMove` / `onPointerUp`: when the pointer press lands **inside** the selection and a drawing tool is active, start `drawing` instead of moving the selection:

```js
  if (hit === 'inside' && tool.value !== 'move' && sel.value) {
    if (tool.value === 'text') {
      textAt.value = { x: p.x, y: p.y, value: '' }
      nextTick(() => textInput.value?.focus())
      return
    }
    pushSnapshot()
    drawing = { tool: tool.value, from: p, to: p, points: [p] }
    return
  }
```

In `onPointerMove`, when `drawing` is active, redraw the preview from the snapshot on each move (restore the last snapshot, then draw the in-progress shape):

```js
  if (drawing) {
    const ctx = annoCanvas.value.getContext('2d')
    const snap = undoStack.value[undoStack.value.length - 1]
    if (snap) ctx.putImageData(snap, 0, 0)
    drawing.to = p
    if (drawing.tool === 'pen') drawing.points.push(p)
    if (drawing.tool === 'mosaic') applyMosaic(ctx, { x: drawing.from.x, y: drawing.from.y, w: p.x - drawing.from.x, h: p.y - drawing.from.y })
    else drawShape(ctx, drawing)
    return
  }
```

In `onPointerUp`, finish `drawing` (`drawing = null`) and keep the pushed snapshot as the undo point. Add the text commit:

```js
function commitText() {
  const at = textAt.value
  if (!at || !annoCanvas.value) return
  const value = (at.value || '').trim()
  if (value) {
    const ctx = annoCanvas.value.getContext('2d')
    pushSnapshot()
    ctx.fillStyle = color.value
    ctx.font = `${12 + width.value * 4}px system-ui, sans-serif`
    ctx.textBaseline = 'top'
    ctx.fillText(value, at.x, at.y)
  }
  textAt.value = null
}
```

Add the toolbar template inside `.shot-root` (after the dim div):

```html
    <div v-if="sel" class="toolbar" :style="barStyle" @pointerdown.stop @pointerup.stop>
      <button v-for="tl in TOOLS" :key="tl" :class="{ on: tool === tl }" :title="t('shot.tool.' + tl)"
        @click="tool = tl">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <path v-if="tl === 'move'" d="M12 3v18M3 12h18M12 3l-3 3M12 3l3 3M12 21l-3-3M12 21l3-3M3 12l3-3M3 12l3 3M21 12l-3-3M21 12l-3 3" />
          <rect v-else-if="tl === 'rect'" x="4" y="6" width="16" height="12" rx="1" />
          <ellipse v-else-if="tl === 'ellipse'" cx="12" cy="12" rx="8" ry="6" />
          <path v-else-if="tl === 'arrow'" d="M4 18L20 6M20 6h-6M20 6v6" />
          <path v-else-if="tl === 'pen'" d="M4 20c4-1 5-6 8-9s5-5 8-7" />
          <path v-else-if="tl === 'text'" d="M5 6h14M12 6v13" />
          <path v-else d="M4 4h16v16H4zM8 8h3v3H8zM14 13h3v3h-3z" />
        </svg>
      </button>
      <span class="sep"></span>
      <button v-for="c in COLORS" :key="c" class="dot" :style="{ background: c }"
        :class="{ on: color === c }" @click="color = c"></button>
      <span class="sep"></span>
      <button v-for="w in WIDTHS" :key="w" :class="{ on: width === w }" @click="width = w">
        <span class="wdot" :style="{ width: w * 2 + 'px', height: w * 2 + 'px' }"></span>
      </button>
      <button v-if="tool === 'mosaic'" v-for="b in BLOCKS" :key="b" :class="{ on: blockSize === b }"
        @click="blockSize = b">{{ b }}</button>
      <span class="sep"></span>
      <button :title="t('shot.undo')" @click="undo">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <path d="M9 7L4 12l5 5M4 12h10a5 5 0 0 1 0 10" />
        </svg>
      </button>
    </div>
    <input v-if="textAt" ref="textInput" v-model="textAt.value" class="text-in"
      :style="{ left: textAt.x + 'px', top: textAt.y + 'px' }"
      @keydown.enter.prevent="commitText" @keydown.esc.prevent="textAt = null" @blur="commitText" />
```

with

```js
const barStyle = computed(() => {
  const bar = { w: 420, h: 40 }
  const p = toolbarPlacement(sel.value || { x: 0, y: 0, w: 0, h: 0 }, winRect.value, bar)
  return { left: p.x + 'px', top: p.y + 'px' }
})
```

and CSS for `.toolbar`, `.toolbar button`, `.toolbar button.on`, `.sep`, `.dot`, `.wdot`, `.text-in` (toolbar: `position:absolute; display:flex; align-items:center; gap:4px; height:36px; padding:0 6px; background:rgba(32,32,32,.92); border-radius:6px; color:#eee`; `.on { color:#1aad19 }`; `.dot { width:16px; height:16px; border-radius:50%; border:2px solid transparent }`; `.dot.on { border-color:#fff }`; `.text-in { position:absolute; min-width:120px; padding:2px 6px; border:1px solid #1aad19; outline:none; background:#fff; color:#000; font-size:14px }`).

Also call `paintAnnoSize()` from `paintBase()` so the annotation layer matches the window after a resize, and add `Ctrl+Z` to `onKeydown`:

```js
  if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === 'z') {
    ev.preventDefault()
    return undo()
  }
```

- [ ] **Step 2: Add i18n strings**

In `src/lib/i18n.js`, add to **both** languages:

```js
  // zh
  'shot.tool.move': '移动/调整选区', 'shot.tool.rect': '矩形', 'shot.tool.ellipse': '椭圆',
  'shot.tool.arrow': '箭头', 'shot.tool.pen': '画笔', 'shot.tool.text': '文字', 'shot.tool.mosaic': '马赛克',
  'shot.undo': '撤销',
  // en
  'shot.tool.move': 'Move/resize selection', 'shot.tool.rect': 'Rectangle', 'shot.tool.ellipse': 'Ellipse',
  'shot.tool.arrow': 'Arrow', 'shot.tool.pen': 'Pen', 'shot.tool.text': 'Text', 'shot.tool.mosaic': 'Mosaic',
  'shot.undo': 'Undo',
```

- [ ] **Step 3: Run the contract tests**

Run: `pnpm test`
Expected: PASS — including `sfc-bindings` for `ScreenshotOverlay.vue` (the template references only names defined in `<script setup>`).

- [ ] **Step 4: Verify annotation behaviour by hand**

Run: `pnpm tauri dev`, trigger `invoke('start_screenshot')` from the main window console, then:
- drag a selection, pick each tool, draw inside it — shapes stay clipped to the selection;
- `Ctrl+Z` undoes the last shape one at a time;
- switch to 马赛克 and drag over text — the area becomes blocks sampled from the underlying screenshot (not from other annotations);
- 文字 places an input, `Enter` burns the text into the annotation layer, `Esc` cancels it;
- moving/resizing the selection after drawing keeps annotations in place (they are window-anchored, exactly like WeChat).

- [ ] **Step 5: Commit**

```bash
git add src/components/ScreenshotOverlay.vue src/lib/i18n.js
git commit -m "feat(shot): 标注工具（矩形/椭圆/箭头/画笔/文字/马赛克）与撤销"
```

---

### Task 9: Confirm pipeline — composite, pending list, clipboard

**Files:**
- Modify: `src/components/ScreenshotOverlay.vue`
- Modify: `src/components/ChatWindow.vue`
- Modify: `src/lib/clipimg.js` (add `b64ToBytes`)
- Modify: `scripts/clipimg.test.mjs`
- Modify: `src/lib/i18n.js`

**Interfaces:**
- Consumes: `cssRectToImageRect` (Task 2), `shot_image` / `close_shot_overlays` / `save_shot_png` / `copy_image_to_clipboard` (Task 6), the annotation canvas (Task 8), `pendingOf` / `imgItemName` / `pendingImgFromB64` in `ChatWindow.vue`.
- Produces: `compositePngDataUrl()` in the overlay; `screenshot-done` → pending item; `screenshot-copy` → clipboard only. Both events carry `{ b64, mime, size, width, height }`.

- [ ] **Step 1: Write the failing test for the new helper**

Append to `scripts/clipimg.test.mjs`:

```js
import { b64ToBytes } from '../src/lib/clipimg.js'

test('base64 → 字节数组（剪贴板图片与截图共用的转换）', () => {
  // "AQID" = [1,2,3]
  assert.deepEqual(Array.from(b64ToBytes('AQID')), [1, 2, 3])
  assert.deepEqual(Array.from(b64ToBytes('')), [])
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test scripts/clipimg.test.mjs`
Expected: FAIL — `b64ToBytes is not a function`.

- [ ] **Step 3: Implement the helper**

In `src/lib/clipimg.js` add:

```js
/** base64 → Uint8Array（写剪贴板图片时给 Tauri 的 Image.fromBytes 用） */
export function b64ToBytes(b64) {
  const bin = atob(b64 || '')
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return bytes
}
```

- [ ] **Step 4: Implement composite + confirm in the overlay**

In `src/components/ScreenshotOverlay.vue`:

```js
import { t } from '../lib/i18n'
import { save as saveDialog } from '@tauri-apps/plugin-dialog'

/** 合成导出：按选区把底图 + 标注裁剪成 PNG base64（在图像物理像素上裁剪） */
function compositeB64() {
  const r = cssRectToImageRect(sel.value, slice.value, winRect.value.w)
  const out = document.createElement('canvas')
  out.width = r.w
  out.height = r.h
  const ctx = out.getContext('2d')
  ctx.drawImage(img.value, r.x, r.y, r.w, r.h, 0, 0, r.w, r.h)
  // 标注层按同一比例缩放贴上去（在物理像素上重绘，避免放大糊掉）
  const anno = annoCanvas.value
  if (anno) {
    ctx.imageSmoothingEnabled = false
    ctx.drawImage(
      anno,
      sel.value.x, sel.value.y, sel.value.w, sel.value.h,
      0, 0, r.w, r.h,
    )
  }
  const url = out.toDataURL('image/png')
  return { b64: url.slice(url.indexOf(',') + 1), width: r.w, height: r.h }
}

async function confirm() {
  if (!canOk.value || busy.value) return
  busy.value = true
  try {
    const { b64, width, height } = compositeB64()
    const bytes = Math.floor((b64.length * 3) / 4)
    if (bytes > 32 * 1024 * 1024) throw new Error(t('shot.tooLarge'))
    await ipc.emitToMain(ipc.EVT.screenshotDone, { b64, mime: 'image/png', size: bytes, width, height })
    await ipc.closeShotOverlays(session)
  } catch (e) {
    errMsg.value = String(e?.message || e)
  } finally {
    busy.value = false
  }
}

async function copyOnly() {
  if (!canOk.value || busy.value) return
  busy.value = true
  try {
    const { b64, width, height } = compositeB64()
    await ipc.emitToMain(ipc.EVT.screenshotCopy, { b64, mime: 'image/png', size: Math.floor((b64.length * 3) / 4), width, height })
    await ipc.closeShotOverlays(session)
  } catch (e) {
    errMsg.value = String(e?.message || e)
  } finally {
    busy.value = false
  }
}

/** 保存：不关闭遮罩，方便继续调整后再发 */
async function saveAs() {
  if (!canOk.value) return
  try {
    const { b64 } = compositeB64()
    const base = `screenshot-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, '')}.png`
    const path = await saveDialog({ defaultPath: base, filters: [{ name: 'PNG', extensions: ['png'] }] })
    if (!path) return
    await ipc.saveShotPng(b64, path)
  } catch (e) {
    errMsg.value = String(e?.message || e)
  }
}
```

Add `emitToMain` to `src/lib/ipc.js`:

```js
/** 从独立窗口（遮罩/查看器）向所有窗口广播事件 */
export const emitToMain = (event, payload) => emit(event, payload)
```

with `import { emit } from '@tauri-apps/api/event'` at the top of `ipc.js`.

Extend the overlay toolbar with the action group (right side of the same bar):

```html
      <span class="sep"></span>
      <button :title="t('shot.copy')" @click="copyOnly">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5h10" />
        </svg>
      </button>
      <button :title="t('shot.save')" @click="saveAs">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <path d="M12 4v10m0 0l-4-4m4 4l4-4M5 19h14" />
        </svg>
      </button>
      <button class="ok" :disabled="!canOk" :title="t('shot.confirm')" @click="confirm">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M5 13l4 4L19 7" />
        </svg>
      </button>
      <button :title="t('shot.cancel')" @click="cancel">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M6 6l12 12M18 6L6 18" />
        </svg>
      </button>
```

The `confirm()` above **replaces** the Task 7 stub (which only set a hint), and `cancel()` / `onKeydown` stay as they are. Delete the now-dead scaffolding from Task 7: the `const hint = ref('')` declaration, the `hint.value = t('shot.todoConfirm')` line, the `<div v-if="hint" class="tip bottom">` template block, and the `shot.todoConfirm` key from both languages in `src/lib/i18n.js`.

Also add to the styles: `.toolbar .ok { color:#1aad19 }`.

- [ ] **Step 5: Wire the main window**

In `src/components/ChatWindow.vue`:

1. Add the toolbar button after the folder button:

```html
        <button :title="t('chat.screenshot')" @click="startShot">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M4 8V6a2 2 0 0 1 2-2h2M16 4h2a2 2 0 0 1 2 2v2M20 16v2a2 2 0 0 1-2 2h-2M8 20H6a2 2 0 0 1-2-2v-2"
              stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
            <path d="M8 10l3 3 2-2 3 3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
```

2. Add the handlers and the listener registration (inside `onMounted`, next to the existing `unlistenDrop`, and cleaned up in `onUnmounted`):

```js
let unlistenShot = null

/** 触发截图；后端抓屏并打开遮罩窗口 */
async function startShot() {
  try {
    await ipc.startScreenshot()
  } catch (e) {
    alert(shotErrorText(e))
  }
}

/** 后端错误串形如 "ERROR_CODE|文案"；有文案就直接给用户看 */
function shotErrorText(e) {
  const s = String(e?.message || e)
  const i = s.indexOf('|')
  return i > 0 ? s.slice(i + 1) : s
}

/** 把截图按「粘贴图片」的同一形状放进待发送列表 */
function pushShotImage({ b64, mime = 'image/png', size }) {
  const p = pendingImgFromB64(b64, mime, size)
  pendingOf(store.activeKey).push({
    kind: 'img', b64: p.b64, mime: p.mime, size: p.size,
    url: URL.createObjectURL(p.blob), name: imgItemName(p.mime),
  })
  nextTick(() => ta.value?.focus())
}

async function copyShotToClipboard(b64) {
  try {
    await ipc.copyShotImage(b64)
  } catch (e) {
    // Linux 之外由插件写图片
    if (String(e) === 'PLUGIN') {
      const { writeImage } = await import('@tauri-apps/plugin-clipboard-manager')
      const { Image } = await import('@tauri-apps/api/image')
      await writeImage(Image.fromBytes(b64ToBytes(b64)))
    } else {
      throw e
    }
  }
}
```

and in `onMounted`:

```js
  unlistenShot = await ipc.listenEvent(ipc.EVT.screenshotDone, async (p) => {
    if (!store.activeKey) {
      // 没有打开会话：不静默丢弃 —— 复制到剪贴板并提示
      try { await copyShotToClipboard(p.b64) } catch (e) { console.error('copy shot failed', e) }
      toast(t('chat.shotNoChat'))
      return
    }
    pushShotImage(p)
    if (store.config?.shot_copy_clipboard !== false) {
      try { await copyShotToClipboard(p.b64) } catch (e) { console.error('copy shot failed', e) }
    }
  })

  unlistenShotCopy = await ipc.listenEvent(ipc.EVT.screenshotCopy, async (p) => {
    try { await copyShotToClipboard(p.b64) } catch (e) { alert(t('chat.shotCopyFailed', { e })) }
  })
```

with matching `unlistenShot?.()` / `unlistenShotCopy?.()` cleanup in `onUnmounted`, `import { b64ToBytes } from '../lib/clipimg'` extended at the top, and a `let unlistenShotCopy = null` declaration.

`store.config.shot_copy_clipboard` is produced by Task 10; until then `undefined !== false` keeps copying enabled, which is the intended default.

- [ ] **Step 6: Add i18n strings**

In `src/lib/i18n.js`, add to **both** languages:

```js
  // zh
  'chat.screenshot': '截图',
  'shot.copy': '复制', 'shot.save': '保存', 'shot.confirm': '确认', 'shot.cancel': '取消',
  'shot.tooLarge': '截图过大（超过 32MB），无法发送',
  'chat.shotNoChat': '请先打开一个会话（截图已复制到剪贴板）',
  'chat.shotCopyFailed': '复制到剪贴板失败：{e}',
  // en
  'chat.screenshot': 'Screenshot',
  'shot.copy': 'Copy', 'shot.save': 'Save', 'shot.confirm': 'Confirm', 'shot.cancel': 'Cancel',
  'shot.tooLarge': 'Screenshot too large (over 32MB)',
  'chat.shotNoChat': 'Open a chat first (screenshot copied to clipboard)',
  'chat.shotCopyFailed': 'Copy to clipboard failed: {e}',
```

- [ ] **Step 7: Run the full test suite**

Run: `pnpm test`
Expected: PASS — every existing test plus the new ones.

- [ ] **Step 8: End-to-end manual verification**

Run: `pnpm tauri dev`, then:
1. Click the new 截图 toolbar button → overlays appear.
2. Select a region, confirm with ✓ → an `img` pending item with the correct thumbnail appears in the composer.
3. Paste into any text field (Ctrl+V) → the same image pastes (clipboard write works).
4. Press Enter → the message sends; the receiving client shows the same image.
5. Trigger again, annotate, press ✓ → the annotations are baked into the sent image at full resolution (no blurring).
6. Trigger again with no chat open → toast appears and the image is on the clipboard.
7. Press Esc / right-click → overlays close, nothing is added to the pending list.

- [ ] **Step 9: Commit**

```bash
git add src/components/ScreenshotOverlay.vue src/components/ChatWindow.vue src/lib/ipc.js src/lib/clipimg.js src/lib/i18n.js scripts/clipimg.test.mjs
git commit -m "feat(shot): 确认链路（合成→待发送列表→剪贴板）与工具栏入口"
```

---

### Task 10: Configuration and settings UI

**Files:**
- Modify: `src-tauri/src/state.rs` (Config + patch)
- Modify: `src-tauri/src/lib.rs` (`save_config` mapping)
- Modify: `src/components/SettingsModal.vue`
- Modify: `src/lib/i18n.js`
- Test: `scripts/settings-ui.test.mjs`

**Interfaces:**
- Consumes: nothing new.
- Produces: config fields `shot_hotkey: String` (default `"Alt+A"`) and `shot_copy_clipboard: bool` (default `true`), exposed to the frontend through `get_config` and writable through `save_config`.

- [ ] **Step 1: Write the failing test**

Append to `scripts/hotkey.test.mjs` (the `isWaylandUA` helper is introduced in this task):

```js
import { isWaylandUA } from '../src/lib/hotkey.js'

test('Wayland 会话粗判只看显式 Wayland 标记', () => {
  assert.equal(isWaylandUA('Mozilla/5.0 (X11; Linux x86_64)'), false)
  assert.equal(isWaylandUA('Mozilla/5.0 (Wayland; Linux x86_64)'), true)
  assert.equal(isWaylandUA(''), false)
})
```

Append to `scripts/settings-ui.test.mjs`:

```js
test('截图设置分区包含热键录制与自动复制开关', () => {
  assert.match(source, /shot_hotkey/, '设置页必须能改截图热键')
  assert.match(source, /shot_copy_clipboard/, '设置页必须有「确认后复制到剪贴板」开关')
})

test('截图热键输入框是只读录制控件，不做自由文本输入', () => {
  const css = source.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''
  assert.match(css, /\.hotkey-input/, '热键录制控件需要独立样式（避免与普通输入框混淆）')
  assert.match(source, /readonly/, '热键框必须 readonly，值只能由按键录制写入')
})
```

Also extend the Rust-side test coverage in `src-tauri/src/state.rs` if a `Config` round-trip test exists; otherwise add one in `src-tauri/src/lib.rs` `mod tests`:

```rust
    #[test]
    fn config_defaults_keep_screenshot_settings() {
        let c = crate::state::Config::default();
        assert_eq!(c.shot_hotkey, "Alt+A");
        assert!(c.shot_copy_clipboard);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pnpm test` → FAIL (`shot_hotkey` not found in SettingsModal).
Run: `cd src-tauri && cargo test config_defaults_keep_screenshot_settings` → FAIL (`no field shot_hotkey`).

- [ ] **Step 3: Add the config fields**

In `src-tauri/src/state.rs`, add to `struct Config` (after `v6_mcast`):

```rust
    /// 截图全局热键（规范形，如 "Alt+A"）；Wayland 下作为 portal 的首选触发器
    #[serde(default = "default_shot_hotkey")]
    pub shot_hotkey: String,
    /// 截图确认后是否自动复制到剪贴板（微信习惯：默认开）
    #[serde(default = "default_true")]
    pub shot_copy_clipboard: bool,
```

with

```rust
fn default_shot_hotkey() -> String {
    "Alt+A".into()
}
```

and the same two lines in `impl Default for Config`.

Add the fields to `ConfigPatch` — it is a **private struct in `src-tauri/src/lib.rs` around line 117** (not in `state.rs`), whose fields are plain (no `pub`) — as `shot_hotkey: Option<String>` and `shot_copy_clipboard: Option<bool>`, and map them in `save_config` in the same file:

```rust
        // 截图热键：补丁未携带保留现值；空串视为「不注册全局热键」
        shot_hotkey: patch.shot_hotkey.unwrap_or(prev.shot_hotkey),
        shot_copy_clipboard: patch.shot_copy_clipboard.unwrap_or(prev.shot_copy_clipboard),
```

- [ ] **Step 4: Add the settings section**

In `src/components/SettingsModal.vue`, add a screenshot section (follow the existing section markup pattern in that file) with:

```html
      <section class="si-section">
        <h3 class="si-title">{{ t('settings.shot') }}</h3>
        <div class="si-row">
          <label>{{ t('settings.shotHotkey') }}</label>
          <input
            class="hotkey-input"
            readonly
            :value="form.shot_hotkey || ''"
            :placeholder="t('settings.shotHotkeyPh')"
            @keydown="onHotkeyKeydown"
            @focus="recording = true"
            @blur="recording = false"
          />
        </div>
        <div class="si-row">
          <label>{{ t('settings.shotCopy') }}</label>
          <input type="checkbox" v-model="form.shot_copy_clipboard" />
        </div>
        <p class="si-hint">{{ hotkeyHint }}</p>
      </section>
```

script additions:

```js
import { comboFromEvent, isValidCombo, isWaylandUA } from '../lib/hotkey'

const recording = ref(false)

/** 录制：按下的组合键直接写进表单；Esc 清空（= 不注册全局热键） */
function onHotkeyKeydown(e) {
  e.preventDefault()
  if (e.key === 'Escape') {
    form.shot_hotkey = ''
    return
  }
  const combo = comboFromEvent(e)
  if (combo) form.shot_hotkey = combo
}

/** 配置里可能存着一个不可用的值（手改配置 / 跨平台拷贝）—— 明确告诉用户它不会生效 */
const hotkeyHint = computed(() => {
  if (form.shot_hotkey && !isValidCombo(form.shot_hotkey)) return t('settings.shotHotkeyInvalid')
  return isWaylandUA(navigator.userAgent)
    ? t('settings.shotHotkeyWayland')
    : t('settings.shotHotkeyHint')
})
```

`isWaylandUA` lives in `src/lib/hotkey.js` so it stays testable:

```js
/** 粗判 Wayland 会话：WebKitGTK 在 XWayland 下 UA 也可能含 X11，故只认显式 Wayland */
export function isWaylandUA(ua = '') {
  return /wayland/i.test(ua)
}
```

Add the styles:

```css
.hotkey-input {
  width: 160px;
  padding: 4px 8px;
  border: 1px solid var(--line);
  border-radius: 4px;
  background: var(--bg-soft);
  color: var(--fg);
  text-align: center;
  cursor: pointer;
}
```

`form` must include the two new fields wherever `form` is initialised from `store.config` and wherever the save patch is built.

- [ ] **Step 5: Add i18n strings**

In `src/lib/i18n.js`, both languages:

```js
  // zh
  'settings.shot': '截图',
  'settings.shotHotkey': '截图快捷键',
  'settings.shotHotkeyPh': '点击后按下组合键',
  'settings.shotHotkeyHint': '默认 Alt+A；点进输入框后直接按组合键即可修改，Esc 清空表示不注册',
  'settings.shotHotkeyInvalid': '当前保存的快捷键不可用（缺少修饰键），请重新录制或清空',
  'settings.shotHotkeyWayland': '当前是 Wayland 会话：首次保存会弹出系统绑定确认；若桌面环境不支持，可用命令行 open-ipmsg --screenshot 自行绑定快捷键',
  'settings.shotCopy': '确认后复制到剪贴板',
  // en
  'settings.shot': 'Screenshot',
  'settings.shotHotkey': 'Screenshot shortcut',
  'settings.shotHotkeyPh': 'Click, then press keys',
  'settings.shotHotkeyHint': 'Default Alt+A. Click the field and press a combination; Esc clears it (no global shortcut)',
  'settings.shotHotkeyInvalid': 'The saved shortcut is unusable (no modifier key) — record a new one or clear it',
  'settings.shotHotkeyWayland': 'Wayland session: the first save asks the desktop to bind the shortcut. If your desktop does not support it, bind "open-ipmsg --screenshot" yourself',
  'settings.shotCopy': 'Copy to clipboard on confirm',
```

Both language blocks must contain exactly the same key set — the i18n test fails otherwise.

- [ ] **Step 6: Run tests**

Run: `pnpm test` and `cd src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 7: Verify by hand**

Run: `pnpm tauri dev` → Settings → 截图: press `Ctrl+Shift+X` in the hotkey field → shows `Ctrl+Shift+X`; save; reopen settings → value persisted; `config.json` in the app data dir contains `shot_hotkey`/`shot_copy_clipboard`; toggling the copy checkbox off and taking a screenshot leaves the clipboard untouched.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/lib.rs src/components/SettingsModal.vue src/lib/hotkey.js src/lib/i18n.js scripts/settings-ui.test.mjs scripts/hotkey.test.mjs
git commit -m "feat(shot): 截图配置项与设置页分区（热键录制 / 自动复制开关）"
```

---

### Task 11: Global hotkey (plugin + Wayland portal + CLI fallback)

**Files:**
- Create: `src-tauri/src/shortcut.rs`
- Modify: `src-tauri/Cargo.toml` (`tauri-plugin-global-shortcut`)
- Modify: `src-tauri/src/lib.rs` (module, plugin init, setup registration, single-instance `--screenshot`)

**Interfaces:**
- Consumes: `screenshot::trigger`, `screenshot::is_wayland`, `screenshot::ShotErr`, `toPortalTrigger` semantics from Task 3.
- Produces: `pub fn register(app: &tauri::AppHandle, combo: &str) -> Backend` where `pub enum Backend { Plugin, Portal, Disabled }`; `pub fn to_portal_trigger(combo: &str) -> Option<String>`; `--screenshot` handled in the single-instance callback.

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/shortcut.rs` with the test module first:

```rust
//! 全局热键：Windows/macOS/X11 用 tauri-plugin-global-shortcut，Wayland 走
//! org.freedesktop.portal.GlobalShortcuts（Tauri 插件在 Wayland 上无效，
//! 注册只会产生误导性的「成功」）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_trigger_follows_xdg_shortcuts_spec() {
        assert_eq!(to_portal_trigger("Alt+A").as_deref(), Some("ALT+a"));
        assert_eq!(to_portal_trigger("Ctrl+Shift+S").as_deref(), Some("CTRL+SHIFT+s"));
        // Super/Windows → LOGO，多修饰键按字母序
        assert_eq!(to_portal_trigger("CmdOrCtrl+Alt+A").as_deref(), Some("ALT+LOGO+a"));
        // 主键用 xkbcommon 键名
        assert_eq!(to_portal_trigger("Ctrl+Enter").as_deref(), Some("CTRL+Return"));
        // 非法组合被拒
        assert_eq!(to_portal_trigger("A"), None);
        assert_eq!(to_portal_trigger(""), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test shortcut::`
Expected: FAIL — `cannot find function to_portal_trigger in this scope`.

- [ ] **Step 3: Implement the module**

Add to `src-tauri/src/shortcut.rs`:

```rust
use tauri::Manager;

#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    /// 平台插件（Windows / macOS / X11）
    Plugin,
    /// Wayland portal GlobalShortcuts
    Portal,
    /// 未注册（热键为空或平台不支持）
    Disabled(&'static str),
}

/// 规范形 "Ctrl+Alt+A" → XDG shortcuts 触发器 "CTRL+ALT+a"
pub fn to_portal_trigger(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let (key, mods) = parts.split_last()?;
    let mut out: Vec<String> = mods
        .iter()
        .map(|m| match *m {
            "CmdOrCtrl" | "Super" | "Meta" => "LOGO".to_string(),
            other => other.to_uppercase(),
        })
        .collect();
    out.sort();
    let key_name = match *key {
        "Enter" => "Return",
        "Space" => "space",
        "Backspace" => "BackSpace",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "PrintScreen" => "Print",
        k if k.len() == 1 => return Some(format!("{}+{}", out.join("+"), k.to_lowercase())),
        k => k,
    };
    Some(format!("{}+{}", out.join("+"), key_name))
}

/// 注册全局热键；返回实际生效的后端
pub fn register(app: &tauri::AppHandle, combo: &str) -> Backend {
    if combo.trim().is_empty() {
        return Backend::Disabled("未配置热键");
    }
    if crate::screenshot::is_wayland() {
        match portal::bind(app, combo) {
            Ok(()) => Backend::Portal,
            Err(e) => {
                oim_log!("[shot] Wayland 热键绑定失败：{e}（可用 --screenshot 自行绑定）");
                Backend::Disabled("当前桌面环境不支持全局热键")
            }
        }
    } else {
        match plugin::register(app, combo) {
            Ok(()) => Backend::Plugin,
            Err(e) => {
                oim_log!("[shot] 全局热键注册失败：{e}");
                Backend::Disabled("热键注册失败（可能被其他程序占用）")
            }
        }
    }
}

/* ---------------- Windows / macOS / X11 ---------------- */

// 三平台共用同一份注册代码：Linux 上插件只在 X11 生效，Wayland 永远走下面的
// portal 分支（见 register 里的 is_wayland 判断），所以这里不需要平台分支。
mod plugin {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    pub fn register(app: &tauri::AppHandle, combo: &str) -> Result<(), String> {
        let shortcut: tauri_plugin_global_shortcut::Shortcut =
            combo.parse().map_err(|e| format!("非法快捷键 {combo}: {e}"))?;
        let handle = app.clone();
        app.global_shortcut()
            .on_shortcut(shortcut, move |_app, _sc, event| {
                if event.state() == ShortcutState::Pressed {
                    if let Err(e) = crate::screenshot::trigger(&handle) {
                        oim_log!("[shot] 热键触发失败：{}", e.message());
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
}

/* ---------------- Wayland: portal GlobalShortcuts ---------------- */

#[cfg(target_os = "linux")]
mod portal {
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const ID: &str = "screenshot";

    /// CreateSession → BindShortcuts → 后台线程监听 Activated
    pub fn bind(app: &tauri::AppHandle, combo: &str) -> Result<(), String> {
        let trigger = super::to_portal_trigger(combo).ok_or("非法快捷键")?;
        let conn = Connection::session().map_err(|e| format!("无法连接会话总线: {e}"))?;

        // 1) 建会话（先订阅 Response 再调用）
        let token = format!("oimsess{}", std::process::id());
        let handle = request_path(&conn, &token)?;
        let req = Proxy::new(&conn, DEST, handle.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| e.to_string())?;
        let mut signals = req.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut opts: HashMap<&str, Value> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("session_handle_token", Value::from(token.as_str()));
        let gs = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts")
            .map_err(|e| e.to_string())?;
        let _: OwnedObjectPath = gs
            .call("CreateSession", &(opts))
            .map_err(|e| format!("PORTAL_MISSING: {e}"))?;
        let (code, results) = next_response(&mut signals)?;
        if code != 0 {
            return Err(format!("创建快捷键会话被拒绝（响应码 {code}）"));
        }
        let session: String = results
            .get("session_handle")
            .ok_or("响应里没有 session_handle")?
            .try_into()
            .map_err(|_| "session_handle 不是字符串")?;

        // 2) 绑定快捷键（KDE/GNOME 会弹一次确认框）
        let token2 = format!("oimbind{}", std::process::id());
        let handle2 = request_path(&conn, &token2)?;
        let req2 = Proxy::new(&conn, DEST, handle2.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| e.to_string())?;
        let mut signals2 = req2.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut sc_opts: HashMap<&str, Value> = HashMap::new();
        sc_opts.insert("description", Value::from("截图"));
        sc_opts.insert("preferred_trigger", Value::from(trigger.as_str()));
        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = vec![(ID, sc_opts)];
        let mut bind_opts: HashMap<&str, Value> = HashMap::new();
        bind_opts.insert("handle_token", Value::from(token2.as_str()));
        let _: OwnedObjectPath = gs
            .call("BindShortcuts", &(session.as_str(), shortcuts, "", bind_opts))
            .map_err(|e| format!("绑定快捷键失败: {e}"))?;
        let (code2, res2) = next_response(&mut signals2)?;
        if code2 != 0 {
            return Err(format!("快捷键绑定被拒绝（响应码 {code2}）"));
        }
        oim_log!("[shot] Wayland 热键已绑定：{combo} → {trigger}（响应 {res2:?}）");

        // 3) 监听 Activated：收到就触发截图，直到进程退出
        let app2 = app.clone();
        std::thread::spawn(move || {
            let Ok(conn) = Connection::session() else { return };
            let Ok(proxy) = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts")
            else {
                return;
            };
            let Ok(mut sig) = proxy.receive_signal("Activated") else { return };
            for msg in &mut sig {
                let Ok((_session, id, _ts, _opts)): Result<
                    (String, String, u64, HashMap<String, OwnedValue>),
                    _,
                > = msg.body().deserialize()
                else {
                    continue;
                };
                if id == ID {
                    if let Err(e) = crate::screenshot::trigger(&app2) {
                        oim_log!("[shot] 热键触发失败：{}", e.message());
                    }
                }
            }
        });
        Ok(())
    }

    fn request_path(conn: &Connection, token: &str) -> Result<String, String> {
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .ok_or("会话总线没有唯一名")?;
        Ok(format!("/org/freedesktop/portal/desktop/request/{sender}/{token}"))
    }

    fn next_response(
        signals: &mut zbus::blocking::SignalIterator<'_>,
    ) -> Result<(u32, HashMap<String, OwnedValue>), String> {
        let msg = signals.next().ok_or("portal 未返回响应")?;
        msg.body().deserialize().map_err(|e| e.to_string())
    }
}
```

The interface also offers `ConfigureShortcuts(session_handle, parent_window, options)`, which re-opens the desktop's shortcut configuration UI — it is intentionally **not** wired up in this task (the settings page tells the user to use their desktop's own shortcut settings instead); adding it later only affects the Wayland branch.

- [ ] **Step 4: Wire it up**

In `src-tauri/Cargo.toml`:

```toml
tauri-plugin-global-shortcut = "2"
```

In `src-tauri/src/lib.rs`:

1. `mod shortcut;`
2. In the builder chain, `.plugin(tauri_plugin_global_shortcut::Builder::new().build())`
3. In `.setup(...)`, after `app.manage(screenshot::ShotState::default());`:

```rust
            // 全局热键：Windows/macOS/X11 走插件，Wayland 走 portal（首次会弹系统确认）
            let combo = st.config().shot_hotkey.clone();
            let backend = shortcut::register(handle, &combo);
            oim_log!("[shot] 全局热键后端：{backend:?}（{combo}）");
```

Note `st` is the `Arc<AppState>` created a few lines above in the same closure.

4. In the single-instance callback, handle the CLI trigger:

```rust
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if argv.iter().any(|a| a == "--log") {
                state::set_log_enabled(true);
            }
            // --screenshot：给没有全局热键的环境（如 GNOME < 48 的 Wayland）留的
            // 命令行入口，用户可在桌面环境里把这条命令绑成自定义快捷键
            if argv.iter().any(|a| a == "--screenshot") {
                if let Err(e) = screenshot::trigger(app) {
                    oim_log!("[shot] 命令行触发失败：{}", e.message());
                }
                return;
            }
            oim_log!("[single-instance] 已有实例在运行，唤起既有窗口");
            activate_from_tray(app);
        }))
```

Also handle `--screenshot` when it is the **first** launch (no other instance): in `setup`, after the hotkey registration:

```rust
            if std::env::args().any(|a| a == "--screenshot") {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    if let Err(e) = screenshot::trigger(&h) {
                        oim_log!("[shot] 命令行触发失败：{}", e.message());
                    }
                });
            }
```

- [ ] **Step 5: Run tests**

Run: `cd src-tauri && cargo test shortcut::` → PASS (1 test).
Run: `cd src-tauri && cargo test` → PASS (all).

- [ ] **Step 6: Verify the hotkey by hand on this machine**

Run: `pnpm tauri dev`, then:
1. First launch: a KDE dialog appears asking to bind the screenshot shortcut; accept it.
2. Minimize the app, press `Alt+A` → the overlays appear (the app does not need focus).
3. In Settings change the shortcut to `Ctrl+Shift+X`, save, restart → the new combination works and `Alt+A` no longer does.
4. Clear the shortcut (Esc in the field), save, restart → no global shortcut is registered and no binding dialog appears; the log says `Disabled`.
5. `./target/debug/open-ipmsg --screenshot` while the app is running → overlays appear.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/shortcut.rs src-tauri/Cargo.toml src-tauri/src/lib.rs
git commit -m "feat(shot): 全局热键（插件/portal GlobalShortcuts）与 --screenshot 命令行入口"
```

---

### Task 12: Windows BitBlt capture backend

**Files:**
- Modify: `src-tauri/Cargo.toml` (`windows-sys` features)
- Modify: `src-tauri/src/screenshot.rs` (Windows `platform::capture`)
- Test: pure-function tests already cover `bgra_to_rgba`; add a stride test for odd widths.

**Interfaces:**
- Consumes: Task 4's `bgra_to_rgba`, Task 5's `Captured` / `ShotErr` / `decode_captured` shape.
- Produces: `platform::capture() -> Result<Captured, ShotErr>` on Windows.

- [ ] **Step 1: Add the GDI features**

In `src-tauri/Cargo.toml`, extend the Windows dependency:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_NetworkManagement_IpHelper",
    "Win32_NetworkManagement_Ndis",
    "Win32_Networking_WinSock",
    "Win32_Graphics_Gdi",
    "Win32_UI_WindowsAndMessaging",
] }
```

- [ ] **Step 2: Write the test that pins the stride/odd-width behaviour**

Append to `src-tauri/src/screenshot.rs` tests:

```rust
    #[test]
    fn bgra_to_rgba_handles_odd_width_rows() {
        // 3×1 行宽 12 字节，stride 16（GDI 常见 4 字节对齐）
        let src = vec![
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 0, 0, 0, 0,
        ];
        assert_eq!(
            bgra_to_rgba(&src, 3, 1, 16),
            vec![30, 20, 10, 255, 60, 50, 40, 255, 90, 80, 70, 255],
        );
    }
```

Run: `cd src-tauri && cargo test screenshot::tests::bgra_to_rgba_handles_odd_width_rows`
Expected: PASS on Linux (this is a pure function) — it guards the Windows path before the platform code lands.

- [ ] **Step 3: Implement the Windows backend**

Replace the `#[cfg(not(target_os = "linux"))] mod platform` stub in `src-tauri/src/screenshot.rs` with the Windows backend **plus** a fallback for every other non-Linux platform (Task 13 then adds the macOS arm — keep the fallback so the tree still builds between the two tasks):

```rust
#[cfg(all(not(target_os = "linux"), not(target_os = "windows"), not(target_os = "macos")))]
mod platform {
    use super::ShotErr;

    pub fn capture() -> Result<super::Captured, ShotErr> {
        Err(ShotErr::CaptureFailed("当前平台尚未实现抓屏".into()))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{bgra_to_rgba, Captured, ShotErr};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    pub fn capture() -> Result<Captured, ShotErr> {
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let w = GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32;
            let h = GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32;
            if w == 0 || h == 0 {
                return Err(ShotErr::CaptureFailed("虚拟桌面尺寸为 0".into()));
            }
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
            let old = SelectObject(mem, bmp);
            // CAPTUREBLT 才能抓到分层窗口（否则只有桌面壁纸）
            let ok = BitBlt(mem, 0, 0, w as i32, h as i32, screen, x, y, SRCCOPY | CAPTUREBLT);
            if ok == 0 {
                ReleaseDC(std::ptr::null_mut(), screen);
                DeleteObject(bmp);
                DeleteDC(mem);
                return Err(ShotErr::CaptureFailed("BitBlt 失败".into()));
            }
            let stride = ((w * 32 + 31) / 32 * 4) as usize;
            let mut buf = vec![0u8; stride * h as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // 负高度 = 自顶向下，省掉一次翻转
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let lines = GetDIBits(
                mem,
                bmp,
                0,
                h,
                buf.as_mut_ptr() as *mut _,
                &mut info,
                DIB_RGB_COLORS,
            );
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            if lines != h as i32 {
                return Err(ShotErr::CaptureFailed(format!(
                    "GetDIBits 只写回 {lines} 行（应为 {h} 行）"
                )));
            }
            let mut rgba = bgra_to_rgba(&buf, w, h, stride);
            // BitBlt 到 DIB 的 alpha 字节是未定义的（实测常见为 0）。若原样当成
            // 透明度用，PNG 会是一张全透明图 —— 抓屏必须强制不透明。
            for px in rgba.chunks_exact_mut(4) {
                px[3] = 255;
            }
            encode_png(rgba, w, h)
        }
    }

    fn encode_png(rgba: Vec<u8>, w: u32, h: u32) -> Result<Captured, ShotErr> {
        use image::ImageEncoder;
        let img = image::RgbaImage::from_raw(w, h, rgba)
            .ok_or_else(|| ShotErr::CaptureFailed("缓冲尺寸不匹配".into()))?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| ShotErr::CaptureFailed(format!("PNG 编码失败: {e}")))?;
        Ok(Captured { png, width: w, height: h })
    }
}
```

- [ ] **Step 4: Verify it compiles for the Windows target**

Run: `rustup target add x86_64-pc-windows-gnu` (once), then
`cd src-tauri && cargo check --target x86_64-pc-windows-gnu 2>&1 | tail -20`
Expected: the crate type-checks for Windows. If the GNU target is unavailable in this environment, record that in the handoff checklist as a Windows-machine verification item instead of claiming it compiles.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/screenshot.rs
git commit -m "feat(shot): Windows BitBlt 抓屏后端"
```

---

### Task 13: macOS CoreGraphics capture backend

**Files:**
- Modify: `src-tauri/Cargo.toml` (macOS `core-graphics`)
- Modify: `src-tauri/src/screenshot.rs` (macOS `platform::capture`)

**Interfaces:**
- Consumes: Task 5's `Captured` / `ShotErr`; Task 4's `Rect`.
- Produces: `platform::capture() -> Result<Captured, ShotErr>` on macOS, returning `MAC_PERMISSION` before attempting capture when the screen-recording permission is missing.

- [ ] **Step 1: Add the dependency**

```toml
[target.'cfg(target_os = "macos")'.dependencies]
core-graphics = "0.24"
```

- [ ] **Step 2: Implement the backend**

Add to `src-tauri/src/screenshot.rs`:

```rust
#[cfg(target_os = "macos")]
mod platform {
    use super::{Captured, ShotErr};
    use core_graphics::display::CGDisplay;

    /// 未授权时 CGDisplayCreateImage 会静默返回一张只有桌面壁纸的图 ——
    /// 这是最难排查的失败形态，所以先做权限预检并给出明确文案。
    pub fn capture() -> Result<Captured, ShotErr> {
        if !has_permission() {
            return Err(ShotErr::MacPermission);
        }
        let ids = CGDisplay::active_displays().map_err(|e| ShotErr::CaptureFailed(format!("{e:?}")))?;
        // 主屏尺寸决定画布：多屏按 bounds 的并集拼接
        let mut shots = Vec::new();
        let mut min_x = i64::MAX;
        let mut min_y = i64::MAX;
        let mut max_x = i64::MIN;
        let mut max_y = i64::MIN;
        for id in ids {
            let display = CGDisplay::new(id);
            let b = display.bounds();
            min_x = min_x.min(b.origin.x as i64);
            min_y = min_y.min(b.origin.y as i64);
            max_x = max_x.max((b.origin.x + b.size.width) as i64);
            max_y = max_y.max((b.origin.y + b.size.height) as i64);
            let img = display
                .image()
                .ok_or_else(|| ShotErr::CaptureFailed(format!("抓取显示器 {id} 失败")))?;
            shots.push((b, img));
        }
        if shots.is_empty() {
            return Err(ShotErr::CaptureFailed("没有可用显示器".into()));
        }
        let scale = shots
            .first()
            .map(|(_, img)| img.width() as f64 / img.bounds().size.width.max(1.0))
            .unwrap_or(1.0);
        let w = ((max_x - min_x) as f64 * scale).round() as u32;
        let h = ((max_y - min_y) as f64 * scale).round() as u32;
        let mut canvas = image::RgbaImage::new(w, h);
        for (b, img) in shots {
            let sw = img.width();
            let sh = img.height();
            let mut data = vec![0u8; sw * sh * 4];
            let ctx = core_graphics::context::CGContext::create_bitmap_context(
                Some(data.as_mut_ptr() as *mut _),
                sw,
                sh,
                8,
                sw * 4,
                &core_graphics::color_space::CGColorSpace::create_device_rgb(),
                core_graphics::base::kCGImageAlphaPremultipliedLast,
            );
            ctx.draw_image(
                core_graphics::geometry::CGRect::new(
                    &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                    &core_graphics::geometry::CGSize::new(sw as f64, sh as f64),
                ),
                &img,
            );
            let ox = ((b.origin.x as i64 - min_x) as f64 * scale).round() as i64;
            let oy = ((b.origin.y as i64 - min_y) as f64 * scale).round() as i64;
            for y in 0..sh {
                for x in 0..sw {
                    let si = (y * sw + x) * 4;
                    let dx = ox + x as i64;
                    let dy = oy + y as i64;
                    if dx < 0 || dy < 0 || dx >= w as i64 || dy >= h as i64 {
                        continue;
                    }
                    canvas.put_pixel(
                        dx as u32,
                        dy as u32,
                        image::Rgba([data[si], data[si + 1], data[si + 2], data[si + 3]]),
                    );
                }
            }
        }
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(canvas.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| ShotErr::CaptureFailed(format!("PNG 编码失败: {e}")))?;
        Ok(Captured { png, width: w, height: h })
    }

    /// macOS 的 TCC 屏幕录制权限预检。
    ///
    /// `core-graphics` 0.24 不导出这个符号，直接声明即可（系统框架自 macOS 10.15 起提供）。
    fn has_permission() -> bool {
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        unsafe { CGPreflightScreenCaptureAccess() }
    }
}
```

Do not substitute a "return true" placeholder here: without the preflight check macOS silently returns a wallpaper-only image, which is the failure mode this guard exists to prevent.

- [ ] **Step 3: Verify it compiles for the macOS target (best effort)**

Run: `rustup target add aarch64-apple-darwin` then `cd src-tauri && cargo check --target aarch64-apple-darwin 2>&1 | tail -20`.
Expected: type-checks. If the target cannot be installed here (needs the macOS SDK for linking, though `cargo check` may still work), record it as a macOS-machine verification item — do not claim it was verified.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/screenshot.rs
git commit -m "feat(shot): macOS CoreGraphics 抓屏后端（含屏幕录制权限预检）"
```

---

### Task 14: Documentation, roadmap, and final verification

**Files:**
- Modify: `README.md`, `README.zh-CN.md`
- Modify: `docs/superpowers/specs/2026-09-13-screenshot-design.md` (record the as-built deltas, if any)

**Interfaces:**
- Consumes: everything above.
- Produces: user-facing documentation and a recorded verification result.

- [ ] **Step 1: Update the feature tables**

In `README.zh-CN.md`, add a row to the 功能 table and tick the roadmap item:

```markdown
| 截图发送 | 工具栏按钮 / 全局热键触发，拖拽选区 + 标注（矩形/椭圆/箭头/画笔/文字/马赛克），确认后进待发送列表 |
```

```markdown
- [x] 开机自启、截图发送
```

If 开机自启 is still not implemented, split the line instead of ticking it falsely:

```markdown
- [x] 截图发送
- [ ] 开机自启
```

Mirror both edits in `README.md` (English).

Add a short 常见问题 entry:

```markdown
**Q:截图热键按了没反应?**
Windows/macOS/X11 由应用注册全局热键；Wayland 下改由桌面环境授权绑定（首次保存设置时会弹确认框），若你的桌面环境不支持，可在系统设置里把命令 `open-ipmsg --screenshot` 绑成自定义快捷键。抓屏本身在 Linux 走 xdg-desktop-portal；若系统没有该服务，截图会提示「系统未提供截图服务」。
```

- [ ] **Step 2: Record the as-built verification**

Append a short 实现记录 section to the spec listing anything that differed from the design (for example: measured capture latency, the `--shot-test` diagnostic, any platform backend that could not be verified on this machine), then commit.

- [ ] **Step 3: Run everything**

```bash
pnpm test
cd src-tauri && cargo test
cd .. && pnpm build
```

Expected: all Node tests pass, all Rust tests pass, the frontend builds with no errors.

- [ ] **Step 4: Final end-to-end pass on this machine (KDE Wayland, dual monitor)**

1. `pnpm tauri dev`
2. Toolbar 截图 → both monitors get an aligned overlay → select on the **secondary** monitor → annotate → ✓ → pending thumbnail → Enter → peer receives the same pixels.
3. `Alt+A` with the app unfocused → same flow.
4. Esc and right-click cancel cleanly; a second trigger works (no stale session).
5. Close the main window to the tray, then trigger via `Alt+A`, select, confirm → the image still lands in the pending list of the active chat (events must not depend on the main window being visible).
6. `--shot-test` prints the workspace size and writes a valid PNG.
7. X11 session (`plasma-x11` or another X11 session): one overlay spanning both monitors, cross-monitor drag works.
8. Notification still works after the zbus change (this is the regression the feature most risks): trigger an incoming message while unfocused and confirm the system notification appears and is clickable.

- [ ] **Step 5: Commit**

```bash
git add README.md README.zh-CN.md docs/superpowers/specs/2026-09-13-screenshot-design.md
git commit -m "docs(shot): 截图功能说明、FAQ 与实现记录"
```

---

## Handoff checklist for the maintainer (cannot be verified on this machine)

- [ ] Windows: capture is pixel-correct at 100% / 125% / 150% scaling and with two monitors; cross-monitor drag selection works.
- [ ] Windows: `Alt+A` works while another app is focused; changing the shortcut in Settings takes effect after restart.
- [ ] macOS: first capture triggers the 屏幕录制 permission prompt; denying it shows the guidance message instead of a blank screenshot.
- [ ] macOS: multi-display capture is stitched correctly (Retina scale handled).
- [ ] Both: clipboard paste of the confirmed screenshot works in another application.
