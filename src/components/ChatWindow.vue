<script setup>
// 右侧聊天窗口：头部 / 消息流（日期分隔+气泡）/ 工具栏 / 输入区
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import * as ipc from '../lib/ipc'
import {
  store, sendText, sendFiles, sendClipboardImage, downloadFile, clearHistory,
  openChat, displayName, dayLabel, fmtTime, fmtSize, refreshUsers, splitDelayedNote,
} from '../store'
import { parseFileUris, highlightParts } from '../lib/text'
import { open as openFileDialog, confirm as confirmDialog } from '@tauri-apps/plugin-dialog'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import { getCurrentWindow } from '@tauri-apps/api/window'
import Avatar from './Avatar.vue'
import EmojiPicker from './EmojiPicker.vue'

const activeUser = computed(
  () => store.userMap[store.activeKey] || store.peerMeta[store.activeKey] || null
)
const isOnline = computed(() => !!store.userMap[store.activeKey])
const msgs = computed(() => store.chats[store.activeKey]?.msgs || [])

/* ---------- 渲染列表：插入日期分隔 + 连续消息聚合 ---------- */
const viewList = computed(() => {
  const out = []
  let prev = null
  for (const m of msgs.value) {
    if (!prev || dayChanged(prev, m)) {
      out.push({ kind: 'day', id: 'day-' + m.ts + '-' + out.length, label: dayLabel(m.ts) })
    }
    const dayInserted = out.length && out[out.length - 1].kind === 'day'
    const merged =
      prev && !dayInserted &&
      prev.dir === m.dir && prev.kind === m.kind && m.ts - prev.ts < 180
    out.push({ kind: 'msg', id: 'm-' + m.pkt + '-' + out.length, m, firstOfCluster: !merged })
    prev = m
  }
  return out
})
function dayChanged(a, b) {
  return new Date(a.ts * 1000).toDateString() !== new Date(b.ts * 1000).toDateString()
}

/* ---------- 滚动控制 ---------- */
const scroller = ref(null)
let autoBottom = true
function onScroll() {
  const el = scroller.value
  if (!el) return
  autoBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 90
}
function scrollBottom(smooth = false) {
  nextTick(() => {
    const el = scroller.value
    if (!el || !autoBottom) return
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' })
  })
}
watch(() => store.activeKey, () => {
  autoBottom = true
  scrollBottom()
})
watch(() => msgs.value.length, () => scrollBottom(true))

/* ---------- 发送 ---------- */
const draft = ref('')
const ta = ref(null)

watch(() => store.activeKey, () => nextTick(() => ta.value?.focus()))

const canSend = computed(() => !!draft.value.trim() || !!pendingImg.value)

async function doSend() {
  if (!store.activeKey || !canSend.value) return
  const text = draft.value.replace(/\n{3,}/g, '\n\n').trimEnd()
  const img = pendingImg.value
  try {
    if (img) {
      // 图片与随行文字一并发出（IPMsg 的一条消息可同时带正文和附件）
      await sendClipboardImage(img.b64, img.mime, text)
      clearPendingImg()
    } else {
      await sendText(text)
    }
    draft.value = ''
    autoBottom = true
  } catch (e) {
    alert('发送失败：' + e)
  }
}

/* ---------- 剪贴板图片 ---------- */
const pendingImg = ref(null)

function clearPendingImg() {
  if (pendingImg.value?.url) URL.revokeObjectURL(pendingImg.value.url)
  pendingImg.value = null
}

/** 大数组分块转 base64，避免 String.fromCharCode 参数过多爆栈 */
function bytesToB64(bytes) {
  let bin = ''
  const step = 0x8000
  for (let i = 0; i < bytes.length; i += step) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + step))
  }
  return btoa(bin)
}

async function takeImageFile(file) {
  if (!file) return false
  if (file.size > 32 * 1024 * 1024) {
    alert('图片超过 32MB，请改用「发送文件」')
    return false
  }
  const buf = new Uint8Array(await file.arrayBuffer())
  clearPendingImg()
  pendingImg.value = {
    b64: bytesToB64(buf),
    mime: file.type || 'image/png',
    size: buf.length,
    url: URL.createObjectURL(file),
  }
  nextTick(() => ta.value?.focus())
  return true
}

