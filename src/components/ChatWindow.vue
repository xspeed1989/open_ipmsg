<script setup>
// 右侧聊天窗口：头部 / 消息流（日期分隔+气泡）/ 工具栏 / 输入区
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import * as ipc from '../lib/ipc'
import {
  store, sendText, sendFiles, sendFilesTo, downloadFile, clearHistory,
  openChat, displayName, dayLabel, fmtTime, fmtSize, refreshUsers, splitDelayedNote,
  sendTextTo, recallMsg, unlockMsg, broadcastTo, sendMulticastTo,
} from '../store'
import { parseFileUris, highlightParts } from '../lib/text'
import { computePopupPosition } from '../lib/popup'
import { composeReplyBody, quotePreview } from '../lib/reply'
import { forwardPayload, mergeForward } from '../lib/forward'
import { copyTextOf } from '../lib/copymsg'
import { pendingImgFromB64 } from '../lib/clipimg'
import { t } from '../lib/i18n'
import { open as openFileDialog, confirm as confirmDialog } from '@tauri-apps/plugin-dialog'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import { getCurrentWindow } from '@tauri-apps/api/window'
import Avatar from './Avatar.vue'
import EmojiPicker from './EmojiPicker.vue'
import RecipientPicker from './RecipientPicker.vue'

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

/* ---------- 右键回复 ---------- */
/** 正在回复的目标：{ preview, nick }；发送/取消/切换会话后清空 */
const replyTarget = ref(null)
/** 右键菜单：{ x, y, msg }；msg 用于启动回复 */
const ctxMenu = ref(null)
const ctxMenuRef = ref(null)

function openCtx(msg, e) {
  ctxMenu.value = {
    x: Math.min(e.clientX, window.innerWidth - 120),
    y: Math.min(e.clientY, window.innerHeight - 48),
    msg,
  }
}
function closeCtx() {
  ctxMenu.value = null
}
function onCtxMouseDown(e) {
  const path = e.composedPath ? e.composedPath() : []
  if (path.includes(ctxMenuRef.value)) return
  closeCtx()
}
function onCtxKeyDown(e) {
  if (e.key === 'Escape') closeCtx()
}
watch(ctxMenu, (open) => {
  if (open) {
    document.addEventListener('mousedown', onCtxMouseDown, true)
    document.addEventListener('keydown', onCtxKeyDown)
  } else {
    document.removeEventListener('mousedown', onCtxMouseDown, true)
    document.removeEventListener('keydown', onCtxKeyDown)
  }
})

function startReply() {
  const m = ctxMenu.value?.msg
  if (!m) return
  replyTarget.value = {
    preview: quotePreview(m),
    nick: m.dir === 'out' ? t('me') : displayName(store.activeKey),
  }
  closeCtx()
  nextTick(() => ta.value?.focus())
}
function cancelReply() {
  replyTarget.value = null
}

/* ---------- 转发 / 批量发送 ---------- */
const picker = ref(null) // { mode: 'forward'|'batch', payload }

function startForward() {
  const m = ctxMenu.value?.msg
  if (!m) return
  const payload = forwardPayload(m)
  closeCtx()
  if (!payload.ok) {
    alert(payload.reason)
    return
  }
  picker.value = { mode: 'forward', payload }
}


/* ---------- 封书 / 密码锁 ---------- */
const secretOn = ref(false) // 封书（SECRETEXOPT）
const pwdOn = ref(false) // 密码锁（PASSWORDOPT，仅密码功能开启时显示）

/** 开封：封书直接开；密码锁需输入本机密码 */
async function doUnlock(m) {
  if (!store.activeKey) return
  if (m.locked) {
    const pw = window.prompt ? window.prompt(t('chat.pwdPrompt')) : ''
    if (pw === null) return
    try {
      await unlockMsg(store.activeKey, m.pkt, pw || null)
      alert(t('chat.unlockedOk'))
    } catch (e) {
      alert(t('chat.unlockFail', { e }))
    }
    return
  }
  try {
    await unlockMsg(store.activeKey, m.pkt, null)
  } catch (e) {
    alert(e)
  }
}

/* ---------- 广播 / 群发 ---------- */
function startBroadcast() {
  const text = draft.value.trim()
  if (!text) {
    alert(t('chat.alertFillContent'))
    return
  }
  broadcastTo(text)
    .then(() => alert(t('chat.broadcastSent')))
    .catch((e) => alert(t('chat.alertSendFailed', { e })))
}
function startMulticast() {
  if (!canSend.value) {
    alert(t('chat.alertFillContent'))
    return
  }
  picker.value = { mode: 'multicast', payload: null }
}

/* ---------- 撤回 ---------- */
function startRecall() {
  const m = ctxMenu.value?.msg
  if (!m || !store.activeKey) return
  closeCtx()
  recallMsg(store.activeKey, m.pkt).catch((e) => alert(e))
}

function startBatch() {
  if (!store.activeKey) {
    alert(t('chat.alertSelectSession'))
    return
  }
  // 输入框为空时不再静默禁用按钮，点击给出明确引导
  if (!canSend.value) {
    alert(t('chat.alertFillContent'))
    nextTick(() => ta.value?.focus())
    return
  }
  picker.value = { mode: 'batch', payload: null }
}

/* ---------- 复制 / 多选转发 ---------- */
/** 多选模式：点击气泡切换选中，底部操作条合并转发 */
const selMode = ref(false)
const selected = ref(new Set()) // 消息对象引用集合（会话内稳定）

