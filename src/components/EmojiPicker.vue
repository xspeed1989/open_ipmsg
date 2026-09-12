<script setup>
// 表情面板：Unicode 常用表情 + 自定义表情库
//
// 自定义页支持：导入本地图片、点一张即发、悬停删除、右键重命名/导出、
// 导出表情包、导入表情包（先预览勾选再入库）。
//
// 定位（position/left/top）与最大高度由父组件 ChatWindow 按触发按钮位置用
// inline style 注入；本组件只负责内容与自身滚动，并把面板所需高度回传给父组件。
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { t } from '../lib/i18n'
import * as ipc from '../lib/ipc'
import { store } from '../store'
import {
  normalizeEmojis, importSummary, packSummary, packItemState, defaultPackSelection,
  packFileName, MAX_IMPORT_BATCH,
} from '../lib/emoji'

const emit = defineEmits(['pick', 'pickSticker', 'need-layout'])

/* ---------------- Unicode 表情 ---------------- */

/**
 * 常用表情：排布对齐微信表情面板 —— 9 列、等间距大格子（见样式里的
 * --emoji-cols / --emoji-cell），每行 9 个、共 8 行（刚好铺满网格，不留半行）。
 */
const EMOJIS = [
  // 9 列 × 9 行 = 81 个：一屏放不下，必须能纵向滚动（与微信表情面板一致）
  '😊', '😂', '🤣', '😍', '😜', '😎', '🤔', '😅', '😭',
  '😡', '🥺', '😴', '🤯', '🥳', '😇', '🙃', '😱', '🤡',
  '😏', '😬', '🙄', '😮', '😯', '😲', '🥱', '😪', '🤤',
  '😷', '🤒', '🤕', '🤢', '🥶', '🥵', '😵', '🤠', '🥸',
  '👍', '👎', '👌', '✌️', '🤝', '🙏', '👏', '💪', '🫶',
  '👋', '🙌', '🤟', '🤘', '👊', '✊', '🫰', '🖐️', '💅',
  '💯', '🔥', '🎉', '🎁', '⚡', '☀️', '🌙', '🌈', '⭐',
  '❤️', '🧡', '💛', '💚', '💙', '💜', '🤍', '💔', '💖',
  '💘', '💝', '🌹', '🎂', '🍎', '🍜', '☕', '🍺', '⚽',
]

/* ---------------- 收藏（心形按钮切换的视图） ---------------- */


/** 按 key 找表情库里的条目（收藏只存 key，缩略图与发送都要用它） */

/**
 * ❤️ 按钮：指向「图片表情包」（就是用户自己的图片表情，不是收藏）。
 * - 普通表情视图：进入图片表情包
 * - 表情库/包预览：进入收藏
 * - ❤️（图片表情包）：回到普通表情
 */
function stickerHint() {
  if (view.value === 'unicode') view.value = 'library'
  else if (view.value === 'library' || view.value === 'pack') view.value = 'stickers'
  else view.value = 'unicode'
}

/* ---------------- 状态 ---------------- */

/** 面板视图（没有顶部页签，改由底部行为条切换）：
 *  unicode 普通表情 / library 图片表情包 / pack 包导入预览 */
const view = ref('unicode')
/** 面板根元素（菜单定位的参照系）与菜单元素（夹边界用） */
const panelRef = ref(null)
const menuRef = ref(null)
const emojis = ref([])
/** id -> 缩略图 data URL（按需加载，避免几十张大图一次性走 IPC） */
const thumbs = ref({})
const err = ref('')
const hint = ref('')
const busy = ref(false)
/** 面板内小菜单（右键缩略图）：{ x, y, entry } */
const menu = ref(null)

/* 表情包导入预览 */
const pack = ref(null) // { path, loading, info, sel:Set, imported:number }
const packPath = ref('')

const files = computed(() => new Set(emojis.value.map((e) => e.file)))

function flash(text) {
  hint.value = text
  setTimeout(() => {
    if (hint.value === text) hint.value = ''
  }, 2800)
}