/**
 * 粘贴处理（窗口级，Ctrl+V 在哪都生效）：
 *  1. clipboardData 里就有文件路径 → 和拖放一样直接发送
 *  2. clipboardData 什么都没有（Linux/WebKitGTK 不暴露文件类剪贴板）→ 读原生 GTK 剪贴板
 *  3. 只有文件内容没有路径（Windows 资源管理器 / 邮件客户端）→ 落盘后发送
 *  4. 只是一张位图（截图工具）→ 转成待发送附件，先预览再按 Enter 发
 *  5. 普通文字 → 保持默认粘贴行为
 */
async function onPaste(e) {
  const dt = e.clipboardData
  if (!dt) return
  // 记下网页层是否真的拿到了可用内容：拿不到时由 Ctrl+V 兜底去问原生剪贴板
  const files0 = Array.from(dt.items || []).filter((it) => it.kind === 'file')
  if (dt.getData('text/plain') || dt.getData('text/uri-list') || files0.length) {
    pasteSeen = true
  }

  // 1) 有路径：零拷贝，直接走发送文件通道
  const paths = parseFileUris(dt.getData('text/uri-list') || dt.getData('text/plain') || '')
  if (paths.length) {
    e.preventDefault()
    await sendPastedPaths(paths)
    return
  }

  const files = Array.from(dt.items || [])
    .filter((it) => it.kind === 'file')
    .map((it) => it.getAsFile())
    .filter(Boolean)

  if (!files.length) return
  e.preventDefault()

  // 4) 单张位图（截图工具剪贴板里的图，浏览器给的名字是空或 image.png）
  //    → 转成待发送附件先预览，避免误粘就发出去
  const f0 = files[0]
  const isScreenshot =
    files.length === 1 &&
    f0.type.startsWith('image/') &&
    (!f0.name || /^image\.(png|jpe?g|gif|bmp|webp)$/i.test(f0.name))
  if (isScreenshot) {
    await takeImageFile(f0)
    return
  }

  // 3) 有内容没路径（Windows 资源管理器 / 邮件客户端）：落盘后按普通附件发送
  try {
    const staged = []
    for (const f of files) {
      const buf = new Uint8Array(await f.arrayBuffer())
      staged.push(await ipc.stagePastedFile(f.name || '粘贴文件', bytesToB64(buf)))
    }
    await sendPastedPaths(staged)
  } catch (err) {
    alert('粘贴发送失败：' + err)
  }
}

/**
 * Ctrl/⌘+V 兜底：Linux 的文件管理器复制文件时，剪贴板里只有 text/uri-list，
 * WebKitGTK 既不会把它交给网页，往往连 paste 事件都不触发 —— 所以不能只等
 * paste 事件。这里在按键后确认网页层确实没拿到内容，再去读原生 GTK 剪贴板。
 */
let pasteSeen = false
function onPasteHotkey(e) {
  if (!(e.ctrlKey || e.metaKey) || e.key?.toLowerCase() !== 'v' || e.altKey) return
  pasteSeen = false
  setTimeout(async () => {
    if (pasteSeen) return // 网页层已经处理（普通文本或文件）
    try {
      const native = await ipc.clipboardFilePaths()
      if (native?.length) await sendPastedPaths(native)
    } catch (err) {
      console.error('clipboard_file_paths failed', err)
    }
  }, 80)
}

/** 粘贴到的文件统一从这里发出（与拖放同一条通道） */
async function sendPastedPaths(paths) {
  if (!store.activeKey) {
    alert('请先在左侧选择要发送给谁')
    return
  }
  try {
    await sendFiles(paths)
    autoBottom = true
  } catch (e) {
    alert('发送失败：' + e)
  }
}

/* ---------- 拖放文件发送 ----------
   Tauri 下系统文件的拖放由原生层接管（webview 的 drop 事件拿不到文件路径），
   必须用窗口的 onDragDropEvent，它给的是真实绝对路径，可直接交给 send_files。*/
const dragOver = ref(false)
/** 拖放的落点对应的会话 key：悬停在左侧某个用户上就发给他，否则发给当前会话 */
const dropTarget = ref('')
let unlistenDrop = null

/** 把拖放的物理坐标换算成页面元素，判断落点是不是左侧某个用户 */
function keyAtPoint(pos) {
  if (!pos) return store.activeKey || ''
  const dpr = window.devicePixelRatio || 1
  const el = document.elementFromPoint(pos.x / dpr, pos.y / dpr)
  const row = el && el.closest ? el.closest('[data-user-key]') : null
  return row?.dataset.userKey || store.activeKey || ''
}

