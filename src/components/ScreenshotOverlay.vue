<script setup>
// 截图遮罩窗口：底图 + 变暗挖洞 + 拖拽选区 + 标注层（矩形/椭圆/箭头/画笔/文字/马赛克）。
// 「确认/复制/另存为」在这里把选区 + 标注合成 PNG 导出（确认/复制回主窗口，另存为落盘）。
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import * as ipc from '../lib/ipc'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle, resizeRect, moveRect, nudgeRect,
  cssRectToImageRect, toolbarPlacement, mosaicBlocks, arrowHead, pushUndo,
} from '../lib/shot'
import { t } from '../lib/i18n'
import { save as saveDialog } from '@tauri-apps/plugin-dialog'

const boot = window.__OIM_SHOT__ || {}
const qs = new URLSearchParams(location.search)
const session = boot.session || qs.get('session') || ''
const index = Number(boot.index ?? qs.get('i') ?? 0)

const root = ref(null)
const baseCanvas = ref(null)
const annoCanvas = ref(null)   // 标注层：与窗口同尺寸的独立 canvas（Task 9 合成时贴到选区上）
const img = ref(null)          // 已解码的整幅图像（Image 对象）
const slice = ref({ x: 0, y: 0, w: 1, h: 1 })
const sel = ref(null)          // 当前选区（CSS 像素），null = 未选
const busy = ref(false)
const errMsg = ref('')
// 底图是否已经画上。遮罩窗口是透明窗，而且建出来就是映射状态（GTK 未映射的窗口里
// requestAnimationFrame 一次都不会触发，页面没法在隐藏状态下等「帧已提交」）。
// 「不闪」靠的是：底图画上来之前页面里**没有任何不透明的东西** —— 压暗层与提示
// 都挂在这个标志上，于是它们与冻结的桌面图在同一帧出现。
const painted = ref(false)

let drag = null                // { mode:'new'|'move'|'resize', handle, start, origin }

// 窗口尺寸必须是响应式的：computed 直接读 DOM（clientWidth）只会求值一次，
// 之后 resize 既不重算也不触发重绘 —— 而 Wayland 上遮罩是先建成 320x200
// 再全屏的，首次绘制会与那次 resize 竞争，赢在错误的一侧就会把整层画进角落。
const winW = ref(window.innerWidth)
const winH = ref(window.innerHeight)
const winRect = computed(() => ({ x: 0, y: 0, w: winW.value, h: winH.value }))

/** 唯一缩放来源：图像物理像素 ÷ 窗口 CSS 像素。
 *  标注层后备像素、马赛克采样与导出裁剪必须同源 —— 之前三处各算一次，
 *  导出那处还用了四舍五入后的 r.w/sel.w，导致标注层被读偏几十像素。 */
const kWin = computed(() => slice.value.w / Math.max(1, winRect.value.w))

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

/* ---------------- 标注层（工具 / 颜色 / 线宽 / 撤销栈） ---------------- */

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

/** 工具栏贴合：优先选区下方，放不下翻到上方，最后夹进窗口。
 *  尺寸必须实测：工具栏加了确认组之后更宽，写死的宽度会让右端的按钮被夹出窗口；
 *  切到马赛克会再多出三个块尺寸按钮，所以每次工具/块尺寸/选区变化后都要重新量。 */
const barW = ref(560)
const barH = ref(40)
const barRef = ref(null)

function measureBar() {
  const el = barRef.value
  if (!el) return
  barW.value = el.offsetWidth
  barH.value = el.offsetHeight
}

const barStyle = computed(() => {
  const p = toolbarPlacement(sel.value || { x: 0, y: 0, w: 0, h: 0 }, winRect.value, {
    w: barW.value,
    h: barH.value,
  })
  return { left: p.x + 'px', top: p.y + 'px' }
})

/** 标注 canvas：后备像素 = 窗口 CSS 尺寸 × kWin（与底图同为图像物理像素），
 *  再用 ctx 变换把绘制坐标保持在 CSS 空间 —— 标注与底图一样 1:1 清晰，
 *  不会在分数缩放/高分屏上被放大糊掉。 */