function fail(e) {
  err.value = String(e?.message || e)
  setTimeout(() => {
    if (err.value) err.value = ''
  }, 4000)
}

/* ---------------- 表情库 ---------------- */

async function loadThumb(e) {
  if (thumbs.value[e.id] || !e.abs) return
  thumbs.value[e.id] = 'loading'
  try {
    const r = await ipc.readImageData(e.abs)
    thumbs.value[e.id] = `data:${r.mime};base64,${r.b64}`
  } catch {
    thumbs.value[e.id] = '' // 读取失败：留空占位，不再重试
  }
}

async function loadList() {
  try {
    const r = await ipc.listEmojis()
    emojis.value = normalizeEmojis(r)
    store.emojiFiles = emojis.value.map((e) => e.file)
    store.emojiCacheFiles = emojis.value.map((e) => e.cacheFile).filter(Boolean)
    for (const e of emojis.value) loadThumb(e)
  } catch (e) {
    fail(t('emoji.loadFailed', { e: String(e?.message || e) }))
  }
}

function syncStore() {
  store.emojiFiles = emojis.value.map((e) => e.file)
  store.emojiCacheFiles = emojis.value.map((e) => e.cacheFile).filter(Boolean)
}

onMounted(() => {
  loadList()
  // 面板高度已固定，这里只需在挂载后报一次实测高度给父组件摆位
  nextTick(() => emit('need-layout'))
})

/* ---------------- 导入本地图片 ---------------- */

async function importImages() {
  if (busy.value) return
  try {
    const sel = await ipc.pickImageFiles()
    if (!sel) return
    const paths = (Array.isArray(sel) ? sel : [sel]).filter(Boolean)
    if (!paths.length) return
    if (paths.length > MAX_IMPORT_BATCH) {
      flash(t('emoji.tooMany', { n: MAX_IMPORT_BATCH }))
      return
    }
    busy.value = true
    const r = await ipc.importEmoji(paths)
    const added = normalizeEmojis({ emojis: r?.imported || [] })
    // 直接增量更新，少一次全量拉取；顺序与后端一致（新导入排最后）
    if (added.length) {
      emojis.value = emojis.value.concat(added)
      syncStore()
      for (const e of added) loadThumb(e)
    }
    const sum = importSummary(r, t)
    if (sum.level === 'warn') err.value = sum.text
    else flash(sum.text)
  } catch (e) {
    fail(t('emoji.importFailed', { e: String(e?.message || e) }))
  } finally {
    busy.value = false
  }
}

/* ---------------- 导出表情包 ---------------- */

/** ids 为空 = 导出全部 */
async function exportPack(ids = [], count = 0) {
  if (busy.value) return
  if (!emojis.value.length) {
    flash(t('emoji.exportEmpty'))
    return
  }
  try {
    const dest = await ipc.pickEmojiPackSavePath(packFileName())
    if (!dest) return
    busy.value = true
    const r = await ipc.exportEmojiPack(ids, dest, t('emoji.packDefaultName'))
    flash(t('emoji.exportDone', { n: r?.count ?? count, path: r?.path || dest }))
  } catch (e) {
    fail(t('emoji.importFailed', { e: String(e?.message || e) }))
  } finally {
    busy.value = false
  }
}

/* ---------------- 导入表情包 ---------------- */

async function choosePack() {
  if (busy.value) return
  try {
    const sel = await ipc.pickEmojiPack()
    if (!sel) return
    packPath.value = Array.isArray(sel) ? sel[0] : sel
    view.value = 'pack'
    pack.value = { loading: true, info: null, sel: new Set(), imported: 0 }
    const info = await ipc.inspectEmojiPack(packPath.value)
    pack.value = { loading: false, info, sel: defaultPackSelection(info), imported: 0 }
  } catch (e) {
    view.value = 'library'
    pack.value = null
    fail(t('emoji.importFailed', { e: String(e?.message || e) }))
  }
}

const packItems = computed(() => pack.value?.info?.items || [])

