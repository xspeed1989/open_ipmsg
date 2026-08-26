// 全局响应式状态 + 后端事件桥接
import { reactive, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  isPermissionGranted, requestPermission, sendNotification,
} from '@tauri-apps/plugin-notification'
import * as ipc from './lib/ipc'
import { splitDelayedNote } from './lib/text'
import { pickLatestUnread, pickLatestActive } from './lib/unread'
import { unreadReceiptPkts } from './lib/receipts'
import { mergeSessions } from './lib/sessions'
import { applyTheme } from './lib/theme'
import { t, setLocale, detectLocale, dayLabel as i18nDayLabel } from './lib/i18n'

export const store = reactive({
  booted: false,
  settingsOpen: false,
  firstRun: false,
  search: '',

  /** 本机配置（nickname/group/download_dir/encoding/encrypt/hostname/ips/key_fp） */
  config: null,

  /** 在线用户 PeerInfo[] 与 key->PeerInfo 映射 */
  users: [],
  userMap: {},
  /** 离线历史会话（list_sessions） */
  sessions: [],
  /** 中栏列表 = 在线用户 ∪ 离线会话（mergeSessions 结果） */
  sessionList: [],

  /** key -> { msgs: [] }，历史与会话内容 */
  chats: {},
  peerMeta: {}, // key -> { nickname, host, group } 快照（离线也能显示）

  activeKey: null,
  unread: {}, // key -> 未读数
  unreadTs: {}, // key -> 最近一条未读消息的时间戳（决定托盘唤起时跳到哪个会话）
  searchHits: [], // 中栏搜索命中的聊天记录
  searching: false,
  /** 需要在聊天窗口里定位并高亮的消息：{ key, pkt, query } */
  locate: null,
  /** 拖放文件时中栏正在悬停的联系人 key（中栏行高亮用；'' 表示不在联系人上） */
  dragHoverKey: '',
  lastTs: {}, // key -> 最后消息时间戳
  windowFocused: true, // 主窗口是否聚焦（决定是否弹通知/自动已读）
})

/* ---------------- 工具函数 ---------------- */