function paintAnnoSize() {
  const c = annoCanvas.value
  if (!c) return
  const k = kWin.value
  const w = Math.max(1, Math.round(winRect.value.w * k))
  const h = Math.max(1, Math.round(winRect.value.h * k))
  if (c.width === w && c.height === h) return
  const keep = c.width && c.height ? c.toDataURL() : ''
  c.width = w
  c.height = h
  // 元素仍按 CSS 尺寸布局，绘制坐标继续用 CSS 像素
  c.style.width = winRect.value.w + 'px'
  c.style.height = winRect.value.h + 'px'
  const ctx = c.getContext('2d')
  ctx.setTransform(k, 0, 0, k, 0, 0)
  if (keep) {
    const im = new Image()
    im.onload = () => {
      // 还原是设备像素 1:1 拷贝，必须临时去掉变换
      ctx.save()
      ctx.setTransform(1, 0, 0, 1, 0, 0)
      ctx.drawImage(im, 0, 0)
      ctx.restore()
    }
    im.src = keep
  }
}

function snapshot() {
  const c = annoCanvas.value
  if (!c) return null
  return c.getContext('2d').getImageData(0, 0, c.width, c.height)
}

/** 撤销栈：64MiB 是**预算上限**（cap），不是历史下限 —— 快照是设备分辨率的
 *  ImageData（本机 2560×1440 每张约 14MB，4K 屏单张就有 33MB），只按 pushUndo
 *  的 20 张条数上限会一直吃到几百 MB。
 *  从最旧的一端一直丢到总量进预算为止；但撤销是标注工具的核心操作，
 *  4K 单张 33MB 时 64MiB 只够一步、等于没有，故**至少保留最新 3 步**，
 *  即使因此超出预算。 */
const UNDO_BYTES = 64 * 1024 * 1024
const UNDO_MIN_STEPS = 3

function trimUndo() {
  const stack = undoStack.value
  let total = 0
  for (const s of stack) total += (s?.width || 0) * (s?.height || 0) * 4
  let drop = 0
  // 丢最旧的一张直到进预算；`stack.length - drop > 3` 是步数下限，超预算也认
  while (total > UNDO_BYTES && stack.length - drop > UNDO_MIN_STEPS) {
    const s = stack[drop]
    total -= (s?.width || 0) * (s?.height || 0) * 4
    drop++
  }
  if (drop) undoStack.value = stack.slice(drop)
}

function pushSnapshot() {
  undoStack.value = pushUndo(undoStack.value, snapshot(), 20)
  trimUndo()
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
  // 裁剪到选区：屏幕上看得到的笔迹，导出裁剪里必须也在（否则「画了却导不出」）
  ctx.save()
  if (sel.value) {
    ctx.beginPath()
    ctx.rect(sel.value.x, sel.value.y, sel.value.w, sel.value.h)
    ctx.clip()
  }
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
  ctx.restore()
}

/** 马赛克：读底图对应区域的像素，按块平均后回填。
 *
 *  坐标系：标注层与选区是 CSS 像素，而 base 画布的后备像素是图像物理像素
 *  （k = slice.w / 窗口 CSS 宽，本机 1.25）。采样必须乘 k；回填也走设备像素，
 *  否则每个块都取到图上别处的像素，块网格也会与拖拽范围错位。
 *
 *  每次调用只整片读一次底图（原先每块一次 getImageData，拖拽时每帧几十次读回，
 *  既慢又让同一帧里的块取到不同时刻的像素）；块的设备像素对齐算法保持不变。 */
function applyMosaic(ctx, cssRect) {
  const base = baseCanvas.value
  if (!base) return
  const bctx = base.getContext('2d')
  const k = kWin.value
  // 块的设备像素矩形：与旧实现逐字一致，只把「逐块读取」换成「一次读取 + 按块平均」
  const blocks = []
  for (const b of mosaicBlocks(cssRect, blockSize.value)) {
    const dx = Math.round(b.x * k)
    const dy = Math.round(b.y * k)
    const dw = Math.max(1, Math.round((b.x + b.w) * k) - dx)
    const dh = Math.max(1, Math.round((b.y + b.h) * k) - dy)
    if (dx >= base.width || dy >= base.height) continue
    const sw = Math.max(1, Math.min(dw, base.width - dx))
    const sh = Math.max(1, Math.min(dh, base.height - dy))
    blocks.push({ dx, dy, dw, dh, sw, sh })
  }
  if (!blocks.length) return
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity
  for (const b of blocks) {
    x0 = Math.min(x0, b.dx)
    y0 = Math.min(y0, b.dy)
    x1 = Math.max(x1, b.dx + b.sw)
    y1 = Math.max(y1, b.dy + b.sh)
  }
  const region = bctx.getImageData(x0, y0, x1 - x0, y1 - y0)
  const rd = region.data
  const rw = region.width
  // 回填也走设备像素：CSS 块网格在分数缩放下会落在半像素上，
  // 相邻两块各盖一半 → 每个块缝漏出一条 25% 透光的原图细线（打码就白打了）
  ctx.save()
  // 先按当前（CSS）变换设好裁剪，再切到设备像素坐标
  if (sel.value) {
    ctx.beginPath()
    ctx.rect(sel.value.x, sel.value.y, sel.value.w, sel.value.h)
    ctx.clip()
  }
  ctx.setTransform(1, 0, 0, 1, 0, 0)
  for (const b of blocks) {
    let r = 0, g = 0, bl = 0, n = 0
    for (let y = b.dy; y < b.dy + b.sh; y++) {
      const row = (y - y0) * rw - x0
      for (let x = b.dx; x < b.dx + b.sw; x++) {
        const i = (row + x) * 4
        r += rd[i]; g += rd[i + 1]; bl += rd[i + 2]; n++
      }
    }
    if (!n) continue
    ctx.fillStyle = `rgb(${Math.round(r / n)},${Math.round(g / n)},${Math.round(bl / n)})`
    ctx.fillRect(b.dx, b.dy, b.dw, b.dh)
  }
  ctx.restore()
}