onMounted(async () => {
  try {
    unlistenDrop = await getCurrentWindow().onDragDropEvent(async ({ payload }) => {
      if (payload.type === 'over') {
        dragOver.value = true
        dropTarget.value = keyAtPoint(payload.position)
        return
      }
      if (payload.type !== 'drop') {
        dragOver.value = false
        return
      }
      const target = keyAtPoint(payload.position)
      dragOver.value = false
      const paths = (payload.paths || []).filter(Boolean)
      if (!paths.length) return
      if (!target) {
        alert('请先在左侧选择要发送给谁')
        return
      }
      try {
        // 文件与文件夹都走同一条附件通道（目录会以 GETDIRFILES 流发送）
        await sendFiles(paths, target)
        autoBottom = true
        // 拖给的是别的联系人：切过去，让用户看到确实发出去了
        if (target !== store.activeKey) await openChat(target)
      } catch (e) {
        alert('发送失败：' + e)
      }
    })
  } catch (e) {
    /* 拿不到窗口事件时静默降级：仍可用「发送文件」按钮 */
  }
})
function onFindHotkey(e) {
  const k = e.key?.toLowerCase()
  if ((e.ctrlKey || e.metaKey) && k === 'f' && store.activeKey) {
    e.preventDefault()
    openFind()
  } else if (e.key === 'Escape' && findOpen.value) {
    closeFind()
  }
}

onMounted(() => {
  window.addEventListener('paste', onPaste)
  window.addEventListener('keydown', onPasteHotkey)
  window.addEventListener('keydown', onFindHotkey)
})
onUnmounted(() => {
  if (unlistenDrop) unlistenDrop()
  window.removeEventListener('paste', onPaste)
  window.removeEventListener('keydown', onPasteHotkey)
  window.removeEventListener('keydown', onFindHotkey)
})
function onKeydown(e) {
  // Enter 发送；Ctrl/Cmd+Enter 与 Shift+Enter 换行（微信PC习惯）
  if (e.key === 'Enter' && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.altKey) {
    e.preventDefault()
    doSend()
  }
}

async function pickFiles() {
  if (!store.activeKey) return
  try {
    const sel = await openFileDialog({ multiple: true, title: '选择要发送的文件' })
    if (!sel) return
    await sendFiles(Array.isArray(sel) ? sel : [sel])
    autoBottom = true
  } catch (e) {
    alert('发送文件失败：' + e)
  }
}

async function pickFolder() {
  if (!store.activeKey) return
  try {
    const sel = await openFileDialog({ multiple: true, directory: true, title: '选择要发送的文件夹' })
    if (!sel) return
    await sendFiles(Array.isArray(sel) ? sel : [sel])
    autoBottom = true
  } catch (e) {
    alert('发送文件夹失败：' + e)
  }
}

/* ---------- 表情 ---------- */
const emojiOpen = ref(false)
function insertEmoji(e) {
  const el = ta.value
  if (!el) return
  const s = el.selectionStart ?? draft.value.length
  draft.value = draft.value.slice(0, s) + e + draft.value.slice(el.selectionEnd ?? s)
  nextTick(() => {
    el.focus()
    el.selectionStart = el.selectionEnd = s + e.length
  })
}

/* ---------- 文件卡片动作 ---------- */
function pct(f) {
  if (!f.total) return 0
  return Math.min(100, Math.round(((f.transferred || 0) / f.total) * 100))
}
/** 单击聊天里的图片 → 在独立窗口打开（仿微信） */
async function viewImage(f) {
  if (!f.path) return
  try {
    await ipc.openImageViewer(f.path, f.name)
  } catch (e) {
    alert('打开图片失败：' + e)
  }
}
async function openFile(path) {
  try { await openPath(path) } catch (e) { alert('打开失败：' + e) }
}
async function revealFile(path) {
  try { await revealItemInDir(path) } catch (e) { alert('打开文件夹失败：' + e) }
}

/* ---------- 会话内查找（Ctrl+F）与搜索结果定位 ---------- */
const findOpen = ref(false)
const findQuery = ref('')
const findInput = ref(null)
const findIdx = ref(0)
/** 高亮用的关键词：查找栏优先，其次是从中栏搜索结果跳进来的词 */
const hlQuery = computed(() => findQuery.value.trim() || locateQuery.value)
const locateQuery = ref('')
const locatePkt = ref(0)

/** 当前会话里命中关键词的消息（按显示顺序） */
const findHits = computed(() => {
  const q = findQuery.value.trim().toLowerCase()
  if (!q) return []
  return msgs.value.filter((m) => {
    if ((bodyOf(m) || '').toLowerCase().includes(q)) return true
    return (m.files || []).some((f) => (f.name || '').toLowerCase().includes(q))
  })
})

