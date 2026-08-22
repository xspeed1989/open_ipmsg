// 全局响应式状态 + 后端事件桥接
import { reactive } from 'vue'
import { listen } from '@tauri-apps/api/event'
import * as ipc from './lib/ipc'

export const store = reactive({
  booted: false,
  /** 页面模式：chat 会话 | contacts 通讯录 */
  page: 'chat',
  settingsOpen: false,
  firstRun: false,
  search: '',

  /** 本机配置（nickname/group/download_dir/encoding/hostname/ips） */
  config: null,

  /** 在线用户 PeerInfo[] 与 key->PeerInfo 映射 */
  users: [],
  userMap: {},

  /** key -> { msgs: [] }，历史与会话内容 */
  chats: {},
  peerMeta: {}, // key -> { nickname, host, group } 快照（离线也能显示）

  activeKey: null,
  unread: {}, // key -> 未读数
  lastTs: {}, // key -> 最后消息时间戳
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
const DAY = 86400000

export function fmtTime(ts) {
  if (!ts) return ''
  const d = new Date(ts * 1000)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** 会话列表右上角时间：今天 HH:mm / 昨天 / 更早 MM-DD */
export function fmtListTime(ts) {
  if (!ts) return ''
  const d = new Date(ts * 1000)
  const now = new Date()
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000
  if (ts >= midnight) return fmtTime(ts)
  if (ts >= midnight - DAY) return '昨天'
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** 消息流中的日期分隔标签 */
export function dayLabel(ts) {
  const d = new Date(ts * 1000)
  const now = new Date()
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000
  if (ts >= midnight) return '今天'
  if (ts >= midnight - DAY) return '昨天'
  const wd = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][d.getDay()]
  if (ts >= midnight - 6 * DAY) return wd
  const sameYear = d.getFullYear() === now.getFullYear()
  return `${sameYear ? '' : d.getFullYear() + '年'}${d.getMonth() + 1}月${d.getDate()}日`
}

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
}

export async function loadUsers() {
  try {
    store.users = await ipc.getUsers()
    const map = {}
    for (const u of store.users) map[u.key] = u
    store.userMap = map
    for (const u of store.users) {
      store.peerMeta[u.key] = {
        nickname: u.nickname,
        host: u.host,
        group: u.group,
      }
    }
  } catch (e) {
    console.error('loadUsers failed', e)
  }
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
  ensureChat(key).then((c) => {
    c.msgs.push(msg)
    if (msg.ts) store.lastTs[key] = Math.max(store.lastTs[key] || 0, msg.ts)
    if (msg.peer) store.peerMeta[key] = msg.peer
  })
}

export async function openChat(key) {
  store.page = 'chat'
  await ensureChat(key)
  store.activeKey = key
  if (store.unread[key]) {
    delete store.unread[key]
  }
}

export async function sendText(text) {
  const key = store.activeKey
  if (!key || !text.trim()) return
  const msg = await ipc.sendText(key, text)
  pushMsg(key, msg)
}

export async function sendFiles(paths) {
  const key = store.activeKey
  if (!key || !paths?.length) return
  const msg = await ipc.sendFiles(key, paths)
  pushMsg(key, msg)
}

/** 发起文件下载；进度通过 file-progress 事件回填 */
export async function downloadFile(msg, file) {
  const key = msg.peer_key || msg.peer?.key
  file.state = 'downloading'
  file.transferred = 0
  try {
    await ipc.downloadFile(key, msg.pkt, file.id, file.name)
  } catch (e) {
    file.state = 'failed'
    file.error = String(e)
  }
}

function onFileProgress(p) {
  const { key, pkt, file_id: fileId, transferred, total, done, path, error } = p
  const chat = store.chats[key]
  if (!chat) return
  const msg = chat.msgs.find((m) => m.pkt === pkt)
  if (!msg) return
  const f = (msg.files || []).find((x) => x.id === fileId)
  if (!f) return
  f.transferred = transferred
  f.total = total ?? f.size
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

/* ---------------- 启动 ---------------- */

let bootedOnce = false

export async function boot() {
  if (bootedOnce) return
  bootedOnce = true

  await refreshConfig()

  await ipc.listenEvent(ipc.EVT.usersUpdated, () => loadUsers())
  await ipc.listenEvent(ipc.EVT.msgIn, ({ key, msg }) => {
    pushMsg(key, msg)
    if (!(store.page === 'chat' && store.activeKey === key)) {
      store.unread[key] = (store.unread[key] || 0) + 1
    }
  })
  await ipc.listenEvent(ipc.EVT.fileProgress, onFileProgress)

  await loadUsers()

  if (!store.config.nickname) store.firstRun = true
  if (store.firstRun) store.settingsOpen = true

  store.booted = true
}