function toggleItem(item) {
  const p = pack.value
  if (!p || item.problem) return
  const s = new Set(p.sel)
  if (s.has(item.file)) s.delete(item.file)
  else s.add(item.file)
  p.sel = s
}

function toggleAll() {
  const p = pack.value
  if (!p) return
  const importable = packItems.value.filter((i) => !i.problem && i.kind)
  p.sel = p.sel.size >= importable.length ? new Set() : new Set(importable.map((i) => i.file))
}

async function confirmPack() {
  const p = pack.value
  if (!p || busy.value) return
  const chosen = packItems.value.filter((i) => p.sel.has(i.file) && !i.problem)
  if (!chosen.length) return
  busy.value = true
  try {
    const r = await ipc.importEmojiPack(packPath.value, chosen.map((i) => i.file))
    p.imported = r?.imported ?? 0
    const sum = importSummary(r, t)
    if (sum.level === 'warn') err.value = sum.text
    await loadList()
    view.value = 'library'
    pack.value = null
    flash(t('emoji.packDone', { n: p.imported }))
  } catch (e) {
    fail(t('emoji.importFailed', { e: String(e?.message || e) }))
  } finally {
    busy.value = false
  }
}

/* ---------------- 单张管理（悬停 / 右键） ---------------- */

/** 右键菜单里的导入/导出（从底部行为条移过来的） */
async function menuImportImages() {
  closeMenu()
  await importImages()
}

async function menuImportPack() {
  closeMenu()
  await choosePack()
}

async function menuExportPack() {
  const n = emojis.value.length
  closeMenu()
  await exportPack([], n)
}

/** 删除一张表情（只从右键菜单进入，因此带一次确认） */
async function deleteSticker(entry) {
  if (busy.value) return
  try {
    await ipc.deleteEmoji([entry.id])
    emojis.value = emojis.value.filter((e) => e.id !== entry.id)
    delete thumbs.value[entry.id]
    syncStore()
  } catch (e) {
    fail(String(e?.message || e))
  }
}

/**
 * 面板内菜单定位。
 *
 * 统一用**视口坐标**（clientX/clientY）换算：菜单是 `.emoji-panel`（position: fixed）
 * 的子元素、按面板左上角绝对定位，所以「视口坐标 − 面板左上角」就是唯一正确的答案。
 * 之前用 currentTarget.offsetParent 反推，空白处右键时参照系不对，菜单位置会偏。
 */
function menuAt(ev) {
  const host = panelRef.value?.getBoundingClientRect?.()
  if (!host) return { x: 0, y: 0 }
  return { x: ev.clientX - host.left, y: ev.clientY - host.top }
}

/** 菜单渲染后按面板边界夹一次（贴边右键时不能溢出面板/窗口） */
function clampMenuToPanel() {
  nextTick(() => {
    const el = menuRef.value
    if (!menu.value || !el) return
    const host = panelRef.value?.getBoundingClientRect?.()
    const box = el.getBoundingClientRect()
    if (!host) return
    const pad = 6
    const maxX = Math.max(pad, host.width - box.width - pad)
    const maxY = Math.max(pad, host.height - box.height - pad)
    // 面板高出屏幕顶/底时，再用视口范围兜一层
    const viewMaxY = Math.max(pad, host.bottom - box.height - pad) - host.top
    menu.value = {
      ...menu.value,
      x: Math.min(Math.max(menu.value.x, pad), maxX),
      y: Math.min(Math.max(menu.value.y, pad), Math.min(maxY, viewMaxY)),
    }
  })
}

/** 关掉右键菜单 */
/**
 * 悬停放大预览（微信同款）：鼠标停在图片表情上时，在面板内浮出一张大图；
 * 移开当前格子即关闭。滚动/关闭面板/切视图都会收起，避免留着错位的浮层。
 */
const hoverPreview = ref(null)
const PREVIEW_SIZE = 200