function openFind() {
  findOpen.value = true
  nextTick(() => findInput.value?.focus())
}
function closeFind() {
  findOpen.value = false
  findQuery.value = ''
  findIdx.value = 0
}
function stepFind(delta) {
  const n = findHits.value.length
  if (!n) return
  findIdx.value = (findIdx.value + delta + n) % n
  scrollToPkt(findHits.value[findIdx.value].pkt)
}
watch(findHits, (hits) => {
  findIdx.value = 0
  // 输入过程中自动跳到最后一条命中（最新的那条更可能是想找的）
  if (hits.length) {
    findIdx.value = hits.length - 1
    scrollToPkt(hits[findIdx.value].pkt)
  }
})

/** 滚动到指定包号的消息并闪一下 */
function scrollToPkt(pkt) {
  if (!pkt) return
  nextTick(() => {
    const el = scroller.value?.querySelector(`[data-pkt="${pkt}"]`)
    if (!el) return
    autoBottom = false
    el.scrollIntoView({ block: 'center', behavior: 'smooth' })
    el.classList.remove('flash')
    // 强制重排，保证连续定位同一条时动画能重放
    void el.offsetWidth
    el.classList.add('flash')
  })
}

// 中栏点开某条搜索结果：定位 + 高亮关键词
watch(
  () => store.locate,
  async (loc) => {
    if (!loc || loc.key !== store.activeKey) return
    locateQuery.value = loc.query || ''
    locatePkt.value = loc.pkt
    await nextTick()
    scrollToPkt(loc.pkt)
    store.locate = null
  }
)
// 切换会话时清掉上一次的定位高亮
watch(() => store.activeKey, () => {
  locateQuery.value = ''
  locatePkt.value = 0
  closeFind()
})

/** 附件名也参与高亮（模板里直接用） */
// eslint-disable-next-line no-unused-vars
const _hl = highlightParts

/** 正文按关键词切片，供模板高亮 */
function textParts(m) {
  return highlightParts(bodyOf(m), hlQuery.value)
}

/* ---------- 「延迟发送」尾注 ---------- */
const noteCache = new Map()
function noteOf(m) {
  const t = m.text || ''
  let v = noteCache.get(t)
  if (!v) {
    v = splitDelayedNote(t)
    noteCache.set(t, v)
  }
  return v
}
const bodyOf = (m) => noteOf(m).body
const delayedOf = (m) => noteOf(m).delayed

/* ---------- 清空聊天记录 ---------- */
async function doClearHistory() {
  const key = store.activeKey
  if (!key) return
  const who = displayName(key)
  const ok = await confirmDialog(
    `确定清空与「${who}」的聊天记录吗？\n本地记录将被删除且无法恢复（不影响对方）。`,
    { title: '清空聊天记录', kind: 'warning', okLabel: '清空', cancelLabel: '取消' }
  )
  if (!ok) return
  try {
    await clearHistory(key)
  } catch (e) {
    alert('清空失败：' + e)
  }
}

/* ---------- 图片内联预览 ---------- */
const IMG_RE = /\.(png|jpe?g|gif|bmp|webp)$/i
function isImg(name) {
  return IMG_RE.test(name || '')
}

/** 本地已有图片内容时加载 base64 预览（发出即显；接收在下载完成后显） */
async function ensureImg(m, f) {
  if (!isImg(f.name) || f.src || f._imgLoading) return
  if (f.size > 32 * 1024 * 1024) return // 超大图不做内联预览
  const path = f.path
  if (!path) return
  if (m.dir === 'in' && f.state !== 'done') return
  f._imgLoading = true
  try {
    const r = await invoke('read_image_data', { path })
    f.mime = r.mime
    f.src = `data:${r.mime};base64,${r.b64}`
    scrollBottom(true)
  } catch {
    /* 预览失败静默降级为文件卡片 */
  } finally {
    f._imgLoading = false
  }
}

watch(
  msgs,
  (list) => {
    for (const m of list) for (const f of m.files || []) ensureImg(m, f)
  },
  { deep: true, immediate: true }
)
</script>