/** 文字工具的输入框与烧录字号同源：改动时两处必须一起改 */
const textFontSize = computed(() => 12 + width.value * 4)
/** 提交点要跳过输入框的边框(1px)与内边距(6px/2px)，否则烧录的文字会比预览往左上跳 */
const TEXT_ORIGIN = { x: 1 + 6, y: 1 + 2 }

/** 文字工具：Enter / 失焦把输入框里的字烧进标注层，空串直接丢弃。
 *  字号与输入框同一公式（textFontSize），起点用输入框的内容框原点，
 *  这样预览什么样、烧出来就是什么样。 */
function commitText() {
  const at = textAt.value
  if (!at || !annoCanvas.value) return
  const value = (at.value || '').trim()
  if (value) {
    const ctx = annoCanvas.value.getContext('2d')
    pushSnapshot()
    ctx.save()
    // 与 drawShape 同理：超出选区的文字不该只活在屏幕上
    if (sel.value) {
      ctx.beginPath()
      ctx.rect(sel.value.x, sel.value.y, sel.value.w, sel.value.h)
      ctx.clip()
    }
    ctx.fillStyle = color.value
    ctx.font = `${textFontSize.value}px system-ui, sans-serif`
    ctx.textBaseline = 'top'
    ctx.fillText(value, at.x + TEXT_ORIGIN.x, at.y + TEXT_ORIGIN.y)
    ctx.restore()
  }
  textAt.value = null
}

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
  // 标注层跟着窗口尺寸走：resize 之后窗口变大，标注可画区域必须同步
  paintAnnoSize()
  // 底图落地：压暗层/提示这一刻才允许出现（见 painted 的注释）
  painted.value = true
}

/** 等 n 个动画帧再继续：第 1 帧把这一帧提交上去，第 2 帧确认它已经进了合成管线。
 *  这里刻意不用定时器 —— 定时器只保证「过了一段时间」，不保证帧真的画出去、
 *  被合成器取走；帧什么时候提交只有 requestAnimationFrame 说得准。 */