function hoverAt(ev, entry) {
  if (view.value !== 'stickers' || !entry) return
  const host = panelRef.value?.getBoundingClientRect?.()
  const box = ev.currentTarget?.getBoundingClientRect?.()
  if (!host || !box) return
  const half = PREVIEW_SIZE / 2
  const margin = 8
  // 优先显示在格子正上方；顶部放不下就翻到下方；横向夹在面板内
  let y = box.top - host.top - PREVIEW_SIZE - margin
  if (y < margin) y = Math.min(box.bottom - host.top + margin, host.height - PREVIEW_SIZE - margin)
  y = Math.min(Math.max(y, margin), Math.max(margin, host.height - PREVIEW_SIZE - margin))
  const centerX = box.left - host.left + box.width / 2
  const x = Math.min(Math.max(centerX - half, margin), Math.max(margin, host.width - PREVIEW_SIZE - margin))
  hoverPreview.value = { entry, x, y }
}

function hoverEnd() {
  hoverPreview.value = null
}

/** 关掉右键菜单（模板的 @scroll、各动作、全局点击/Esc 都调它） */
function closeMenu() {
  menu.value = null
}

/** 点击菜单以外的地方 / 按 Esc → 关闭（失焦自动收起） */
function onDocMouseDown(e) {
  const path = e.composedPath ? e.composedPath() : []
  const el = menuRef.value
  if (el && path.includes(el)) return
  closeMenu()
}
function onDocKeyDown(e) {
  if (e.key === 'Escape') closeMenu()
}
watch(menu, (open) => {
  if (open) {
    document.addEventListener('mousedown', onDocMouseDown, true)
    document.addEventListener('keydown', onDocKeyDown)
  } else {
    document.removeEventListener('mousedown', onDocMouseDown, true)
    document.removeEventListener('keydown', onDocKeyDown)
  }
})
onUnmounted(() => {
  document.removeEventListener('mousedown', onDocMouseDown, true)
  document.removeEventListener('keydown', onDocKeyDown)
})

/** 空白处右键：只出「导入 / 导出」（没有表情时唯一能用右键的地方） */
function openBlankMenu(ev) {
  menu.value = { ...menuAt(ev), entry: null }
  clampMenuToPanel()
}

/** 表情库格子的右键菜单（移到最前 / 删除 + 导入导出） */
function openMenu(ev, entry) {
  menu.value = { ...menuAt(ev), entry }
  clampMenuToPanel()
}

/** ❤️ 收藏页格子的右键菜单（移到最前 / 删除） */
function openFavMenu(ev, entry) {
  menu.value = { ...menuAt(ev), entry }
  clampMenuToPanel()
}

/**
 * 移到最前：
 * - ❤️ 收藏页 → 提到收藏列表最前
 * - 表情库 → 提到表情库最前（并写回后端顺序）
 */
async function menuMoveFront() {
  const m = menu.value
  closeMenu()
  const entry = m?.entry
  if (!entry?.file) return
  // 两个页面都是「图片表情库」的同一个顺序，改一次两处都生效
  const list = emojis.value.slice()
  const i = list.findIndex((x) => x.file === entry.file)
  if (i < 0) {
    fail(t('emoji.gone')) // 表情库里已经没有它（比如刚被删除）
    return
  }
  const alreadyFirst = i === 0
  if (!alreadyFirst) {
    const [hit] = list.splice(i, 1)
    list.unshift(hit)
    emojis.value = list
    try {
      await ipc.reorderEmojis(list.map((x) => x.id))
    } catch (e) {
      fail(String(e?.message || e))
    }
  }
  flash(alreadyFirst ? t('emoji.alreadyFirst') : t('emoji.moved'))
}

/** 从收藏里移除（不动表情库里的原图） */
async function menuDelete() {
  const entry = menu.value?.entry
  closeMenu()
  if (!entry) return
  if (!window.confirm(t('emoji.deleteConfirm', { n: 1 }))) return
  await deleteSticker(entry)
}

/* ---------------- 拖拽排序 ---------------- */

const dragId = ref('')

function onDragStart(e, entry) {
  dragId.value = entry.id
  e.dataTransfer?.setData('text/plain', entry.id)
  if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move'
}