<template>
  <section class="chat-window">
    <!-- 头部 -->
    <header v-if="activeUser" class="cw-head">
      <div class="peer">
        <div class="name">{{ displayName(store.activeKey) }}</div>
        <div class="sub">
          <i class="stat" :class="isOnline ? 'on' : 'off'">{{ isOnline ? '● 在线' : '● 离线' }}</i>
          {{ activeUser.host || '' }}<template v-if="activeUser.ip"> · {{ activeUser.ip }}</template>
          <template v-if="activeUser.group"> · {{ activeUser.group }}</template>
        </div>
      </div>
      <div class="head-actions">
        <button class="mini-btn" title="清空聊天记录" @click="doClearHistory">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none">
            <path d="M4 7h16M9 7V5h6v2M6 7l1 13h10l1-13" stroke="currentColor" stroke-width="1.8"
              stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
        <button class="mini-btn" title="重新广播上线，刷新在线用户" @click="refreshUsers">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none">
            <path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
          </svg>
        </button>
      </div>
    </header>

    <!-- 拖放提示遮罩 -->
    <div v-if="dragOver" class="drop-mask">
      <div class="drop-card">
        <svg width="34" height="34" viewBox="0 0 24 24" fill="none">
          <path d="M12 16V4m0 0L7.5 8.5M12 4l4.5 4.5" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" />
          <path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" />
        </svg>
        <p v-if="dropTarget">松开即发送给「{{ displayName(dropTarget) }}」</p>
        <p v-else>请先在左侧选择要发送给谁</p>
        <span class="sub">支持多个文件与文件夹</span>
      </div>
    </div>

    <!-- 会话内查找 -->
    <div v-if="activeUser && findOpen" class="findbar">
      <input
        ref="findInput"
        v-model="findQuery"
        placeholder="在本会话中查找"
        spellcheck="false"
        @keydown.enter.prevent="stepFind(1)"
        @keydown.esc.prevent="closeFind"
      />
      <span class="cnt">{{ findHits.length ? findIdx + 1 : 0 }}/{{ findHits.length }}</span>
      <button title="上一个" :disabled="!findHits.length" @click="stepFind(-1)">∧</button>
      <button title="下一个（Enter）" :disabled="!findHits.length" @click="stepFind(1)">∨</button>
      <button title="关闭（Esc）" @click="closeFind">✕</button>
    </div>

    <!-- 消息区 -->
    <div v-if="activeUser" ref="scroller" class="msgs" @scroll="onScroll">
      <div v-for="v in viewList" :key="v.id">
        <div v-if="v.kind === 'day'" class="day-sep"><span>{{ v.label }}</span></div>

        <div
          v-else
          class="msg-row"
          :data-pkt="v.m.pkt"
          :class="[v.m.dir === 'out' ? 'self' : 'peer', { merge: !v.firstOfCluster }]"
        >
          <Avatar class="m-ava" :name="v.m.dir === 'out' ? store.config?.nickname : displayName(store.activeKey)"
            :seed="v.m.dir === 'out' ? 'self' : store.activeKey" :size="34" />
          <div class="bubble-wrap">
            <div class="bubble" :class="{ file: v.m.kind === 'file' }">
              <div v-if="bodyOf(v.m)" class="b-text">
                <template v-if="hlQuery">
                  <span v-for="(p, i) in textParts(v.m)" :key="i" :class="{ hl: p.hit }">{{ p.text }}</span>
                </template>
                <template v-else>{{ bodyOf(v.m) }}</template>
              </div>
              <template v-for="f in v.m.files || []" :key="f.id">
                <!-- 图片：本地已有内容时直接内联预览，点击查看原图 -->
                <div v-if="isImg(f.name) && f.src" class="img-wrap">
                  <img :src="f.src" class="chat-img" title="点击在新窗口查看原图" @click="viewImage(f)" />
                </div>
                <!-- 无预览时显示文件卡片（下载中/失败/非图片/超大图） -->
                <div v-else class="file-card">
                <div class="fc-icon">
                  <svg v-if="f.dir_entry" width="26" height="26" viewBox="0 0 24 24" fill="none">
                    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"
                      stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
                  </svg>
                  <svg v-else width="26" height="26" viewBox="0 0 24 24" fill="none">
                    <path d="M6 3h8l4 4v14H6V3z" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
                    <path d="M14 3v4h4" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
                  </svg>
                </div>
                <div class="fc-main">
                  <div class="fc-name ellipsis" :title="f.name">
                    <template v-if="hlQuery">
                      <span v-for="(p, i) in highlightParts(f.name, hlQuery)" :key="i" :class="{ hl: p.hit }">{{ p.text }}</span>
                    </template>
                    <template v-else>{{ f.name }}</template>
                  </div>
                  <div class="fc-sub">
                    <span>{{ fmtSize(f.size) }}</span>
                    <!-- 收到的文件 -->
                    <template v-if="v.m.dir === 'in'">
                      <template v-if="f.state === 'pending'">
                        <a @click.prevent="downloadFile(v.m, f)">下载</a>
                      </template>
                      <template v-else-if="f.state === 'downloading'">
                        <span>{{ pct(f) }}%</span>
                        <i class="bar"><i :style="{ width: pct(f) + '%' }"></i></i>
                      </template>
                      <template v-else-if="f.state === 'done'">
                        <span class="ok">{{ f.dir_entry ? '文件夹已保存' : '已保存' }}</span>
                        <a @click.prevent="openFile(f.path)">打开</a>
                        <a @click.prevent="revealFile(f.path)">所在文件夹</a>
                      </template>
                      <template v-else-if="f.state === 'failed'">
                        <span class="err">失败</span>
                        <a @click.prevent="downloadFile(v.m, f)">重试</a>
                      </template>
                    </template>
                    <!-- 发出的文件 -->
                    <template v-else>
                      <span class="ok">已发送</span>
                      <a @click.prevent="revealFile(f.path)">所在文件夹</a>
                    </template>
                  </div>
                </div>
                </div>
              </template>
            </div>
            <!-- 对端「延迟发送/离线留言」的尾注：收成一个小标记，不占正文 -->
            <div v-if="delayedOf(v.m) !== null" class="delay-tag" :title="'对方在 ' + delayedOf(v.m) + ' 发出，你当时不在线，上线后才补投'">
              离线留言 · 原发送时间 {{ delayedOf(v.m) || '未知' }}
            </div>
            <div class="m-time" :class="{ self: v.m.dir === 'out' }">
              <span v-if="v.m.dir === 'out' && v.m.rcpt" class="read-tag" :class="{ done: v.m.read }">
                {{ v.m.read ? '已读' : '未读' }}
              </span>
              {{ fmtTime(v.m.ts) }}
            </div>
          </div>
        </div>
      </div>
      <div style="height: 10px"></div>
    </div>

    <!-- 空态 -->
    <div v-else class="placeholder">
      <svg width="72" height="72" viewBox="0 0 24 24" fill="none">
        <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.6c-.5.42-1.3.07-1.3-.6V5.5z"
          stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" />
      </svg>
      <p>选择一个会话，开始聊天</p>
      <p class="sub">局域网内基于 IPMsg 协议（UDP/TCP 2425）</p>
    </div>

    <!-- 输入区 -->
    <footer v-if="activeUser" class="composer">
      <div class="toolbar">
        <button title="表情" @click="emojiOpen = !emojiOpen">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" />
            <circle cx="9" cy="10" r="1.2" fill="currentColor" />
            <circle cx="15" cy="10" r="1.2" fill="currentColor" />
            <path d="M8.2 14c.9 1.4 2.2 2.1 3.8 2.1s2.9-.7 3.8-2.1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button title="发送文件" @click="pickFiles">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M11 5.5A3.5 3.5 0 0 1 17.9 7l.6 6.5a5.5 5.5 0 0 1-11 .5L7 8" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" transform="rotate(45 12 12)" />
            <path d="M13.5 8.5l-5 5a2.5 2.5 0 0 0 3.5 3.5l5.5-5.5a4 4 0 1 0-5.7-5.7L6 11" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button title="发送文件夹" @click="pickFolder">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"
              stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
          </svg>
        </button>
        <button class="disabled" title="截图功能开发中" disabled>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <rect x="3" y="6" width="18" height="14" rx="2" stroke="currentColor" stroke-width="1.6" />
            <path d="M8 6l1.5-2.5h5L16 6" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
            <circle cx="12" cy="13" r="3.4" stroke="currentColor" stroke-width="1.6" />
          </svg>
        </button>
      </div>

      <EmojiPicker v-if="emojiOpen" @pick="insertEmoji" />

      <!-- 待发送的剪贴板图片 -->
      <div v-if="pendingImg" class="paste-strip">
        <img :src="pendingImg.url" class="paste-thumb" />
        <div class="paste-meta">
          <div>剪贴板图片</div>
          <div class="paste-sub">{{ fmtSize(pendingImg.size) }} · Enter 发送</div>
        </div>
        <button class="paste-x" title="取消" @click="clearPendingImg">✕</button>
      </div>

      <textarea
        ref="ta"
        v-model="draft"
        class="input-area"
        placeholder="输入消息…"
        spellcheck="false"
        @keydown="onKeydown"
      ></textarea>

      <div class="composer-foot">
        <span class="hint">Enter 发送 / Ctrl+Enter 换行 / 可直接粘贴图片</span>
        <button class="send-btn" :disabled="!canSend" @click="doSend">
          发送<span class="s-key">(S)</span>
        </button>
      </div>
    </footer>
  </section>