function copyMsg() {
  // 用户已经选中了部分文本 → 复制选中部分；没有选区才复制整条消息
  // （气泡正文可自由选中，Windows/WebView2 上同样生效）
  const sel = window.getSelection()
  const selText = sel && !sel.isCollapsed ? sel.toString() : ''
  if (selText.trim()) {
    closeCtx()
    ipc.copyText(selText).catch((e) => alert(t('chat.alertCopyFailed', { e })))
    return
  }
  const m = ctxMenu.value?.msg
  closeCtx()
  if (!m) return
  const ct = copyTextOf(m)
  if (!ct) {
    alert(t('chat.alertNoCopy'))
    return
  }
  ipc.copyText(ct).catch((e) => alert(t('chat.alertCopyFailed', { e })))
}

function enterSelMode() {
  // 右键的那条默认选中，省一次点击
  const m = ctxMenu.value?.msg
  closeCtx()
  selMode.value = true
  selected.value = new Set(m ? [m] : [])
}

function exitSel() {
  selMode.value = false
  selected.value = new Set()
}

function toggleSel(m) {
  if (!selMode.value) return
  const s = new Set(selected.value)
  if (s.has(m)) s.delete(m)
  else s.add(m)
  selected.value = s
}

function startMultiForward() {
  const peerNick = displayName(store.activeKey) || t('chat.peer')
  const merged = mergeForward([...selected.value], (dir) => (dir === 'out' ? t('me') : peerNick))
  if (!merged) {
    alert(t('chat.alertNoForward'))
    return
  }
  exitSel()
  picker.value = { mode: 'forward', payload: { kind: 'text', text: merged } }
}

async function onPickerConfirm(keys) {
  const p = picker.value
  picker.value = null
  if (!p || !keys.length) return
  // 批量发送：待发送附件只落盘一次，多个收件人复用同一批路径
  let paths = null
  if (p.mode === 'batch' && pendingList.value.length) {
    try {
      paths = await pendingToPaths(pendingList.value)
    } catch (e) {
      alert(t('chat.alertStageFailed', { e }))
      return
    }
  }
  const fails = []
  let ok = 0
  for (const k of keys) {
    try {
      if (p.mode === 'forward') {
        if (p.payload.kind === 'text') await sendTextTo(k, p.payload.text)
        else await sendFilesTo(k, p.payload.paths)
      } else if (p.mode === 'multicast') {
        await sendTextTo(k, draft.value.replace(/\n{3,}/g, '\n\n').trimEnd())
      } else {
        const text = draft.value.replace(/\n{3,}/g, '\n\n').trimEnd()
        if (paths) await sendFilesTo(k, paths, text)
        else await sendTextTo(k, text)
      }
      ok++
    } catch (e) {
      fails.push(k)
    }
  }
  if (p.mode === 'batch') {
    clearPending()
    draft.value = ''
  }
  const name = (k) => displayName(k) || k
  const names = keys.map(name).join(t('sep.list'))
  if (!fails.length) alert(t('chat.alertSentTo', { n: ok, names }))
  else alert(t('chat.alertPartial', { ok, fail: fails.length, names }))
}

watch(() => store.activeKey, () => nextTick(() => ta.value?.focus()))
// 切会话退出多选模式：选中集是按消息对象引用记的，跨会话无意义
watch(() => store.activeKey, () => exitSel())

const canSend = computed(() => !!draft.value.trim() || pendingList.value.length > 0)

async function doSend() {
  if (!store.activeKey || !canSend.value) return
  let text = draft.value.replace(/\n{3,}/g, '\n\n').trimEnd()
  // 正在回复某条消息时，把引用原文作为文本并入正文（对方看到即回的内容）
  if (replyTarget.value) {
    text = composeReplyBody(replyTarget.value.preview, text)
    replyTarget.value = null
  }
  try {
    const opts = { secret: secretOn.value, password: pwdOn.value }
    if (pendingList.value.length) {
      // 待发送附件与随行文字一并发出（IPMsg 的一条消息可同时带正文和附件）
      const paths = await pendingToPaths(pendingList.value)
      await sendFilesTo(store.activeKey, paths, text, opts.secret, opts.password)
      clearPending()
    } else {
      await sendTextTo(store.activeKey, text, opts.secret, opts.password)
    }
    secretOn.value = false
    pwdOn.value = false
    draft.value = ''
    autoBottom = true
  } catch (e) {
    alert(t('chat.alertSendFailed', { e }))
  }
}

/* ---------- 待发送附件（剪贴板图片 / 粘贴的文件 / 拖入的文件） ----------
   与微信一致：粘贴/拖入只进输入区上方的待发送列表，按 Enter 或点「发送」才真正发出；
   待发送列表按会话独立保存（key -> 数组），切会话不串台，各自 Enter 各自发。
   每条未发送的附件都可以单独移除。条目两类：
     { kind:'img',  b64, mime, size, url, name }   —— 剪贴板截图，发送时落盘
     { kind:'file', path, name }                   —— 已有本地路径的文件
*/
const pendingMap = ref({}) // key -> 该会话的待发送条目数组

/** 取某会话的待发送条目数组（不存在则建空数组）；无会话返回空数组 */
function pendingOf(key) {
  if (!key) return []
  if (!pendingMap.value[key]) pendingMap.value[key] = []
  return pendingMap.value[key]
}

