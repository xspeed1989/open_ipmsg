<script setup>
// 截图遮罩窗口：底图 + 变暗挖洞 + 拖拽选区 + 标注层（矩形/椭圆/箭头/画笔/文字/马赛克）。
// 「确认/复制/另存为」的合成导出在 Task 9 接入，本任务只画不导出。
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import * as ipc from '../lib/ipc'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle, resizeRect, moveRect, nudgeRect,
  cssRectToImageRect, toolbarPlacement, mosaicBlocks, arrowHead, pushUndo,
} from '../lib/shot'
import { t } from '../lib/i18n'

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
const hint = ref('')

let drag = null                // { mode:'new'|'move'|'resize', handle, start, origin }

// 窗口尺寸必须是响应式的：computed 直接读 DOM（clientWidth）只会求值一次，
// 之后 resize 既不重算也不触发重绘 —— 而 Wayland 上遮罩是先建成 320x200
// 再全屏的，首次绘制会与那次 resize 竞争，赢在错误的一侧就会把整层画进角落。
const winW = ref(window.innerWidth)
const winH = ref(window.innerHeight)
const winRect = computed(() => ({ x: 0, y: 0, w: winW.value, h: winH.value }))

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

/** 工具栏贴合：优先选区下方，放不下翻到上方，最后夹进窗口 */
const barStyle = computed(() => {
  const bar = { w: 420, h: 40 }
  const p = toolbarPlacement(sel.value || { x: 0, y: 0, w: 0, h: 0 }, winRect.value, bar)
  return { left: p.x + 'px', top: p.y + 'px' }
})

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

/** 马赛克：读底图对应区域的像素，按块平均后回填。
 *
 *  坐标系：标注层与选区是 CSS 像素，而 base 画布的后备像素是图像物理像素
 *  （k = slice.w / 窗口 CSS 宽，本机 1.25）。采样必须乘 k，回填仍用 CSS 坐标，
 *  否则每个块都取到图上别处的像素，块网格也会与拖拽范围错位。 */
function applyMosaic(ctx, cssRect) {
  const base = baseCanvas.value
  if (!base) return
  const bctx = base.getContext('2d')
  const k = base.width / Math.max(1, winRect.value.w)
  for (const b of mosaicBlocks(cssRect, blockSize.value)) {
    const sx = Math.max(0, Math.round(b.x * k))
    const sy = Math.max(0, Math.round(b.y * k))
    if (sx >= base.width || sy >= base.height) continue
    const sw = Math.max(1, Math.min(Math.round(b.w * k), base.width - sx))
    const sh = Math.max(1, Math.min(Math.round(b.h * k), base.height - sy))
    const data = bctx.getImageData(sx, sy, sw, sh).data
    let r = 0, g = 0, bl = 0, n = 0
    for (let i = 0; i < data.length; i += 4) {
      r += data[i]; g += data[i + 1]; bl += data[i + 2]; n++
    }
    if (!n) continue
    ctx.fillStyle = `rgb(${Math.round(r / n)},${Math.round(g / n)},${Math.round(bl / n)})`
    ctx.fillRect(b.x, b.y, b.w, b.h)
  }
}

/** 文字工具：Enter / 失焦把输入框里的字烧进标注层，空串直接丢弃 */
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
  if (drawing && ev.buttons === 0) onPointerUp()   // 丢过 pointerup：按松手处理，别粘住
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
  // 先取新尺寸再重绘：paintBase 用的是 winRect（= winW/winH）
  winW.value = window.innerWidth
  winH.value = window.innerHeight
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
    @pointercancel="onPointerUp"
    @contextmenu.prevent="cancel"
  >
    <canvas ref="baseCanvas" class="base"></canvas>
    <!-- 标注层：整窗同尺寸的透明 canvas，压在底图之上、暗罩之下
         （选区之外的标注和底图一起被压暗） -->
    <canvas ref="annoCanvas" class="anno"></canvas>
    <!-- 还没有选区时整屏压暗（微信行为：进入截图态立刻有反馈）；
         有选区后改用 box-shadow 挖洞，只暗选区之外 -->
    <div v-if="!sel" class="dim-full"></div>
    <div v-else class="dim" :style="selStyle">
      <div class="frame"></div>
      <div class="size" v-if="sel">{{ sizeLabel }}</div>
      <span v-for="h in ['nw','n','ne','e','se','s','sw','w']" :key="h" :class="['handle', h]"></span>
    </div>
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
      @pointerdown.stop @pointerup.stop
      @keydown.enter.stop.prevent="commitText" @keydown.esc.stop.prevent="textAt = null"
      @keydown.ctrl.z.stop @keydown.meta.z.stop @blur="commitText" />
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
/* 标注层：CSS 像素 1:1（canvas 后备像素 = 窗口 CSS 尺寸），
   与 .base 同位置即可对齐；不设 image-rendering，图形保持抗锯齿 */
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
  font-size: 14px;
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