</template>

<style scoped>
.findbar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: var(--c-card);
  border-bottom: 1px solid var(--c-hairline);
}
.findbar input {
  flex: 1;
  min-width: 0;
  height: 26px;
  padding: 0 8px;
  border: 1px solid var(--c-border);
  border-radius: 4px;
  background: var(--c-card);
  font-size: 13px;
}
.findbar .cnt {
  flex: none;
  font-size: 12px;
  color: var(--c-sub);
  min-width: 42px;
  text-align: center;
}
.findbar button {
  flex: none;
  width: 26px;
  height: 26px;
  border-radius: 4px;
  color: var(--c-sub);
}
.findbar button:hover:not(:disabled) {
  background: var(--c-hover);
  color: var(--c-text);
}
.findbar button:disabled {
  opacity: 0.35;
  cursor: default;
}
/* 命中关键词 */
.fc-name .hl,
.b-text .hl {
  background: #ffe08a;
  color: #191919;
  border-radius: 2px;
}
:root[data-theme='dark'] .fc-name .hl,
.b-text .hl {
  background: #8a6d1f;
  color: #fff;
}
/* 定位到某条消息时闪一下 */
.msg-row.flash .bubble {
  animation: flash-hit 1.2s ease-out 1;
}
@keyframes flash-hit {
  0%,
  40% {
    box-shadow: 0 0 0 3px var(--c-accent);
  }
  100% {
    box-shadow: 0 0 0 0 transparent;
  }
}
.drop-mask {
  position: absolute;
  inset: 0;
  z-index: 30;
  /* 不挡住命中判定：拖放落点要能穿透遮罩找到底下的联系人行 */
  pointer-events: none;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--c-mask-soft);
  backdrop-filter: blur(1px);
}
.drop-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 22px 34px;
  border: 2px dashed var(--c-accent);
  border-radius: 10px;
  color: var(--c-accent);
  background: var(--c-card);
  font-size: 14px;
}
.drop-card .sub {
  font-size: 11.5px;
  color: var(--c-sub);
}
.delay-tag {
  margin-top: 3px;
  font-size: 11px;
  color: var(--c-sub);
  cursor: help;
}
.paste-strip {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 0 12px 6px;
  padding: 6px 8px;
  border: 1px solid var(--c-line, var(--c-hairline));
  border-radius: 6px;
  background: var(--c-tint);
}
.paste-thumb {
  width: 46px;
  height: 46px;
  object-fit: cover;
  border-radius: 4px;
  flex: none;
}
.paste-meta {
  flex: 1;
  min-width: 0;
  font-size: 12px;
}
.paste-sub {
  color: var(--c-sub);
  margin-top: 2px;
}
.paste-x {
  flex: none;
  border: none;
  background: none;
  cursor: pointer;
  color: var(--c-sub);
  font-size: 13px;
  padding: 4px 6px;
}
.paste-x:hover {
  color: var(--c-text);
}
.chat-window {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--c-chat);
  position: relative;
}
.cw-head {
  flex: none;
  height: 52px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 14px;
  border-bottom: 1px solid var(--c-hairline);
}
.cw-head .peer {
  flex: 1;
  min-width: 0;
}
/* 头部按钮成组靠右，彼此挨着 */
.head-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 2px;
}
.peer .name {
  font-size: 15px;
  font-weight: 600;
}
.peer .sub {
  font-size: 11.5px;
  color: var(--c-sub);
}
.stat {
  font-style: normal;
  font-size: 11px;
  margin-right: 6px;
}
.stat.on {
  color: var(--c-accent);
}
.stat.off {
  color: var(--c-weak);
}
.mini-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  color: var(--c-sub);
  display: flex;
  align-items: center;
  justify-content: center;
}
.mini-btn:hover {
  background: var(--c-hover);
  color: var(--c-text);
}