/** 当前会话的待发送列表（只读视图：模板用；改动一律走 pendingOf） */
const pendingList = computed(() => pendingMap.value[store.activeKey] || [])

function baseName(p) {
  return (p || '').split(/[\\/]/).pop() || p || ''
}
const MIME_EXT = {
  'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif',
  'image/bmp': 'bmp', 'image/webp': 'webp',
}
function imgItemName(mime) {
  return t('chat.clipboardImage') + '.' + (MIME_EXT[mime] || 'png')
}

/** 把粘贴得到的本地路径统一变成待发送文件条目 */
function pendingFileItems(paths) {
  return (paths || []).filter(Boolean).map((path) => ({ kind: 'file', path, name: baseName(path) }))
}

/** 把待发送条目全部落成可发送的本地路径（剪贴板图片先落盘），失败抛错 */
async function pendingToPaths(items) {
  const paths = []
  for (const it of items || []) {
    if (it.kind === 'file' && it.path) paths.push(it.path)
    else paths.push(await ipc.stagePastedFile(it.name || imgItemName(it.mime), it.b64))
  }
  return paths
}

/**
 * 从待发送列表移除一条（未发送的文件可随时取消）。
 * pendingList 是当前会话的只读视图，这里直接改底层数组。
 */
function removePending(i) {
  const it = pendingOf(store.activeKey)[i]
  if (it?.url) URL.revokeObjectURL(it.url)
  pendingOf(store.activeKey).splice(i, 1)
}

/** 清空当前会话的待发送列表并释放预览 URL */
function clearPending() {
  const list = pendingOf(store.activeKey)
  for (const it of list) {
    if (it.url) URL.revokeObjectURL(it.url)
  }
  list.splice(0)
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
    alert(t('chat.alertImgTooBig'))
    return false
  }
  const buf = new Uint8Array(await file.arrayBuffer())
  const mime = file.type || 'image/png'
  pendingOf(store.activeKey).push({
    kind: 'img',
    b64: bytesToB64(buf),
    mime,
    size: buf.length,
    url: URL.createObjectURL(file),
    name: imgItemName(mime),
  })
  nextTick(() => ta.value?.focus())
  return true
}

/**
 * 粘贴处理（窗口级，Ctrl+V 在哪都生效）：
 *  1. clipboardData 里就有文件路径 → 进「待发送列表」，按 Enter 才发（不再直接发）
 *  2. clipboardData 什么都没有（Linux/WebKitGTK 不暴露文件类剪贴板）→ 读原生 GTK 剪贴板
 *  3. 只有文件内容没有路径（Windows 资源管理器 / 邮件客户端）→ 落盘后进待发送列表
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

  // 1) 有路径（文件管理器复制的 file:// 列表）：零拷贝，先收进待发送列表
  const paths = parseFileUris(dt.getData('text/uri-list') || dt.getData('text/plain') || '')
  if (paths.length) {
    e.preventDefault()
    pendingOf(store.activeKey).push(...pendingFileItems(paths))
    nextTick(() => ta.value?.focus())
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

  // 3) 有内容没路径（Windows 资源管理器 / 邮件客户端）：落盘后进待发送列表
  try {
    const pending = pendingOf(store.activeKey)
    for (const f of files) {
      const buf = new Uint8Array(await f.arrayBuffer())
      const path = await ipc.stagePastedFile(f.name || t('chat.pasteFile'), bytesToB64(buf))
      pending.push({ kind: 'file', path, name: baseName(f.name || t('chat.pasteFile')) })
    }
    nextTick(() => ta.value?.focus())
  } catch (err) {
    alert(t('chat.alertPasteFailed', { e: err }))
  }
}

/**
 * Ctrl/⌘+V 兜底：WebKitGTK 既不会把文件/图片剪贴板内容交给网页，
 * 往往连 paste 事件都不触发 —— 所以不能只等 paste 事件。这里在按键后
 * 确认网页层确实没拿到内容，再去读原生 GTK 剪贴板（文件路径 → 图片）。
 */
let pasteSeen = false
function onPasteHotkey(e) {
  if (!(e.ctrlKey || e.metaKey) || e.key?.toLowerCase() !== 'v' || e.altKey) return
  pasteSeen = false
  setTimeout(async () => {
    if (pasteSeen) return // 网页层已经处理（普通文本或文件）
    try {
      const native = await ipc.clipboardFilePaths()
      if (native?.length) {
        // 复制的文件路径：进待发送列表，不直接发
        pendingOf(store.activeKey).push(...pendingFileItems(native))
        nextTick(() => ta.value?.focus())
        return
      }
      // 剪贴板里没有文件路径 → 可能是截图：读原生剪贴板位图，先预览再发
      const img = await ipc.clipboardImage()
      if (img?.b64) {
        const p = pendingImgFromB64(img.b64, img.mime, img.size)
        pendingOf(store.activeKey).push({
          kind: 'img',
          b64: p.b64,
          mime: p.mime,
          size: p.size,
          url: URL.createObjectURL(p.blob),
          name: imgItemName(p.mime),
        })
        nextTick(() => ta.value?.focus())
      }
    } catch (err) {
      console.error('clipboard fallback failed', err)
    }
  }, 80)
}

/* ---------- 拖放文件（进待发送列表，不直接发送） ----------
   Tauri 下系统文件的拖放由原生层接管（webview 的 drop 事件拿不到文件路径），
   必须用窗口的 onDragDropEvent，它给的是真实绝对路径。
   拖到中栏某个联系人上 → 高亮该联系人，松开后打开对应会话并加入该会话的待发送列表；
   拖到其余位置 → 加入当前会话的待发送列表（按 Enter 才真正发出）。*/