export function fmtSize(n) {
  if (n == null) return ''
  if (n < 1024) return n + ' B'
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB'
  if (n < 1024 * 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB'
  return (n / 1024 / 1024 / 1024).toFixed(2) + ' GB'
}

function pad(x) {
  return String(x).padStart(2, '0')
}

export function fmtTime(ts) {
  if (!ts) return ''
  const d = new Date(ts * 1000)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** 消息流中的日期分隔标签（文案按当前界面语言） */
export const dayLabel = (ts) => i18nDayLabel(ts)

export function displayName(key) {
  const u = store.userMap[key]
  if (u && u.nickname) return u.nickname
  const m = store.peerMeta[key]
  if (m && m.nickname) return m.nickname
  return key || ''
}

/* ---------------- 数据动作 ---------------- */

export async function refreshConfig() {
  store.config = await ipc.getConfig()
  // 配置里的主题立刻生效（首屏与保存设置后都走这里）
  applyTheme(store.config?.theme)
  // 界面语言：config.lang 未设置（空串 = 跟随系统）时按系统语言探测
  setLocale(store.config?.lang || detectLocale())
}

export { applyTheme }

/** 清洗后端文本中的控制字符，避免污染界面 */
function clean(s) {
  return typeof s === 'string' ? s.replace(/[\u0000-\u001f\u007f]/g, '') : ''
}

export async function loadUsers() {
  try {
    store.users = await ipc.getUsers()
    const map = {}
    for (const u of store.users) {
      map[u.key] = {
        ...u,
        nickname: clean(u.nickname) || clean(u.user),
        host: clean(u.host),
        group: clean(u.group),
        user: clean(u.user),
      }
    }
    store.userMap = map
    for (const u of store.users) {
      store.peerMeta[u.key] = {
        nickname: clean(u.nickname) || clean(u.user),
        host: clean(u.host),
        group: clean(u.group),
      }
    }
    mergeSessionList()
  } catch (e) {
    console.error('loadUsers failed', e)
  }
}

/** 拉取离线历史会话并合并进中栏列表（在线优先，离线补位） */
export async function loadSessions() {
  try {
    store.sessions = await ipc.listSessions()
    // 离线会话也进 peerMeta：打开历史会话时昵称/群组能正常显示
    for (const s of store.sessions) {
      if (!store.peerMeta[s.key]) {
        store.peerMeta[s.key] = {
          nickname: clean(s.nickname) || clean(s.key),
          host: clean(s.host),
          group: clean(s.group),
        }
      }
    }
    mergeSessionList()
  } catch (e) {
    console.error('loadSessions failed', e)
  }
}

/** 用「在线用户 ∪ 离线会话」重建中栏列表 */
function mergeSessionList() {
  store.sessionList = mergeSessions(store.users, store.sessions)
}

export async function refreshUsers() {
  await ipc.refreshUsers()
  // 稍等回应后再拉一次列表（users-updated 事件也会触发）
  setTimeout(loadUsers, 600)
}

export async function ensureChat(key) {
  if (!store.chats[key]) {
    let msgs = []
    try {
      msgs = await ipc.getHistory(key)
    } catch (e) {
      console.error('getHistory failed', e)
    }
    for (const m of msgs) {
      if (m.peer) store.peerMeta[key] = m.peer
      if (m.ts) store.lastTs[key] = Math.max(store.lastTs[key] || 0, m.ts)
    }
    store.chats[key] = { msgs }
  }
  return store.chats[key]
}

export function pushMsg(key, msg) {
  // 返回 Promise：调用方需要在消息真正入列后再做已读处理
  return ensureChat(key).then((c) => {
    // 对端"延迟发送"会以相同包号反复重发：原地替换，并保留本地运行时状态
    // （下载进度/保存路径/已读），既不刷屏也不丢已完成的状态
    const idx = c.msgs.findIndex((m) => m.dir === msg.dir && m.pkt === msg.pkt)
    if (idx >= 0) {
      const old = c.msgs[idx]
      for (const f of msg.files || []) {
        // 保留旧卡片的非 pending 状态（下载中/已完成/失败）与本地路径、原因
        const prev = (old.files || []).find(
          (x) => x.id === f.id && x.state && x.state !== 'pending'
        )
        if (prev) {
          f.state = prev.state
          f.path = prev.path ?? f.path
          f.transferred = prev.transferred
          f.error = prev.error
          if (prev.src) f.src = prev.src
        }
      }
      if (old.read && msg.dir === 'in') msg.read = true
      c.msgs.splice(idx, 1, msg)
    } else {
      c.msgs.push(msg)
    }
    if (msg.ts) store.lastTs[key] = Math.max(store.lastTs[key] || 0, msg.ts)
    if (msg.peer) store.peerMeta[key] = msg.peer
  })
}

/**
 * 清空某会话的聊天记录（仅本地，不通知对方）。
 * 同时清掉未读计数与列表里的时间排序依据，返回删掉的条数。
 */
export async function clearHistory(key) {
  if (!key) return 0
  const n = await ipc.clearHistory(key)
  if (store.chats[key]) store.chats[key].msgs = []
  delete store.unread[key]
  delete store.unreadTs[key]
  delete store.lastTs[key]
  return n
}

export async function openChat(key) {
  await ensureChat(key)
  store.activeKey = key
  await markReadFor(key)
}

/* ---------------- 已读回执 ---------------- */

/**
 * 用户正在查看某会话 → 把该会话的入站消息全部标记为已读：
 * 中栏未读红点清零 + 本地乐观置位 + 通知后端持久化。
 *
 * 传给后端的是"所有未读入站包号"，不只是要求回执的那些：后端只会对带
 * READCHECKOPT 的消息回 READMSG（见 pending_receipts），其余仅落库已读状态，
 * 这样 read 标志对所有入站消息都成立，未读数才能和已读状态保持一致。
 */
export async function markReadFor(key) {
  if (!key) return 0
  // 未读数先清零：哪怕这个会话里没有需要回执的消息，提示也该消失
  if (store.unread[key]) delete store.unread[key]
  delete store.unreadTs[key]

  const chat = store.chats[key]
  if (!chat) return 0
  const unread = chat.msgs.filter((m) => m.dir === 'in' && !m.read)
  if (!unread.length) return 0
  const pkts = [...new Set(unread.map((m) => m.pkt))]
  for (const m of unread) m.read = true // 乐观置位，避免连发时重复回执
  try {
    await ipc.markRead(key, pkts)
  } catch (e) {
    console.error('mark_read failed', e)
  }
  return pkts.length
}

function onMsgRead({ key, pkt }) {
  const chat = store.chats[key]
  if (!chat) return
  // 这是对端回来的 READMSG：被读的是「我方发出」的那条消息
  const msg = chat.msgs.find((m) => m.pkt === pkt && m.dir === 'out')
  if (msg) msg.read = true
}

/* ---------------- 系统通知 ---------------- */

async function notify(key, title, body) {
  try {
    // Linux：走原生可点击通知（点击通知 → open-chat 事件 → 弹窗切到对应会话）。
    // DBus 通知无需权限申请，绕开插件的权限检查直接发
    if (/linux/i.test(navigator.userAgent)) {
      await ipc.notifyMessage(key, title, body, store.config?.lang || '')
      return
    }
    let granted = await isPermissionGranted()
    if (!granted) granted = (await requestPermission()) === 'granted'
    if (granted) sendNotification({ title, body })
  } catch (e) {
    console.error('notify failed', e)
  }
}

/** 消息预览文本（通知用） */
export { splitDelayedNote }

export function previewText(msg) {
  if (msg.kind === 'file') {
    const n = (msg.files || []).length
    const name = msg.files?.[0]?.name || ''
    const head = `${t('file.tag')} ${name}`
    return n > 1 ? head + t('preview.etc', { n }) : head
  }
  const { body, delayed } = splitDelayedNote(msg.text)
  const tip = (body || (delayed !== null ? t('preview.offline') : '')).replace(/\s+/g, ' ')
  return tip.length > 48 ? tip.slice(0, 48) + '…' : tip
}

/** 消息可见（聊天已打开且窗口聚焦）时视为已读 */
function isChatVisible(key) {
  return store.windowFocused && store.activeKey === key
}

/** 发送文本到指定会话（转发/批量发送用）；对方离线时后端自动入队，返回的
 * 记录带 queued 标记，气泡上显示「离线留言·上线后自动投递」 */
export async function sendTextTo(key, text) {
  if (!key || !text?.trim()) return null
  const msg = await ipc.sendText(key, text)
  await pushMsg(key, msg)
  return msg
}

/** 发送文本到当前会话 */
export async function sendText(text) {
  return sendTextTo(store.activeKey, text)
}

/** 发送文件/文件夹到指定会话；target 省略时发给当前会话（拖放到列表某个用户时指定目标） */
export async function sendFilesTo(key, paths, text = '') {
  if (!key || !paths?.length) return null
  const msg = await ipc.sendFiles(key, paths, text)
  await pushMsg(key, msg)
  return msg
}

/** 发送文件/文件夹；target 省略时发给当前会话（拖放到列表某个用户时指定目标） */
export async function sendFiles(paths, target) {
  return sendFilesTo(target || store.activeKey, paths)
}

/** 发送剪贴板里的图片（对端按普通附件接收，本客户端内联显示） */
export async function sendClipboardImageTo(key, b64, mime, text = '') {
  if (!key || !b64) return null
  const msg = await ipc.sendClipboardImage(key, text, b64, mime)
  await pushMsg(key, msg)
  return msg
}

/** 发送剪贴板里的图片到当前会话 */
export async function sendClipboardImage(b64, mime, text = '') {
  return sendClipboardImageTo(store.activeKey, b64, mime, text)
}

/** 发起文件下载；进度通过 file-progress 事件回填 */
export async function downloadFile(msg, file) {
  const key = msg.peer_key || msg.peer?.key
  file.state = 'downloading'
  file.transferred = 0
  try {
    await ipc.downloadFile(
      key, msg.pkt, file.id, file.name, file.rid || '', file.size || 0, !!file.dir_entry,
    )
  } catch (e) {
    file.state = 'failed'
    file.error = String(e)
  }
}

function onFileProgress(p) {
  const { key, pkt, file_id: fileId, transferred, total, done, path, error } = p
  const chat = store.chats[key]
  if (!chat) return
  const msg = chat.msgs.find((m) => m.pkt === pkt && m.dir === 'in')
  if (!msg) return
  const f = (msg.files || []).find((x) => x.id === fileId)
  if (!f) return
  f.transferred = transferred
  f.total = total || f.size
  if (error) {
    f.state = 'failed'
    f.error = error
    return
  }
  if (done) {
    f.state = 'done'
    f.path = path || f.path
  }
}

/* ---------------- 聊天记录搜索 ---------------- */

let searchTimer = null
/** 中栏搜索框输入：联系人本地过滤，聊天记录走后端全文搜索（防抖 200ms） */
export function onSearchInput(q) {
  store.search = q
  clearTimeout(searchTimer)
  const query = (q || '').trim()
  if (!query) {
    store.searchHits = []
    store.searching = false
    return
  }
  store.searching = true
  searchTimer = setTimeout(async () => {
    try {
      store.searchHits = await ipc.searchHistory(query, undefined, 80)
    } catch (e) {
      console.error('search_history failed', e)
      store.searchHits = []
    } finally {
      store.searching = false
    }
  }, 200)
}

/**
 * 打开某条搜索结果：切到对应会话，并请聊天窗口定位、高亮那条消息。
 * 记录可能很旧，不一定在默认加载的 300 条里，这里按需把历史整段拉回来。
 */
export async function openHit(hit) {
  if (!hit?.key) return
  await openChat(hit.key)
  const chat = store.chats[hit.key]
  const has = (chat?.msgs || []).some((m) => m.pkt === hit.pkt)
  if (!has) {
    try {
      const msgs = await ipc.getHistory(hit.key, 5000)
      if (store.chats[hit.key]) store.chats[hit.key].msgs = msgs
    } catch (e) {
      console.error('getHistory failed', e)
    }
  }
  store.locate = { key: hit.key, pkt: hit.pkt, query: store.search.trim() }
}

/** 未读总数（所有会话相加），驱动托盘图标的闪烁 */
export function totalUnread() {
  return Object.values(store.unread).reduce((a, b) => a + (b || 0), 0)
}

/** 未读会话中最近收到消息的那个（从托盘唤起时直接跳过去）；都读完了返回空 */
export function latestUnreadKey() {
  return pickLatestUnread(store.unread, store.unreadTs, store.lastTs)
}

/* ---------------- 启动 ---------------- */

let bootedOnce = false

export async function boot() {
  if (bootedOnce) return
  bootedOnce = true

  await refreshConfig()

  // 窗口焦点跟踪：失焦时来消息弹通知；重新聚焦自动标记已读
  getCurrentWindow().onFocusChanged(({ payload: focused }) => {
    store.windowFocused = focused
    if (focused && store.activeKey) {
      markReadFor(store.activeKey)
    }
  })

  await ipc.listenEvent(ipc.EVT.usersUpdated, () => {
    loadUsers()
    loadSessions()
  })
  await ipc.listenEvent(ipc.EVT.msgIn, async ({ key, msg, resend }) => {
    if (!key || !msg) return
    // 对端延迟重发（同包号重复投递）不重复计未读、不重复通知
    const isRebroadcast =
      resend ||
      (store.chats[key]?.msgs || []).some((m) => m.dir === 'in' && m.pkt === msg.pkt)
    // 等消息真正入列再判断已读，否则 markReadFor 可能看不到这条新消息
    await pushMsg(key, msg)
    // 「对方回话即视为已读」兜底：飞秋等实现不会对我们带 READCHECKOPT 的
    // 消息回 READMSG，出站消息的未读标记会永远挂着。对端既然发来了新消息，
    // 人就在对话里 —— 把此前要求回执且未读的出站消息翻成已读（本地+落库）。
    if (msg.dir === 'in' && !isRebroadcast) {
      const pkts = unreadReceiptPkts(store.chats[key]?.msgs)
      if (pkts.length) {
        for (const m of store.chats[key].msgs) {
          if (pkts.includes(m.pkt)) m.read = true
        }
        ipc.markOutRead(key, pkts).catch((e) => console.error('mark_out_read failed', e))
      }
    }
    if (isChatVisible(key)) {
      await markReadFor(key)
    } else if (!isRebroadcast) {
      store.unread[key] = (store.unread[key] || 0) + 1
      store.unreadTs[key] = msg.ts || Math.floor(Date.now() / 1000)
      notify(key, displayName(key), previewText(msg))
    }
  })
  // 托盘唤起主窗口：跳到最新未读会话（没有未读就跳到最近活动的会话）。
  // 切会话必须先做——show/setFocus 抛错不能挡住 openChat（Wayland 重映射后
  // 窗口操作偶发异常会吞掉后面的代码，open-chat 曾踩过同一个坑）。
  await ipc.listenEvent(ipc.EVT.openUnread, async () => {
    try {
      const key = latestUnreadKey() || pickLatestActive(store.lastTs, store.unreadTs)
      if (key) await openChat(key)
      const w = getCurrentWindow()
      await w.show()
      await w.unminimize()
      await w.setFocus()
    } catch (e) {
      console.error('open-unread failed', e)
    }
  })
  // 点击系统通知（Linux 原生）：弹出主窗口并切到发出这条消息的会话。
  // 注意：切会话必须先做——窗口 show/setFocus 若抛错不能挡住 openChat。
  await ipc.listenEvent(ipc.EVT.openChat, async ({ key }) => {
    try {
      if (!key) return
      await openChat(key)
      const w = getCurrentWindow()
      await w.show()
      await w.unminimize()
      await w.setFocus()
    } catch (e) {
      console.error('open-chat failed', e)
    }
  })
  await ipc.listenEvent(ipc.EVT.msgRead, onMsgRead)
  await ipc.listenEvent(ipc.EVT.fileProgress, onFileProgress)

  // 未读总数变化 → 托盘图标在「有新消息（红点角标）」与「无消息」之间切换
  watch(
    () => totalUnread(),
    (n) => {
      ipc.setUnread(n).catch((e) => console.error('set_unread failed', e))
    },
    { immediate: true }
  )

  await loadUsers()
  await loadSessions()

  if (!store.config.nickname) store.firstRun = true
  if (store.firstRun) store.settingsOpen = true

  store.booted = true
}