.msgs {
  flex: 1;
  overflow-y: auto;
  padding: 16px 18px 4px;
}
.day-sep {
  text-align: center;
  margin: 8px 0 14px;
}
.day-sep span {
  font-size: 11px;
  color: var(--c-sub);
  background: var(--c-tint);
  border-radius: 3px;
  padding: 2px 8px;
}
.msg-row {
  display: flex;
  gap: 10px;
  margin-bottom: 4px;
}
.msg-row.merge {
  margin-top: -2px;
}
.msg-row.self {
  flex-direction: row-reverse;
}
.m-ava {
  margin-top: 2px;
}
.bubble-wrap {
  max-width: 62%;
  min-width: 0;
  display: inline-flex;
  flex-direction: column;
}
.msg-row.peer .bubble-wrap {
  align-items: flex-start;
}
.msg-row.self .bubble-wrap {
  align-items: flex-end;
}
.bubble {
  position: relative;
  background: var(--c-bubble-peer);
  border-radius: 6px;
  padding: 8px 11px;
  line-height: 1.55;
  font-size: 14px;
  word-break: break-word;
  white-space: pre-wrap;
  box-shadow: 0 1px 1px var(--c-tint);
  max-width: 100%;
}
.bubble.file {
  white-space: normal;
}
.msg-row.self .bubble {
  background: var(--c-bubble-self);
}
.bubble::before {
  content: '';
  position: absolute;
  top: 11px;
  border: 6px solid transparent;
}
.msg-row.peer .bubble::before {
  left: -11px;
  border-right-color: var(--c-bubble-peer);
}
.msg-row.self .bubble::before {
  right: -11px;
  border-left-color: var(--c-bubble-self);
}
.m-time {
  font-size: 10.5px;
  color: var(--c-sub);
  margin-top: 3px;
}
.read-tag {
  margin-right: 6px;
  color: var(--c-weak);
}
.read-tag.done {
  color: var(--c-accent);
}