const dragOver = ref(false)
let unlistenDrop = null

/* 拖放坐标的来源与单位因平台而异：
   - Windows(WebView2)：ScreenToClient 结果是物理像素 → 需 / devicePixelRatio 得 CSS 像素
   - Linux(WebKitGTK)：drag_motion 的 x,y 就是 widget 逻辑像素，等于 CSS 像素
   - macOS(WKWebView)：draggingLocation 的 points，也是 CSS 像素
   所以只对 Windows 做 DPR 缩放，否则高分屏/系统缩放下高亮会偏离鼠标位置。*/
const DRAG_SCALE = /windows/i.test(navigator.userAgent) ? window.devicePixelRatio || 1 : 1

/** 把拖放的 position 换算成 elementFromPoint 可用的 CSS 像素坐标 */
function cssPoint(pos) {
  if (!pos) return { x: 0, y: 0 }
  return { x: pos.x / DRAG_SCALE, y: pos.y / DRAG_SCALE }
}

/** 把拖放的物理坐标换算成页面元素，判断落点是不是中栏某个联系人；
 *  返回其 key，不在联系人上时返回空串（区别于「当前会话」兜底） */
function keyAtPoint(pos) {
  if (!pos) return ''
  const p = cssPoint(pos)
  const el = document.elementFromPoint(p.x, p.y)
  const row = el && el.closest ? el.closest('[data-user-key]') : null
  return row?.dataset.userKey || ''
}

/** 把拖入的路径收进指定会话的待发送列表，等待用户按 Enter 发送 */
function attachToPending(key, paths) {
  if (!key || !paths?.length) return
  pendingOf(key).push(...pendingFileItems(paths))
}