async function onDrop(e, target) {
  const from = dragId.value
  dragId.value = ''
  if (!from || from === target.id) return
  const list = emojis.value.slice()
  const i = list.findIndex((x) => x.id === from)
  const j = list.findIndex((x) => x.id === target.id)
  if (i < 0 || j < 0) return
  const [moved] = list.splice(i, 1)
  list.splice(j, 0, moved)
  emojis.value = list
  try {
    await ipc.reorderEmojis(list.map((x) => x.id))
  } catch (e) {
    fail(String(e?.message || e))
  }
}

/* ---------------- 对外接口 ---------------- */

/** 打开自定义页（父组件首次展开时用） */
function showCustom() {
  view.value = 'library'
}

defineExpose({ showCustom, reload: loadList })
</script>

<template>
  <div ref="panelRef" class="emoji-panel" @mousedown.stop @wheel.stop @contextmenu.prevent>
    <!-- 内容区：固定高度，切视图不改变面板尺寸（否则表情少的视图会把面板缩成一小块） -->
    <div class="panel-body">
      <!-- 普通表情：9 列 × 40px 大格子（对齐微信表情面板） -->
      <div v-if="view === 'unicode'" class="grid uni">
        <button v-for="e in EMOJIS" :key="e" class="emoji" :title="e" @click="emit('pick', e)">{{ e }}</button>
      </div>

      <!-- ❤️ 图片表情包：与表情库同一份数据与顺序（拖拽排序、右键移到最前/删除） -->
      <div v-else-if="view === 'stickers'" class="lib-body">
        <p v-if="!emojis.length" class="empty" @contextmenu.prevent="openBlankMenu($event)">
          <span class="empty-title">{{ t('emoji.empty') }}</span>
          <span class="empty-sub">{{ t('emoji.emptyHint') }}</span>
        </p>
        <div
          v-else
          class="grid stickers"
          @scroll="closeMenu(); hoverEnd()"
          @mouseleave="hoverEnd"
          @contextmenu.prevent="openBlankMenu($event)"
        >
          <div
            v-for="e in emojis"
            :key="e.id"
            class="cell"
            :class="{ dragging: dragId === e.id }"
            draggable="true"
            :title="e.name"
            @mouseenter="hoverAt($event, e)"
            @mouseleave="hoverEnd"
            @dragstart="onDragStart($event, e)"
            @dragover.prevent
            @drop.prevent="onDrop($event, e)"
          >
            <button class="sticker" @click="emit('pickSticker', e)" @contextmenu.prevent.stop="openMenu($event, e)">
              <img v-if="thumbs[e.id] && thumbs[e.id] !== 'loading'" :src="thumbs[e.id]" :alt="e.name" />
            </button>
          </div>
        </div>

        <!-- 悬停放大预览：浮在面板内，不移开不关闭（pointer-events: none 不吃鼠标事件） -->
        <div
          v-if="hoverPreview"
          class="hover-preview"
          :style="{ left: hoverPreview.x + 'px', top: hoverPreview.y + 'px', width: PREVIEW_SIZE + 'px', height: PREVIEW_SIZE + 'px' }"
        >
          <img v-if="thumbs[hoverPreview.entry.id] && thumbs[hoverPreview.entry.id] !== 'loading'"
            :src="thumbs[hoverPreview.entry.id]" :alt="hoverPreview.entry.name" />
        </div>
      </div>

      <!-- 自定义表情库 -->
      <div v-else-if="view === 'library'" class="lib-body">
        <p v-if="!emojis.length" class="empty" @contextmenu.prevent="openBlankMenu($event)">
          <span class="empty-title">{{ t('emoji.empty') }}</span>
          <span class="empty-sub">{{ t('emoji.emptyHint') }}</span>
        </p>
        <div
          v-else
          class="grid stickers"
          @scroll="closeMenu(); hoverEnd()"
          @mouseleave="hoverEnd"
          @contextmenu.prevent="openBlankMenu($event)"
        >
          <div
            v-for="e in emojis"
            :key="e.id"
            class="cell"
            :class="{ dragging: dragId === e.id }"
            draggable="true"
            :title="e.name"
            @mouseenter="hoverAt($event, e)"
            @mouseleave="hoverEnd"
            @dragstart="onDragStart($event, e)"
            @dragover.prevent
            @drop.prevent="onDrop($event, e)"
          >
            <button class="sticker" @click="emit('pickSticker', e)" @contextmenu.prevent.stop="openMenu($event, e)">
              <img v-if="thumbs[e.id] && thumbs[e.id] !== 'loading'" :src="thumbs[e.id]" :alt="e.name" />
            </button>
          </div>
        </div>

        <!-- 悬停放大预览：浮在面板内，不移开不关闭（pointer-events: none 不吃鼠标事件） -->
        <div
          v-if="hoverPreview"
          class="hover-preview"
          :style="{ left: hoverPreview.x + 'px', top: hoverPreview.y + 'px', width: PREVIEW_SIZE + 'px', height: PREVIEW_SIZE + 'px' }"
        >
          <img v-if="thumbs[hoverPreview.entry.id] && thumbs[hoverPreview.entry.id] !== 'loading'"
            :src="thumbs[hoverPreview.entry.id]" :alt="hoverPreview.entry.name" />
        </div>
      </div>

      <!-- 表情包导入预览 -->
      <div v-else class="pack-body">
        <p v-if="pack?.loading || !pack?.info" class="tip">{{ t('emoji.packLoading') }}</p>
        <template v-else>
          <div class="pack-head">
            <span class="pack-name">{{ pack.info.name }}</span>
            <span class="pack-sum">{{ packSummary(pack.info, t).text }}</span>
          </div>
          <label class="pack-all">
            <input type="checkbox" :checked="pack.sel.size > 0 && pack.sel.size >= packSummary(pack.info, t).importable"
              @change="toggleAll" />
            {{ t('emoji.packSelectAll') }}
          </label>
          <div class="pack-list">
            <label v-for="it in packItems" :key="it.file" class="pack-row"
              :class="{ bad: !!it.problem && !it.duplicate, dup: it.duplicate }">
              <input type="checkbox" :disabled="!!it.problem" :checked="pack.sel.has(it.file)"
                @change="toggleItem(it)" />
              <span class="pack-item-name">{{ it.name }}</span>
              <span class="pack-state">{{ packItemState(it, t).text }}</span>
            </label>
          </div>
          <div class="pack-actions">
            <button class="btn-plain" @click="view = 'library'; pack = null">{{ t('cancel') }}</button>
            <button class="btn-primary" :disabled="busy || !pack.sel.size" @click="confirmPack">
              {{ t('emoji.packImport', { n: pack.sel.size }) }}
            </button>
          </div>
        </template>
      </div>
    </div>

    <!-- 底部行为条：两个图标按钮成组靠左（微信口径：普通表情 / 表情包） -->
    <div class="panel-bar">
      <button class="bar-icon" :class="{ on: view === 'unicode' }" :title="t('emoji.tabUnicode')"
        @click="view = 'unicode'">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" />
          <circle cx="9" cy="10" r="1.2" fill="currentColor" />
          <circle cx="15" cy="10" r="1.2" fill="currentColor" />
          <path d="M8.2 14c.9 1.4 2.2 2.1 3.8 2.1s2.9-.7 3.8-2.1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </button>
      <button class="bar-icon" :class="{ on: view === 'stickers' || view === 'library' || view === 'pack' }"
        :title="t('emoji.stickerHint')" @click="stickerHint">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M12 20.3l-1.4-1.3C5.6 14.6 3 12.2 3 9.1 3 6.6 4.9 4.8 7.3 4.8c1.6 0 3 .7 3.9 2 .9-1.3 2.4-2 3.9-2 2.4 0 4.3 1.8 4.3 4.3 0 3.1-2.6 5.5-7.6 9.9L12 20.3z"
            stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" />
        </svg>
      </button>
    </div>

    <!-- 提示/错误（固定一行，不改变面板高度） -->
    <p v-if="err" class="msg err">{{ err }}</p>
    <p v-else-if="hint" class="msg">{{ hint }}</p>

    <!-- 右键菜单：图片表情上 → 移到最前 / 删除；空白处 → 只有导入导出 -->
    <div v-if="menu" ref="menuRef" class="menu" :style="{ left: menu.x + 'px', top: menu.y + 'px' }">
      <template v-if="menu.entry">
        <button @click="menuMoveFront">{{ t('emoji.moveFront') }}</button>
        <button class="danger" @click="menuDelete">{{ t('emoji.delete') }}</button>
        <span class="menu-sep"></span>
      </template>
      <button :disabled="busy" @click="menuImportImages">＋ {{ t('emoji.importImages') }}</button>
      <button :disabled="busy" @click="menuImportPack">⇩ {{ t('emoji.importPack') }}</button>
      <button :disabled="busy || !emojis.length" @click="menuExportPack">⇧ {{ t('emoji.exportPack') }}</button>
    </div>