/* 图片内联预览 */
.img-wrap {
  margin-top: 6px;
  background: var(--c-card);
  border-radius: 6px;
  padding: 3px;
}
.chat-img {
  display: block;
  max-width: min(260px, 100%);
  max-height: 200px;
  border-radius: 4px;
  cursor: zoom-in;
}

/* 文件卡片 */
.file-card {
  display: flex;
  gap: 8px;
  align-items: center;
  background: var(--c-mask-soft);
  border: 1px solid var(--c-hairline);
  border-radius: 6px;
  padding: 7px 10px;
  min-width: 210px;
  margin-top: 6px;
}
.b-text + .file-card {
  margin-top: 8px;
}
.fc-icon {
  color: var(--c-sub);
  flex: none;
}
.fc-main {
  flex: 1;
  min-width: 0;
}
.fc-name {
  font-size: 13px;
  max-width: 220px;
}
.fc-sub {
  font-size: 11.5px;
  color: var(--c-sub);
  display: flex;
  gap: 8px;
  align-items: center;
  margin-top: 2px;
}
.fc-sub a {
  color: var(--c-link);
  cursor: pointer;
}
.fc-sub a:hover {
  text-decoration: underline;
}
.ok {
  color: var(--c-accent);
}
.err {
  color: var(--c-danger);
}
.bar {
  width: 90px;
  height: 4px;
  border-radius: 2px;
  background: var(--c-hairline);
  overflow: hidden;
  display: inline-block;
}
.bar > i {
  display: block;
  height: 100%;
  background: var(--c-accent);
  transition: width 0.2s;
}

.placeholder {
  color: var(--c-weak);
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--c-weak);
  font-size: 14px;
}
.placeholder .sub {
  font-size: 11.5px;
  color: var(--c-border);
}

/* 输入区 */
.composer {
  flex: none;
  border-top: 1px solid var(--c-hairline);
  background: var(--c-card-alt);
  position: relative;
}
.toolbar {
  display: flex;
  gap: 4px;
  padding: 7px 12px 0;
}
.toolbar button {
  width: 30px;
  height: 30px;
  border-radius: 6px;
  color: var(--c-sub);
  display: flex;
  align-items: center;
  justify-content: center;
}
.toolbar button:hover:not(.disabled) {
  background: var(--c-hover);
  color: var(--c-text);
}
.toolbar .disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.input-area {
  display: block;
  width: calc(100% - 24px);
  height: 88px;
  margin: 4px 12px 0;
  resize: none;
  font-size: 14px;
  line-height: 1.6;
  user-select: text;
}
.composer-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 4px 12px 10px;
}
.hint {
  font-size: 11px;
  color: var(--c-weak);
}
.send-btn {
  border: 1px solid var(--c-border);
  background: var(--c-list);
  border-radius: 4px;
  padding: 5px 22px;
  font-size: 13px;
  color: var(--c-text);
}
.send-btn:hover:not(:disabled) {
  background: var(--c-list-hover);
}
.send-btn:disabled {
  opacity: 0.55;
  cursor: default;
}
.s-key {
  font-size: 11px;
  color: var(--c-sub);
}
</style>