function nextFrames(n) {
  return new Promise((resolve) => {
    const tick = (left) => (left <= 0 ? resolve() : requestAnimationFrame(() => tick(left - 1)))
    tick(n)
  })
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
  } finally {
    // 底图（或错误提示）画完、帧真的提交出去之后才报「已就绪」。后端拿它做三件事：
    // 记录一条锚点日志、把窗口端到前台/交回焦点、Wayland 上补一次幂等的指定屏全屏。
    // 出错也必须报：这条信号是「遮罩这一层已经画完了」的唯一凭据，
    // 少一次日志里就分不清是页面挂了还是后端没把窗口弄出来
    await nextFrames(2)
    try {
      await ipc.shotOverlayReady(session, index)
    } catch (e) {
      console.error('shot overlay ready failed', e)
    }
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
  // 选中了标注工具、且落点在选区内部：本笔属于标注，不再是移动/新建选区
  if (hit === 'inside' && tool.value !== 'move' && sel.value) {
    if (tool.value === 'text') {
      // 阻止 mousedown 的默认聚焦行为：否则刚建出来的输入框立刻失焦 → blur 提交空值
      ev.preventDefault()
      textAt.value = { x: p.x, y: p.y, value: '' }
      nextTick(() => textInput.value?.focus())
      return
    }
    pushSnapshot()
    drawing = { tool: tool.value, from: p, to: p, points: [p] }
    root.value.setPointerCapture?.(ev.pointerId)
    return
  }
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
  // 丢过 pointerup：按松手处理，别粘住（标注收笔与选区拖拽都要兜）
  if (ev.buttons === 0 && (drawing || drag)) onPointerUp()
  const p = localPoint(ev)
  // 正在画：每帧从上一张快照重画，避免拖拽预览越描越黑
  if (drawing) {
    const ctx = annoCanvas.value.getContext('2d')
    const snap = undoStack.value[undoStack.value.length - 1]
    if (snap) ctx.putImageData(snap, 0, 0)
    drawing.to = p
    if (drawing.tool === 'pen') drawing.points.push(p)
    if (drawing.tool === 'mosaic') applyMosaic(ctx, rectFromDrag(drawing.from, p, winRect.value))
    else drawShape(ctx, drawing)
    return
  }
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
  // 收笔：本笔起点的快照已经压栈（onPointerDown），这里只结束预览
  if (drawing) {
    drawing = null
    return
  }
  if (drag && !canConfirm(sel.value)) sel.value = null
  drag = null
}

function onKeydown(ev) {
  // 文字输入进行中：键盘归输入框（Enter/Esc 已 .stop，其余按键不应影响画布与选区）
  if (textAt.value) return
  if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === 'z') {
    ev.preventDefault()
    return undo()
  }
  if (ev.key === 'Escape') return cancel()
  if (ev.key === 'Enter') return confirm()
  if (ev.key.startsWith('Arrow') && sel.value) {
    ev.preventDefault()
    sel.value = nudgeRect(sel.value, ev.key, ev.shiftKey ? 10 : 1, winRect.value)
  }
}

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
    // 标注层后备像素 = 窗口 CSS × kWin，取样矩形必须换算到设备像素，否则会取错/缩小。
    // 只能用 kWin：拿四舍五入后的 r.w / sel.w 当比例，会让取样原点偏 sel.x*(k-k0) 个像素
    const k = kWin.value
    ctx.imageSmoothingEnabled = false
    ctx.drawImage(
      anno,
      sel.value.x * k, sel.value.y * k, sel.value.w * k, sel.value.h * k,
      0, 0, r.w, r.h,
    )
  }
  const url = out.toDataURL('image/png')
  return { b64: url.slice(url.indexOf(',') + 1), width: r.w, height: r.h }
}

async function confirm() {
  if (!canOk.value || busy.value) return
  errMsg.value = ''
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
  errMsg.value = ''
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
  if (!canOk.value || busy.value) return
  errMsg.value = ''
  busy.value = true
  try {
    const { b64 } = compositeB64()
    const base = `screenshot-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, '')}.png`
    const path = await saveDialog({ defaultPath: base, filters: [{ name: 'PNG', extensions: ['png'] }] })
    if (!path) return
    await ipc.saveShotPng(b64, path)
  } catch (e) {
    errMsg.value = String(e?.message || e)
  } finally {
    busy.value = false
  }
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
  // 先取新尺寸再重绘：paintBase 用的是 winRect（= winW/winH）
  winW.value = window.innerWidth
  winH.value = window.innerHeight
  paintBase()
}