onMounted(async () => {
  try {
    unlistenDrop = await getCurrentWindow().onDragDropEvent(async ({ payload }) => {
      if (payload.type === 'over') {
        dragOver.value = true
        // 拖动过程中实时更新中栏高亮：悬停到哪个联系人，哪一行亮
        store.dragHoverKey = keyAtPoint(payload.position)
        return
      }
      if (payload.type !== 'drop') {
        dragOver.value = false
        store.dragHoverKey = ''
        return
      }
      const hoverKey = keyAtPoint(payload.position)
      dragOver.value = false
      store.dragHoverKey = ''
      const paths = (payload.paths || []).filter(Boolean)
      if (!paths.length) return
      // 落点在联系人上 → 打开对应会话再附加；其余位置 → 当前会话的待发送列表
      const key = hoverKey || store.activeKey
      if (!key) {
        alert(t('chat.alertPickContact'))
        return
      }
      if (hoverKey && hoverKey !== store.activeKey) await openChat(hoverKey)
      attachToPending(key, paths)
      nextTick(() => ta.value?.focus())
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
  window.addEventListener('keydown', onReplyEsc)
})
onUnmounted(() => {
  if (unlistenDrop) unlistenDrop()
  window.removeEventListener('paste', onPaste)
  window.removeEventListener('keydown', onPasteHotkey)
  window.removeEventListener('keydown', onFindHotkey)
  window.removeEventListener('keydown', onReplyEsc)
})
/** Esc 依次关闭：接收人弹窗 → 回复条；输入框/搜索框内按 Esc 不干扰 */
function onReplyEsc(e) {
  if (e.key !== 'Escape') return
  const t = e.target
  if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return
  if (picker.value) {
    picker.value = null
    return
  }
  if (replyTarget.value) cancelReply()
}
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
    const sel = await openFileDialog({ multiple: true, title: t('chat.pickFilesTitle') })
    if (!sel) return
    await sendFiles(Array.isArray(sel) ? sel : [sel])
    autoBottom = true
  } catch (e) {
    alert(t('chat.alertSendFailed', { e }))
  }
}

async function pickFolder() {
  if (!store.activeKey) return
  try {
    const sel = await openFileDialog({ multiple: true, directory: true, title: t('chat.pickFolderTitle') })
    if (!sel) return
    await sendFiles(Array.isArray(sel) ? sel : [sel])
    autoBottom = true
  } catch (e) {
    alert(t('chat.alertSendFailed', { e }))
  }
}

/* ---------- 表情 ---------- */
const emojiOpen = ref(false)
// 面板尺寸估估值：宽度取实际样式，高度按 5 行网格估算（仅用于上下翻转判断）
const EMOJI_PANEL_W = 264
const EMOJI_PANEL_H = 176
const emojiBtnRef = ref(null)
const emojiPanelRef = ref(null)
const emojiStyle = ref({})

function toggleEmoji() {
  if (emojiOpen.value) {
    emojiOpen.value = false
    return
  }
  const btn = emojiBtnRef.value
  const rect = (btn?.$el || btn)?.getBoundingClientRect?.()
  if (!rect) {
    emojiOpen.value = true
    return
  }
  // 跟随按钮位置弹出（上方优先，越界翻转/夹回），而不是固定挂在右下角
  const { left, top } = computePopupPosition(
    { left: rect.left, top: rect.top, bottom: rect.bottom, width: rect.width },
    EMOJI_PANEL_W,
    EMOJI_PANEL_H,
    window.innerWidth,
    window.innerHeight,
  )
  emojiStyle.value = { position: 'fixed', left: left + 'px', top: top + 'px' }
  emojiOpen.value = true
}

/** 点击面板和触发按钮之外的地方 / 按 Esc → 关闭面板 */
function onDocMouseDown(e) {
  const path = e.composedPath ? e.composedPath() : []
  const panelEl = emojiPanelRef.value?.$el || emojiPanelRef.value
  if (path.includes(panelEl) || path.includes(emojiBtnRef.value)) return
  emojiOpen.value = false
}
function onDocKeyDown(e) {
  if (e.key === 'Escape') emojiOpen.value = false
}
watch(emojiOpen, (open) => {
  if (open) {
    document.addEventListener('mousedown', onDocMouseDown, true)
    document.addEventListener('keydown', onDocKeyDown)
  } else {
    document.removeEventListener('mousedown', onDocMouseDown, true)
    document.removeEventListener('keydown', onDocKeyDown)
  }
})
// 切换会话时收起面板、回复状态与接收人弹窗，避免挂在新会话上
watch(() => store.activeKey, () => {
  emojiOpen.value = false
  replyTarget.value = null
  picker.value = null
  closeCtx()
})
onUnmounted(() => {
  document.removeEventListener('mousedown', onDocMouseDown, true)
  document.removeEventListener('keydown', onDocKeyDown)
  document.removeEventListener('mousedown', onCtxMouseDown, true)
  document.removeEventListener('keydown', onCtxKeyDown)
})

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
/** 对端公告的体积可能是 0（官方客户端对文件夹就这么发），
 *  这时百分比没有意义，改显示已接收的字节数 */
function progressLabel(f) {
  return f.total ? `${pct(f)}%` : fmtSize(f.transferred || 0)
}
/** 单击聊天里的图片 → 在独立窗口打开（仿微信） */
async function viewImage(f) {
  if (!f.path) return
  try {
    await ipc.openImageViewer(f.path, f.name)
  } catch (e) {
    alert(t('chat.alertOpenImg', { e }))
  }
}
async function openFile(path) {
  try { await openPath(path) } catch (e) { alert(t('chat.alertOpen', { e })) }
}
async function revealFile(path) {
  try { await revealItemInDir(path) } catch (e) { alert(t('chat.alertReveal', { e })) }
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
    t('chat.clearConfirm', { who }),
    { title: t('chat.clearTitle'), kind: 'warning', okLabel: t('chat.clearOk'), cancelLabel: t('cancel') }
  )
  if (!ok) return
  try {
    await clearHistory(key)
  } catch (e) {
    alert(t('chat.alertClear', { e }))
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
          <i class="stat" :class="isOnline ? 'on' : 'off'">{{ isOnline ? '● ' + t('online') : '● ' + t('offline') }}</i>
          {{ activeUser.host || '' }}<template v-if="activeUser.ip"> · {{ activeUser.ip }}</template>
          <template v-if="activeUser.group"> · {{ activeUser.group }}</template>
        </div>
      </div>
      <div class="head-actions">
        <button class="mini-btn" :title="t('chat.clearHistory')" @click="doClearHistory">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none">
            <path d="M4 7h16M9 7V5h6v2M6 7l1 13h10l1-13" stroke="currentColor" stroke-width="1.8"
              stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
        <button class="mini-btn" :title="t('chat.refreshUsers')" @click="refreshUsers">
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
        <p v-if="store.dragHoverKey">{{ t('chat.dropOpen', { name: displayName(store.dragHoverKey) }) }}</p>
        <p v-else-if="activeUser">{{ t('chat.dropAdd') }}</p>
        <p v-else>{{ t('chat.dropChoose') }}</p>
        <span class="sub">{{ t('chat.dropSupport') }}</span>
      </div>
    </div>

    <!-- 会话内查找 -->
    <div v-if="activeUser && findOpen" class="findbar">
      <input
        ref="findInput"
        v-model="findQuery"
        :placeholder="t('chat.findPh')"
        spellcheck="false"
        @keydown.enter.prevent="stepFind(1)"
        @keydown.esc.prevent="closeFind"
      />
      <span class="cnt">{{ findHits.length ? findIdx + 1 : 0 }}/{{ findHits.length }}</span>
      <button :title="t('chat.findPrev')" :disabled="!findHits.length" @click="stepFind(-1)">∧</button>
      <button :title="t('chat.findNext')" :disabled="!findHits.length" @click="stepFind(1)">∨</button>
      <button :title="t('chat.findClose')" @click="closeFind">✕</button>
    </div>

    <!-- 消息区 -->
    <div v-if="activeUser" ref="scroller" class="msgs" @scroll="onScroll">
      <div v-for="v in viewList" :key="v.id">
        <div v-if="v.kind === 'day'" class="day-sep"><span>{{ v.label }}</span></div>

        <div
          v-else
          class="msg-row"
          :data-pkt="v.m.pkt"
          :class="[v.m.dir === 'out' ? 'self' : 'peer', { merge: !v.firstOfCluster, selectable: selMode, picked: selected.has(v.m) }]"
          @click.stop="toggleSel(v.m)"
        >
          <Avatar class="m-ava" :name="v.m.dir === 'out' ? store.config?.nickname : displayName(store.activeKey)"
            :seed="v.m.dir === 'out' ? 'self' : store.activeKey" :size="34" />
          <div class="bubble-wrap">
            <i v-if="selMode" class="sel-check" :class="{ on: selected.has(v.m) }" @click.stop="toggleSel(v.m)"></i>
            <div class="bubble" :class="{ file: v.m.kind === 'file' }"
              @contextmenu.prevent="selMode ? null : openCtx(v.m, $event)">
              <div v-if="v.m.recalled" class="b-text recalled">{{ t('chat.recalledTip') }}</div>
              <div v-else-if="(v.m.locked || (v.m.secret && !v.m.unlocked))" class="b-text locked">
                <span class="lock-ico">🔒</span>
                <template v-if="v.m.locked && !v.m.unlocked">{{ t('chat.pwdLocked') }}</template>
                <template v-else>{{ t('chat.secretSealed') }}</template>
                <button class="unlock-btn" @click.stop="doUnlock(v.m)">{{ t('chat.unlock') }}</button>
              </div>
              <div v-else-if="bodyOf(v.m)" class="b-text">
                <template v-if="hlQuery">
                  <span v-for="(p, i) in textParts(v.m)" :key="i" :class="{ hl: p.hit }">{{ p.text }}</span>
                </template>
                <template v-else>{{ bodyOf(v.m) }}</template>
              </div>
              <template v-for="f in v.m.files || []" :key="f.id">
                <!-- 图片：本地已有内容时直接内联预览，点击查看原图（多选模式下点击改为切换选中） -->
                <div v-if="isImg(f.name) && f.src" class="img-wrap">
                  <img :src="f.src" class="chat-img" :title="t('chat.viewImg')" @click="selMode ? toggleSel(v.m) : viewImage(f)" />
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
                    <!-- 历史导入的附件：官方日志库里只有文件名没有内容，不给下载/定位 -->
                    <template v-if="f.state === 'imported'">
                      <span class="muted">{{ t('chat.histImport') }}</span>
                    </template>
                    <template v-else>
                      <!-- 文件夹不显示体积：对端（官方客户端/飞秋）公告文件夹大小恒为 0，
                           显示 "0 B" 毫无意义；下载完成后有进度与「文件夹已保存」状态即可 -->
                      <span v-if="!f.dir_entry">{{ fmtSize(f.size) }}</span>
                      <!-- 收到的文件 -->
                      <template v-if="v.m.dir === 'in'">
                        <template v-if="f.state === 'pending'">
                          <a @click.prevent="downloadFile(v.m, f)">{{ t('chat.download') }}</a>
                        </template>
                        <template v-else-if="f.state === 'downloading'">
                          <span v-if="f.enc">{{ t('chat.downloadEnc') }} {{ progressLabel(f) }}</span>
                          <span v-else>{{ progressLabel(f) }}</span>
                          <i v-if="f.total" class="bar"><i :style="{ width: pct(f) + '%' }"></i></i>
                        </template>
                        <template v-else-if="f.state === 'done'">
                          <span class="ok">{{ f.enc ? t('chat.decrypted') : (f.dir_entry ? t('chat.folderSaved') : t('chat.saved')) }}</span>
                          <a @click.prevent="openFile(f.path)">{{ t('chat.open') }}</a>
                          <a @click.prevent="revealFile(f.path)">{{ t('chat.revealDir') }}</a>
                        </template>
                        <template v-else-if="f.state === 'failed'">
                          <span class="err">{{ t('chat.failed') }}</span>
                          <a @click.prevent="downloadFile(v.m, f)">{{ t('chat.retry') }}</a>
                        </template>
                      </template>
                      <!-- 发出的文件 -->
                      <template v-else>
                        <span v-if="v.m.queued" class="muted">{{ t('chat.queuedNote') }}</span>
                        <span v-else class="ok">{{ t('chat.sent') }}</span>
                        <a @click.prevent="revealFile(f.path)">{{ t('chat.revealDir') }}</a>
                      </template>
                    </template>
                  </div>
                </div>
                </div>
              </template>
            </div>
            <!-- 对端「延迟发送/离线留言」的尾注：收成一个小标记，不占正文 -->
            <div v-if="delayedOf(v.m) !== null" class="delay-tag" :title="t('chat.delayedTitle', { t: delayedOf(v.m) || t('unknown') })">
              {{ t('chat.delayedNote', { t: delayedOf(v.m) || t('unknown') }) }}
            </div>
            <div v-if="v.m.dir === 'out' && v.m.queued" class="delay-tag out-queued">
              {{ t('chat.queuedNote') }}
            </div>
            <div class="m-time" :class="{ self: v.m.dir === 'out' }">
              <span v-if="v.m.dir === 'out' && v.m.rcpt && !v.m.queued" class="read-tag" :class="{ done: v.m.read }">
                {{ v.m.read ? t('chat.read') : t('chat.unread') }}
              </span>
              {{ fmtTime(v.m.ts) }}<svg v-if="v.m.enc" class="m-lock" width="10" height="10" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <rect x="4.5" y="10" width="15" height="10" rx="2" stroke="currentColor" stroke-width="2" />
                <path d="M8 10V7.5a4 4 0 0 1 8 0V10" stroke="currentColor" stroke-width="2" />
              </svg><span v-if="v.m.enc && v.m.sig_ok === false" class="sig-warn" :title="t('chat.sigWarn')">⚠</span>
            </div>
          </div>
        </div>
      </div>
      <div style="height: 10px"></div>
    </div>

    <!-- 消息右键菜单 -->
    <div v-if="ctxMenu" ref="ctxMenuRef" class="ctx-menu"
      :style="{ position: 'fixed', left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }">
      <button class="ctx-item" @click="copyMsg">{{ t('chat.copy') }}</button>
      <button class="ctx-item" @click="startReply">{{ t('chat.reply') }}</button>
      <button class="ctx-item" @click="startForward">{{ t('chat.forward') }}</button>
      <button v-if="ctxMenu.msg?.dir === 'out' && ctxMenu.msg?.kind === 'text' && !ctxMenu.msg?.recalled"
        class="ctx-item danger" @click="startRecall">{{ t('chat.recall') }}</button>
      <button class="ctx-item" @click="enterSelMode">{{ t('chat.multiSelect') }}</button>
    </div>

    <!-- 转发（排除当前会话）/ 批量发送（默认含当前会话）的接收人选择 -->
    <RecipientPicker
      v-if="picker"
      :title="picker.mode === 'forward' ? t('chat.forwardTo') : picker.mode === 'multicast' ? t('chat.multicastTo') : t('chat.batchTo')"
      :exclude-key="picker.mode === 'forward' ? store.activeKey : ''"
      :preselect-key="picker.mode === 'batch' && store.userMap[store.activeKey] ? store.activeKey : ''"
      @confirm="onPickerConfirm"
      @cancel="picker = null"
    />

    <!-- 空态：显式条件，不依赖与上方元素的 v-if/v-else 配对
         （中间隔着右键菜单/接收人弹窗时，v-else 会错误地配给它们） -->
    <div v-if="!activeUser" class="placeholder">
      <svg width="72" height="72" viewBox="0 0 24 24" fill="none">
        <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.6c-.5.42-1.3.07-1.3-.6V5.5z"
          stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" />
      </svg>
      <p>{{ t('chat.placeholderTitle') }}</p>
      <p class="sub">{{ t('chat.placeholderSub') }}</p>
    </div>

    <!-- 多选模式操作条：合并转发 / 取消 -->
    <div v-if="selMode && activeUser" class="sel-bar">
      <span class="sel-count">{{ t('chat.selected', { n: selected.size }) }}</span>
      <span class="flex1"></span>
      <button class="btn-plain" @click="exitSel">{{ t('cancel') }}</button>
      <button class="btn-primary" :disabled="!selected.size" @click="startMultiForward">{{ t('chat.mergeForward') }}</button>
    </div>

    <!-- 输入区 -->
    <footer v-if="activeUser" class="composer">
      <div class="toolbar">
        <button ref="emojiBtnRef" :title="t('chat.emoji')" @click="toggleEmoji">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" />
            <circle cx="9" cy="10" r="1.2" fill="currentColor" />
            <circle cx="15" cy="10" r="1.2" fill="currentColor" />
            <path d="M8.2 14c.9 1.4 2.2 2.1 3.8 2.1s2.9-.7 3.8-2.1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button :title="t('chat.batchSend')" @click="startBatch">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" stroke-dasharray="3 3" />
            <path d="M7 12h10M12 7v10" stroke="currentColor" stroke-width="1.6" />
          </svg>
        </button>
        <button :title="t('chat.sendFiles')" @click="pickFiles">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M11 5.5A3.5 3.5 0 0 1 17.9 7l.6 6.5a5.5 5.5 0 0 1-11 .5L7 8" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" transform="rotate(45 12 12)" />
            <path d="M13.5 8.5l-5 5a2.5 2.5 0 0 0 3.5 3.5l5.5-5.5a4 4 0 1 0-5.7-5.7L6 11" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button :title="t('chat.sendFolder')" @click="pickFolder">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"
              stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
          </svg>
        </button>
        <button v-if="store.config?.password_use" :class="{ on: pwdOn }" :title="t('chat.pwdLock')" @click="pwdOn = !pwdOn">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <rect x="5" y="11" width="14" height="9" rx="2" stroke="currentColor" stroke-width="1.6" />
            <path d="M8 11V8a4 4 0 0 1 8 0v3" stroke="currentColor" stroke-width="1.6" />
            <circle cx="12" cy="15.5" r="1.2" fill="currentColor" />
          </svg>
        </button>
        <button :class="{ on: secretOn }" :title="t('chat.secretSend')" @click="secretOn = !secretOn">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <rect x="4.5" y="10.5" width="15" height="9" rx="2" stroke="currentColor" stroke-width="1.6" />
            <path d="M9 10.5V7.5a3 3 0 0 1 6 0v3" stroke="currentColor" stroke-width="1.6" />
            <path d="M12 14v2.2" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button :title="t('chat.broadcast')" @click="startBroadcast">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="2" fill="currentColor" />
            <path d="M7.8 7.8a6 6 0 0 0 0 8.4M16.2 7.8a6 6 0 0 1 0 8.4M4.9 4.9a10 10 0 0 0 0 14.2M19.1 4.9a10 10 0 0 1 0 14.2" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button :title="t('chat.multicast')" @click="startMulticast">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" stroke-dasharray="3 3" />
            <path d="M9.2 9.2a4 4 0 0 0 0 5.6M14.8 9.2a4 4 0 0 1 0 5.6M6.6 6.6a7 7 0 0 0 0 10.8M17.4 6.6a7 7 0 0 1 0 10.8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <!-- 正在回复某条消息：输入区上方的引用条 -->
      <div v-if="replyTarget" class="reply-bar">
        <span class="rb-txt ellipsis">{{ t('chat.replyTo', { nick: replyTarget.nick }) }}{{ replyTarget.preview }}</span>
        <button class="rb-x" :title="t('chat.cancelReply')" @click="cancelReply">✕</button>
      </div>

      <EmojiPicker ref="emojiPanelRef" v-if="emojiOpen" :style="emojiStyle" @pick="insertEmoji" />

      <!-- 待发送附件列表：剪贴板图片 / 粘贴的文件；未发送前可逐项移除 -->
      <div v-if="pendingList.length" class="paste-strip">
        <div v-for="(it, i) in pendingList" :key="i" class="paste-item">
          <img v-if="it.kind === 'img'" :src="it.url" class="paste-thumb" />
          <div v-else class="paste-ficon">
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
              <path d="M6 3h8l4 4v14H6V3z" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
              <path d="M14 3v4h4" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
            </svg>
          </div>
          <div class="paste-meta">
            <div class="paste-name ellipsis" :title="it.name">{{ it.name }}</div>
            <div class="paste-sub">{{ it.kind === 'img' ? fmtSize(it.size) + ' · ' : '' }}{{ t('chat.pendingEnter') }}</div>
          </div>
          <button class="paste-x" :title="t('chat.removePending')" @click="removePending(i)">✕</button>
        </div>
      </div>

      <textarea
        ref="ta"
        v-model="draft"
        class="input-area"
        :placeholder="t('chat.inputPh')"
        spellcheck="false"
        @keydown="onKeydown"
      ></textarea>

      <div class="composer-foot">
        <span class="hint">{{ t('chat.enterHint') }}</span>
        <button class="send-btn" :disabled="!canSend" @click="doSend">
          {{ t('chat.send') }}<span class="s-key">(S)</span>
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
  flex-wrap: wrap;
  gap: 8px;
  margin: 0 12px 6px;
  padding: 8px;
  border: 1px solid var(--c-line, var(--c-hairline));
  border-radius: 6px;
  background: var(--c-tint);
  max-height: 148px;
  overflow-y: auto;
}
.paste-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 6px;
  border: 1px solid var(--c-hairline);
  border-radius: 6px;
  background: var(--c-card);
  min-width: 0;
}
.paste-thumb {
  width: 42px;
  height: 42px;
  object-fit: cover;
  border-radius: 4px;
  flex: none;
}
.paste-ficon {
  width: 42px;
  height: 42px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 4px;
  background: var(--c-tint);
  color: var(--c-sub);
}
.paste-meta {
  flex: 1;
  min-width: 0;
  font-size: 12px;
}
.paste-name {
  max-width: 220px;
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
  color: var(--c-danger);
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
  /* 全局 body 关了选择，气泡正文单独放开：Windows/WebView2 上也能
     像微信一样用鼠标自由选中文本再复制 */
  user-select: text;
  -webkit-user-select: text;
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
/* 加密锁标：仅 enc===true 的记录显示（旧记录/明文消息无该字段，渲染保持原样） */
.m-lock {
  margin-left: 3px;
  vertical-align: -1px;
  opacity: 0.8;
}
/* 签名校验失败警示：仅加密且 sig_ok===false 的记录显示 */
.sig-warn {
  margin-left: 3px;
  color: var(--c-danger);
  cursor: help;
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
.muted {
  color: var(--c-weak);
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

/* ---------- 多选转发模式 ---------- */
/* 选中态：气泡描边；行内容整体变为「点一下即切换选中」 */
.msg-row.selectable {
  cursor: pointer;
}
.msg-row.selectable .bubble * {
  /* 内容全部惰性化：链接/图片不再各自响应，点击统一落到行上切换选中 */
  pointer-events: none;
  user-select: none;
}
.msg-row.picked .bubble {
  outline: 2px solid var(--c-accent);
  outline-offset: -2px;
}
.sel-check {
  flex: none;
  align-self: center;
  width: 18px;
  height: 18px;
  margin: 0 4px;
  border-radius: 50%;
  border: 1.6px solid var(--c-border);
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.sel-check.on {
  border-color: var(--c-accent);
  background: var(--c-accent);
}
.sel-check.on::after {
  content: '';
  width: 5px;
  height: 9px;
  border: solid #fff;
  border-width: 0 1.8px 1.8px 0;
  transform: rotate(45deg) translate(-0.5px, -0.5px);
}
.sel-bar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 16px;
  border-top: 1px solid var(--c-hairline);
  background: var(--c-card-alt);
}
.sel-count {
  font-size: 12.5px;
  color: var(--c-sub);
}
.flex1 {
  flex: 1;
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
.toolbar button.on {
  color: var(--c-accent, #07c160);
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
  flex: 1;
  min-width: 0;
  padding-right: 8px;
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

/* ---------- 右键回复 ---------- */
.ctx-menu {
  z-index: 40;
  min-width: 96px;
  padding: 4px;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 8px;
  box-shadow: 0 6px 24px var(--c-shadow);
}
.ctx-item {
  display: block;
  width: 100%;
  padding: 6px 10px;
  border-radius: 4px;
  text-align: left;
  font-size: 13px;
  color: var(--c-text);
}
.ctx-item:hover {
  background: var(--c-list-hover);
}

.reply-bar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 6px;
  margin: 6px 12px 0;
  font-size: 12px;
  color: var(--c-sub);
}
.rb-txt {
  flex: 1;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.rb-x {
  flex: none;
  width: 20px;
  height: 20px;
  border-radius: 4px;
  color: var(--c-sub);
}
.rb-x:hover {
  background: var(--c-hover);
  color: var(--c-text);
}
</style>
