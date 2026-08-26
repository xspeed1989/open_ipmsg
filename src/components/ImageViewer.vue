<script setup>
// 图片查看器（独立窗口，仿微信 PC）：滚轮缩放 / 拖动平移 / 旋转 / 1:1 / 适应窗口
//
// 缩放以"适应窗口"为基准：图片始终 max-width/height 100%，scale 在此之上叠加，
// 这样从适应态开始缩放不会突然跳成原始像素尺寸。
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { t } from '../lib/i18n'

const appWindow = getCurrentWindow()
// 参数优先取窗口创建时注入的对象，其次回退查询串
const boot = window.__OIM_VIEWER__ || {}
const params = new URLSearchParams(location.search)
const path = boot.path || params.get('path') || ''
const name = boot.name || path.split(/[\\/]/).pop() || t('viewer.image')

const src = ref('')
const error = ref('')
const scale = ref(1)
const rotate = ref(0)
const tx = ref(0)
const ty = ref(0)
const dragging = ref(false)
const img = ref(null)
/** 原始像素 ÷ 适应窗口后的显示尺寸；1:1 就是把 scale 设成它 */
const oneToOne = ref(1)

const zoomed = computed(() => Math.abs(scale.value - 1) > 0.001)
const pct = computed(() => Math.round((scale.value / oneToOne.value) * 100))

const style = computed(() => ({
  transform: `translate(${tx.value}px, ${ty.value}px) scale(${scale.value}) rotate(${rotate.value}deg)`,
  cursor: dragging.value ? 'grabbing' : zoomed.value ? 'grab' : 'zoom-in',
}))

onMounted(async () => {
  if (!path) {
    error.value = t('viewer.missingPath')
  } else {
    try {
      const r = await invoke('read_image_data', { path })
      src.value = `data:${r.mime};base64,${r.b64}`
    } catch (e) {
      error.value = t('viewer.failed', { e })
    }
  }
  window.addEventListener('keydown', onKey)
  window.addEventListener('resize', measure)
})
onUnmounted(() => {
  window.removeEventListener('keydown', onKey)
  window.removeEventListener('resize', measure)
})

/** 量出"适应窗口"下的显示尺寸，换算 1:1 需要的缩放倍数 */
function measure() {
  const el = img.value
  if (!el || !el.naturalWidth) return
  const shown = el.getBoundingClientRect().width / (scale.value || 1)
  oneToOne.value = shown > 0 ? el.naturalWidth / shown : 1
}

function onKey(e) {
  const k = e.key.toLowerCase()
  if (e.key === 'Escape' || (k === 'w' && (e.ctrlKey || e.metaKey))) close()
  else if (e.key === '+' || e.key === '=') zoom(1.2)
  else if (e.key === '-') zoom(1 / 1.2)
  else if (e.key === '0') fit()
  else if (k === 'r') rotate.value = (rotate.value + 90) % 360
}

function zoom(k) {
  scale.value = Math.min(16, Math.max(0.1, scale.value * k))
  if (!zoomed.value) {
    tx.value = 0
    ty.value = 0
  }
}
function fit() {
  scale.value = 1
  tx.value = 0
  ty.value = 0
}
function actualSize() {
  scale.value = oneToOne.value
  tx.value = 0
  ty.value = 0
}
/** 单击/双击在「适应窗口」与「原始大小」之间切换（图片本来就比窗口小时不放大） */
function toggleZoom() {
  zoomed.value || oneToOne.value <= 1 ? fit() : actualSize()
}
function onWheel(e) {
  e.preventDefault()
  zoom(e.deltaY < 0 ? 1.15 : 1 / 1.15)
}

let sx = 0
let sy = 0
function onDown(e) {
  if (e.button !== 0 || !zoomed.value) return
  dragging.value = true
  sx = e.clientX - tx.value
  sy = e.clientY - ty.value
}
function onMove(e) {
  if (!dragging.value) return
  tx.value = e.clientX - sx
  ty.value = e.clientY - sy
}
function onUp() {
  dragging.value = false
}

function close() {
  appWindow.close().catch(() => {})
}
async function reveal() {
  try {
    await revealItemInDir(path)
  } catch (e) {
    /* 打开文件夹失败时忽略 */
  }
}
</script>

<template>
  <div class="viewer" @mousemove="onMove" @mouseup="onUp" @mouseleave="onUp">
    <!-- 双击最大化由 Tauri 的 drag-region 脚本原生处理，勿再绑 @dblclick（会切换两次） -->
    <header class="bar" data-tauri-drag-region>
      <span class="title" data-tauri-drag-region>{{ name }}</span>
      <div class="acts">
        <button :title="t('viewer.zoomOut')" @click="zoom(1 / 1.2)">－</button>
        <button :title="t('viewer.zoomIn')" @click="zoom(1.2)">＋</button>
        <span class="pct">{{ pct }}%</span>
        <button :title="zoomed ? t('viewer.fitTitle') : t('viewer.actualSize')" @click="toggleZoom">
          {{ zoomed ? t('viewer.fit') : t('viewer.oneToOne') }}
        </button>
        <button :title="t('viewer.rotate')" @click="rotate = (rotate + 90) % 360">⟳</button>
        <button :title="t('viewer.reveal')" @click="reveal">📂</button>
        <button class="x" :title="t('viewer.close')" @click="close">✕</button>
      </div>
    </header>

    <div class="stage" @wheel="onWheel" @mousedown="onDown" @dblclick="toggleZoom">
      <p v-if="error" class="err">{{ error }}</p>
      <img
        v-else-if="src"
        ref="img"
        :src="src"
        :style="style"
        draggable="false"
        alt=""
        @load="measure"
      />
      <p v-else class="loading">{{ t('viewer.loading') }}</p>
    </div>
  </div>
</template>

<style scoped>
.viewer {
  position: fixed;
  inset: 0;
  display: flex;
  flex-direction: column;
  background: #1c1c1c;
  color: #ddd;
  user-select: none;
  overflow: hidden;
}
.bar {
  flex: none;
  height: 38px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 6px 0 12px;
  background: #262626;
  border-bottom: 1px solid #000;
}
.title {
  flex: 1;
  min-width: 0;
  font-size: 12.5px;
  color: #bbb;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.acts {
  flex: none;
  display: flex;
  gap: 2px;
}
.acts button {
  width: 30px;
  height: 26px;
  border: none;
  background: transparent;
  color: #bbb;
  font-size: 13px;
  border-radius: 4px;
  cursor: pointer;
}
.acts button:hover {
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
}
.acts button.x:hover {
  background: #e81123;
  color: #fff;
}
.stage {
  flex: 1;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
}
.pct {
  flex: none;
  font-size: 11.5px;
  color: #888;
  min-width: 40px;
  text-align: right;
}
.stage img {
  max-width: 100%;
  max-height: 100%;
  transform-origin: center center;
  transition: transform 0.06s linear;
  -webkit-user-drag: none;
}
.err,
.loading {
  font-size: 13px;
  color: #999;
}
</style>