onMounted(() => {
  // 兜底层：把 html/body 的底色清成透明（global.css 给 body 上了 var(--c-card)）。
  //
  // 真正救命的是后端建窗时注入的初始化脚本（`open_overlays` 里的 OVERLAY_BOOT_CSS）：
  // 打包版 index.html 用 <link> 引入 global.css，底色**在模块脚本执行之前**就被涂上，
  // 而遮罩窗口建出来即已映射 —— 那一帧只有「文档脚本之前生效的 !important 规则」拦得住，
  // 这句 onMounted 太晚、拦不住。两处都留着：这句在 dev 下立即生效，也能兜住
  // 初始化脚本失效的情况；底图与压暗层画上去之后整窗不透明（canvas 铺满窗口）。
  document.documentElement.style.background = 'transparent'
  document.body.style.background = 'transparent'
  load()
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('resize', onResize)
})
// 工具栏元素与它的宽度都随工具/选区变化：等 DOM 落地后重新量一次，再夹位置
onMounted(() => nextTick(measureBar))
watch([tool, blockSize, sel], () => nextTick(measureBar))
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
    @pointercancel="onPointerUp"
    @contextmenu.prevent="cancel"
  >
    <canvas ref="baseCanvas" class="base"></canvas>
    <!-- 标注层：整窗同尺寸的透明 canvas，压在底图之上、暗罩之下
         （选区之外的标注和底图一起被压暗） -->
    <canvas ref="annoCanvas" class="anno"></canvas>
    <!-- 还没有选区时整屏压暗（微信行为：进入截图态立刻有反馈）；
         有选区后改用 box-shadow 挖洞，只暗选区之外。
         `painted` 之前不画：底图没上来时这一层会先把整屏压暗（透明窗上就是
         「桌面忽然暗一下」），那正是要修的闪烁 -->
    <div v-if="!sel && painted" class="dim-full"></div>
    <div v-else class="dim" :style="selStyle">
      <div class="frame"></div>
      <div class="size" v-if="sel">{{ sizeLabel }}</div>
      <span v-for="h in ['nw','n','ne','e','se','s','sw','w']" :key="h" :class="['handle', h]"></span>
    </div>
    <div v-if="sel" ref="barRef" class="toolbar" :style="barStyle" @pointerdown.stop @pointerup.stop>
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
    </div>
    <input v-if="textAt" ref="textInput" v-model="textAt.value" class="text-in"
      :style="{ left: textAt.x + 'px', top: textAt.y + 'px', fontSize: textFontSize + 'px' }"
      @pointerdown.stop @pointerup.stop
      @keydown.enter.stop.prevent="commitText" @keydown.esc.stop.prevent="textAt = null"
      @keydown.ctrl.z.stop @keydown.meta.z.stop @blur="commitText" />
    <div v-if="!sel && !errMsg && painted" class="tip">{{ t('shot.tip') }}</div>
    <div v-if="errMsg" class="error">{{ errMsg }}</div>
  </div>
</template>

<style scoped>
.shot-root {
  position: fixed;
  inset: 0;
  overflow: hidden;
  /* 透明：底图没画上去之前窗口里应当什么都看不到（窗口本身也是透明窗）；
     画完之后整窗被 canvas + 压暗层盖满，不存在透出桌面的问题 */
  background: transparent;
  user-select: none;
}
.base {
  position: absolute;
  left: 0;
  top: 0;
  image-rendering: pixelated;
}
/* 标注层：后备像素是设备分辨率（窗口 CSS 尺寸 × kWin，与底图同源），
   元素仍按 CSS 尺寸布局，故与 .base 同位置即可对齐；
   不设 image-rendering，图形保持抗锯齿 */
.anno {
  position: absolute;
  left: 0;
  top: 0;
}
/* 变暗用「挖洞」实现：选区那一块不盖黑罩，靠超大 box-shadow 覆盖其余区域。
   比每帧重绘底图便宜得多（GPU 合成），拖拽 100% 跟手。 */
.dim-full {
  position: absolute;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
}
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
/* 工具栏：位置由 toolbarPlacement 算好（选区下方→上方→窗口内） */
.toolbar {
  position: absolute;
  display: flex;
  align-items: center;
  gap: 4px;
  height: 36px;
  padding: 0 6px;
  background: rgba(32, 32, 32, 0.92);
  border-radius: 6px;
  color: #eee;
}
.toolbar button {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 26px;
  padding: 0 4px;
  border: none;
  border-radius: 4px;
  background: none;
  color: inherit;
  font-size: 12px;
  line-height: 1;
  cursor: pointer;
}
.toolbar button:hover {
  background: rgba(255, 255, 255, 0.14);
}
.toolbar button.on {
  color: #1aad19;
}
.toolbar .ok {
  color: #1aad19;
}
.sep {
  width: 1px;
  height: 18px;
  margin: 0 2px;
  background: rgba(255, 255, 255, 0.25);
}
.dot {
  width: 16px;
  height: 16px;
  padding: 0;
  border-radius: 50%;
  border: 2px solid transparent;
}
.dot.on {
  border-color: #fff;
}
.wdot {
  display: block;
  background: currentColor;
  border-radius: 50%;
}
.text-in {
  position: absolute;
  min-width: 120px;
  padding: 2px 6px;
  border: 1px solid #1aad19;
  outline: none;
  background: #fff;
  color: #000;
  /* 字号由内联样式绑定 textFontSize（与烧录同一公式，别在这里再写死一个）；
     字族也要与 canvas 的 system-ui 一致，否则预览与烧录的字宽对不上 */
  font-family: system-ui, sans-serif;
}
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
