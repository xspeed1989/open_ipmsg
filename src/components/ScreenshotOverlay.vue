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