</div>
</template>

<style scoped>
/* 定位（position/left/top/max-height）由父组件按触发按钮位置以 inline style 注入 */
.emoji-panel {
  /* 与微信表情面板同口径：9 列 × 40px 格子 + 4px 间隔（见 .uni / .emoji）。
     高度固定，切到表情少的视图也不会缩成一小块。 */
  --emoji-cols: 9;
  --emoji-cell: 40px;
  --emoji-gap: 4px;
  /* 宽度按「9 列刚好放下 + 预留纵向滚动条」反算，横向永不裁切也不会出横向滚动条：
     边框 1×2 + 行内边距 9×2 + 网格 9×40 + 间隔 8×4 + 滚动条 ≈10 = 2+18+360+32+10 = 422。
     （曾经的 bug：404px 面板装不下 392px 网格，第 9 列被裁 12px；
       后来 412px 在「内容多到出纵向滚动条」时又被滚动条吃掉 6px） */
  --panel-body-h: 352px;
  width: 422px;
  padding: 10px 9px 8px;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 8px;
  box-shadow: 0 6px 24px var(--c-shadow);
  display: flex;
  flex-direction: column;
  gap: 8px;
  z-index: 30;
  box-sizing: border-box;
  overflow: visible;
}
.panel-body {
  height: var(--panel-body-h);
  overflow: hidden; /* 只纵向滚动，横向永不裁切 */
  display: flex;
  flex-direction: column;
}
.panel-body > * {
  min-height: 0;
}
.grid {
  display: grid;
  gap: 2px;
}
.uni {
  /* 固定轨道（与微信口径一致）：列宽 = 格子宽，配合上面反算的面板宽度刚好铺满 */
  grid-template-columns: repeat(var(--emoji-cols), var(--emoji-cell));
  gap: var(--emoji-gap);
  justify-content: space-between;
  overflow-y: auto; /* 行数多时纵向滚动 */
  overflow-x: hidden;
  align-content: start;
}
.emoji {
  width: var(--emoji-cell);
  height: var(--emoji-cell);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 24px;
  line-height: 1;
  border-radius: 6px;
}
.emoji:hover {
  background: var(--c-list-hover);
}
.stickers {
  /* 固定格子尺寸（不能用 1fr：那样只会把格子越挤越小，也不会滚动）。
     宽度要预留纵向滚动条（WebKitGTK 下约占 10px）：
     5 列 × 71 + 4 × 6 = 379 ≤ 可用 382 → 出滚动条时也不横向截断 */
  --sticker-cols: 5;
  --sticker-cell: 71px;
  grid-template-columns: repeat(var(--sticker-cols), var(--sticker-cell));
  grid-auto-rows: var(--sticker-cell);
  justify-content: space-between;
  align-content: start;
  gap: 6px;
  flex: 1;
  min-height: 0; /* flex 子项默认 min-height:auto 会把网格压扁成不滚动 */
  overflow-y: auto;
  overflow-x: hidden;
}
/* 图片表情用大方格子（微信表情包页的观感），不用表情区的 40px 小格 */
.cell {
  position: relative;
  aspect-ratio: 1 / 1;
}
.cell.dragging {
  opacity: 0.4;
}
.sticker {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  overflow: hidden;
}
.cell:hover .sticker {
  background: var(--c-list-hover);
}
.sticker img {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  pointer-events: none;
}
/* 悬停放大预览：与微信一致——200×200 大图浮在面板内 */
.hover-preview {
  position: absolute;
  z-index: 45;
  padding: 6px;
  box-sizing: border-box;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 8px;
  box-shadow: 0 8px 24px var(--c-shadow);
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}
.hover-preview img {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
}
.lib-body,
.pack-body {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-height: 0;
  flex: 1;
}
.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 16px 8px;
  text-align: center;
  color: var(--c-sub);
  font-size: 12px;
}
.empty-title {
  color: var(--c-text);
  font-size: 13px;
}
.tip {
  font-size: 11px;
  line-height: 15px;
  color: var(--c-sub);
  margin: 0;
}
.msg {
  font-size: 11px;
  line-height: 15px;
  color: var(--c-sub);
  margin: 0;
  word-break: break-all;
  max-height: 45px;
  overflow: hidden;
}
.msg.err {
  color: var(--c-danger, #d9534f);
}
/* 底部行为条：三个视图图标 + 自定义库的导入导出，全部靠左成组，不分散到两端 */
.panel-bar {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 6px;
  flex-wrap: wrap;
  padding-top: 6px;
  border-top: 1px solid var(--c-hairline);
  flex: none;
}
.bar-icon {
  width: 34px;
  height: 30px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  color: var(--c-sub);
  outline: none; /* 选中态由 .on 表达；鼠标点击不画焦点方块 */
}
.bar-icon:focus-visible {
  outline: 1px solid var(--c-accent);
  outline-offset: 1px;
}
.bar-icon:hover {
  background: var(--c-list-hover);
}
.bar-icon.on {
  color: var(--c-text);
  background: var(--c-list-hover);
}
.bar-btn {
  font-size: 11px;
  line-height: 24px;
  border-radius: 4px;
  border: 1px solid var(--c-hairline);
  color: var(--c-text);
  white-space: nowrap;
}
.bar-btn:hover:not(:disabled) {
  background: var(--c-list-hover);
}
.bar-btn:disabled {
  opacity: 0.5;
}
.pack-head {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.pack-name {
  font-size: 12px;
  font-weight: 600;
  color: var(--c-text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pack-sum {
  font-size: 11px;
  color: var(--c-sub);
}
.pack-all {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  color: var(--c-sub);
  cursor: pointer;
}
.pack-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  border: 1px solid var(--c-hairline);
  border-radius: 4px;
  padding: 2px;
}
.pack-row {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  line-height: 20px;
  padding: 0 4px;
  border-radius: 3px;
  cursor: pointer;
}
.pack-row:hover {
  background: var(--c-list-hover);
}
.pack-row.bad,
.pack-row.dup {
  color: var(--c-sub);
}
.pack-item-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pack-state {
  flex: none;
  font-size: 10px;
  opacity: 0.85;
}
.pack-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
}
.menu {
  position: absolute;
  min-width: 104px;
  padding: 4px;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 6px;
  box-shadow: 0 6px 20px var(--c-shadow);
  display: flex;
  flex-direction: column;
  z-index: 40;
}
.menu button {
  text-align: left;
  font-size: 12px;
  line-height: 24px;
  padding: 0 8px;
  border-radius: 4px;
  white-space: nowrap;
}
.menu button:hover {
  background: var(--c-list-hover);
}
.menu-sep {
  height: 1px;
  margin: 3px 4px;
  background: var(--c-hairline);
}
.menu button:disabled {
  opacity: 0.5;
}
.menu button.danger {
  color: var(--c-danger, #d9534f);
}
</style>
