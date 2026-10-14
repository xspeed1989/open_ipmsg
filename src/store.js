// 全局响应式状态 + 后端事件桥接
import { reactive, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  isPermissionGranted, requestPermission, sendNotification,
} from '@tauri-apps/plugin-notification'
import * as ipc from './lib/ipc.js'
import { splitDelayedNote } from './lib/text.js'
import { applyUnreadFromSessions } from './lib/unread.js'
import { mergeSessions } from './lib/sessions.js'
import {
  BROADCAST_SESSION_KEY, isBroadcastSession, senderOf, senderIp,
} from './lib/sessions.js'
import { normalizeEmojis } from './lib/emoji.js'
import { resolveRuntimeCloseHotkey } from './lib/hotkey.js'
import { sealedPreviewKey } from './lib/secret.js'
import { replayFileProgress } from './lib/fileprogress.js'
// 事件驱动的那半边迁移（见 docs/adr/0001）：本文件只负责状态容器、I/O 与用户动作
import * as transitions from './lib/transitions.js'
import { applyTheme } from './lib/theme.js'
import { t, setLocale, detectLocale, dayLabel as i18nDayLabel } from './lib/i18n.js'

export const store = reactive({
  booted: false,
  settingsOpen: false,
  firstRun: false,
  search: '',

  /** 本机配置（nickname/group/download_dir/encoding/encrypt/hostname/ips/key_fp） */
  config: null,

  /**
   * 本次运行**实际生效**的关闭窗口快捷键（规范形；`null` = 配置尚未加载，空串 = 禁用）。
   * 快捷键一律改配置不重启不换绑：首次成功加载配置后由 refreshConfig 冻结一次，
   * 之后不再覆盖；App 的 keydown 处理器读它（见 resolveRuntimeCloseHotkey）。
   */
  runtimeCloseHotkey: null,

  /** 在线用户 PeerInfo[] 与 key->PeerInfo 映射 */
  users: [],
  userMap: {},
  /** 离线历史会话（list_sessions） */
  sessions: [],
  /** 中栏列表 = 在线用户 ∪ 离线会话（mergeSessions 结果） */
  sessionList: [],

  /** key -> { msgs: [] }，历史与会话内容 */
  chats: {},
  /** 早于消息入列到达的下载进度事件（成因见 lib/fileprogress.js）：这是**事件机制
   *  的状态**，不是界面状态；放进来是因为事件迁移 module 要就地改它 */
  pendingFileProgress: new Map(),
  peerMeta: {}, // key -> { nickname, host, group } 快照（离线也能显示）

  activeKey: null,
  chatOpenVersion: 0, // 同一会话再次打开也要触发最新消息定位
  unread: {}, // key -> 未读数
  unreadTs: {}, // key -> 最近一条未读消息的时间戳（决定托盘唤起时跳到哪个会话）
  searchHits: [], // 中栏搜索命中的聊天记录
  searching: false,
  /** 需要在聊天窗口里定位并高亮的消息：{ key, pkt, query } */
  locate: null,
  /** 拖放文件时中栏正在悬停的联系人 key（中栏行高亮用；'' 表示不在联系人上） */
  dragHoverKey: '',
  /** 自定义表情库：图片文件名 / 已发出的缓存副本名（聊天里识别「表情」按缩略渲染） */
  emojiFiles: [],
  emojiCacheFiles: [],
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

/** 「广播」信箱的固定会话 key（与后端 state::BROADCAST_SESSION_KEY 一致）：
 *  收发的广播都进这一个常驻条目，不按发送方拆成多个会话 */
export const BROADCAST_KEY = BROADCAST_SESSION_KEY

/** 会话是否就是「广播」信箱 */
export const isBroadcastKey = isBroadcastSession

/** 会话列表里显示的名字：广播信箱固定叫「广播」，不随最新发送方改名 */
export function sessionName(key) {
  if (isBroadcastKey(key)) return t('broadcast.name')
  return displayName(key)
}

/** 广播信箱里每条消息的发送方名（纯函数，见 lib/sessions.js；文案在此注入） */
export function msgSenderName(m) {
  return senderOf(m, { me: t('me'), unknown: t('unknown') })
}

/** 发送方 IP：同一昵称可能有多台机器，气泡标签右侧给出 IP 便于区分 */
export const senderIpOf = senderIp

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
  // 关闭窗口快捷键：本次运行只冻结一次（首次成功加载配置后），改配置不重启不换绑。
  // null=尚未加载 → 采用本次加载值；空串=禁用，原样保留，不回落默认 Ctrl+W。
  store.runtimeCloseHotkey = resolveRuntimeCloseHotkey(
    store.runtimeCloseHotkey,
    store.config?.close_hotkey ?? ''
  )
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
          ip: clean(s.ip),
          group: clean(s.group),
        }
      }
    }
    // 启动补数：WebView 监听就绪前到达的消息（对端离线留言在我方上线瞬间
    // 重投即属此列）事件会被丢弃，但后端已落库——用会话摘要的未读计数把
    // 中栏红点与托盘闪烁补回来（只增不覆盖，重复调用安全）
    applyUnreadFromSessions(store.unread, store.unreadTs, store.chats, store.sessions)
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
    // 历史加载是「消息入列」的一条路径：等在这里的完成事件此刻才落得下去
    replayFileProgress(store.pendingFileProgress, store.chats[key], key)
  }
  return store.chats[key]
}

/**
 * 消息入列（用户动作路径：发送、表情、广播都走这里）。
 * 合并与保真的数学在 transitions.insertMessage，那边的事件路径用同一个原语，
 * 不会出现第二份。返回 Promise：调用方需要在消息真正入列后再做已读处理。
 */
export function pushMsg(key, msg) {
  return ensureChat(key).then(() => {
    transitions.insertMessage(store, key, msg)
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

/**
 * 删除会话（微信式）：后端清掉本地记录并把联系人从列表隐藏，
 * 对端再发消息时自动恢复并重新出现。前端同步清掉本地缓存、
 * 未读角标与列表中的该联系人，并和过滤后的后端视图对齐。
 */
export async function deleteContact(key) {
  if (!key) return
  try {
    await ipc.deleteContact(key)
  } catch (e) {
    console.error('delete_contact failed', e)
  }
  delete store.chats[key] // 历史缓存一并丢弃，避免重新打开时回显已删内容
  delete store.unread[key]
  delete store.unreadTs[key]
  delete store.lastTs[key]
  delete store.peerMeta[key]
  delete store.userMap[key]
  const ui = store.users.findIndex((u) => u.key === key)
  if (ui >= 0) store.users.splice(ui, 1)
  if (store.activeKey === key) store.activeKey = null
  // 与后端视图对齐（get_users / list_sessions 已过滤被删联系人；
  // 后端发的 users-updated 事件也会触发同样刷新，重复调用安全）
  await loadUsers()
  await loadSessions()
}

export async function openChat(key) {
  await ensureChat(key)
  store.activeKey = key
  store.chatOpenVersion += 1
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
  // 「算什么」在 transitions.markChatRead（事件路径用同一个原语），「发什么」在这里
  const pkts = transitions.markChatRead(store, key)
  if (!pkts.length) return 0
  try {
    await ipc.markRead(key, pkts)
  } catch (e) {
    console.error('mark_read failed', e)
  }
  return pkts.length
}

/* ---- onMsgRead / onMsgDelivered / onFileProgress / isChatVisible 已搬到
   lib/transitions.js：它们都是「事件到达后状态怎么变」，属于事件迁移的那半边 ---- */

/* ---------------- 系统通知 ---------------- */

async function notify(key, title, body) {
  try {
    // Linux/Windows：走原生可点击通知（点击通知 → open-chat 事件 → 弹窗切到对应会话）。
    // 两边的原生通知都无需权限申请，绕开插件的权限检查直接发；
    // macOS 仍走下面的插件路径（没有等价的点击回调）。
    if (/linux|windows/i.test(navigator.userAgent)) {
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
  // 未开封的封书/密码消息：通知正文里也不能带出「信里」的内容，否则气泡上
  // 遮住了、系统通知却把正文弹在屏幕上（与信封占位是同一条规则）。
  const sealedKey = sealedPreviewKey(msg)
  if (sealedKey) return t(sealedKey)
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

/** 发送文本到指定会话（转发/群发用）；对方离线时后端自动入队，返回的
 * 记录带 queued 标记，气泡上显示「离线留言·上线后自动投递」。
 * 后端返回记录**数组**：加密模式下长文本会自动拆成多条发送，每条一条记录、各自气泡。 */
export async function sendTextTo(key, text, secret = false, password = false) {
  if (!key || !text?.trim()) return null
  const msgs = await ipc.sendText(key, text, secret, password)
  const list = Array.isArray(msgs) ? msgs : [msgs] // 兼容旧后端单对象返回
  for (const m of list) await pushMsg(key, m)
  return list[list.length - 1] || null
}

/** 发送文本到当前会话 */
export async function sendText(text) {
  return sendTextTo(store.activeKey, text)
}

/** 发送文件/文件夹到指定会话；target 省略时发给当前会话（拖放到列表某个用户时指定目标）。
 * 后端返回记录**数组**：加密模式下附件公告的文本段超限会自动拆段——
 * 首段带附件、其余段纯文本，每条一条记录、各自气泡。 */
export async function sendFilesTo(key, paths, text = '', secret = false, password = false) {
  if (!key || !paths?.length) return null
  const msgs = await ipc.sendFiles(key, paths, text, secret, password)
  const list = Array.isArray(msgs) ? msgs : [msgs] // 兼容旧后端单对象返回
  for (const m of list) await pushMsg(key, m)
  return list[list.length - 1] || null
}

/** 发送文件/文件夹；target 省略时发给当前会话（拖放到列表某个用户时指定目标） */
export async function sendFiles(paths, target) {
  return sendFilesTo(target || store.activeKey, paths)
}

/** 发送剪贴板里的图片（对端按普通附件接收，本客户端内联显示）。
 * 同 sendFilesTo：附带长文本超限时后端自动拆段，按数组逐条上屏 */
export async function sendClipboardImageTo(key, b64, mime, text = '') {
  if (!key || !b64) return null
  const msgs = await ipc.sendClipboardImage(key, text, b64, mime)
  const list = Array.isArray(msgs) ? msgs : [msgs]
  for (const m of list) await pushMsg(key, m)
  return list[list.length - 1] || null
}

/** 发送剪贴板里的图片到当前会话 */
export async function sendClipboardImage(b64, mime, text = '') {
  return sendClipboardImageTo(store.activeKey, b64, mime, text)
}

/* ---------------- 撤回 / 开封 / 不在 / 广播 / 群发 ---------------- */

/** 撤回我方发出的文本消息（DELMSG）：本地记录标记 recalled，气泡替换占位 */
export async function recallMsg(key, pkt) {
  await ipc.recallMessage(key, pkt)
  const chat = store.chats[key]
  const m = chat?.msgs.find((x) => x.dir === 'out' && x.pkt === pkt)
  if (m) m.recalled = true
}

/** 封书/密码锁开封（密码锁场景校验输入） */
export async function unlockMsg(key, pkt, password = null) {
  await ipc.unlockMessage(key, pkt, password)
  const chat = store.chats[key]
  const m = chat?.msgs.find((x) => x.pkt === pkt)
  if (m) {
    m.unlocked = true
    m.locked = false
    m.read = true // 开封即已读（后端同时补发 READMSG）
  }
}

/** 不在模式开关（BR_ABSENCE + ABSENCEOPT） */
export function setAbsence(on, text) {
  return ipc.setAbsence(on, text)
}

/** 广播群发（BROADCASTOPT 同报）。
 *  历史由后端写入「广播」信箱并发 msg-out 事件，这里不重复上屏。 */
export function broadcastTo(text) {
  return ipc.broadcastMessage(text)
}

/** 主动索取对端不在通知文 */
export function getAbsenceInfoFor(key) {
  return ipc.getAbsenceInfo(key)
}

/** 主动发起主机列表交换（BR_ISGETLIST → ANSLIST） */
export function requestHostlist() {
  return ipc.requestHostlist()
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
  setLocate({ key: hit.key, pkt: hit.pkt, query: store.search.trim() })
}

/** 未读总数（所有会话相加），驱动托盘图标的闪烁 */
export function totalUnread() {
  return Object.values(store.unread).reduce((a, b) => a + (b || 0), 0)
}

/** 未读会话中最近收到消息的那个（从托盘唤起时直接跳过去）；都读完了返回空 */
export function latestUnreadKey() {
  return transitions.latestUnreadKey(store)
}

/* ---------------- 自定义表情库 ---------------- */

/**
 * 拉取表情库文件名索引（供聊天里把「表情」按缩略渲染）。
 * emojiFiles：库内文件名；emojiCacheFiles：最近一次发送时缓存目录里的副本名
 * （自己发出的表情在消息里显示的是缓存副本，只能靠这张映射认出来）。
 */
export async function loadEmojiIndex() {
  const list = normalizeEmojis(await ipc.listEmojis())
  setStickerIndex(list)
  return list
}

/**
 * 发送一张自定义表情到指定会话。
 * 后端按官方「粘贴图片」协议公告，返回的记录与 send_text 同构，
 * 这里走同一条上屏链路（pushMsg），保证气泡、未读、落下与普通消息一致。
 */
export async function sendEmojiTo(key, id, text = '') {
  if (!key || !id) return null
  const msgs = await ipc.sendEmoji(key, id, text)
  const list = Array.isArray(msgs) ? msgs : [msgs]
  for (const m of list) await pushMsg(key, m)
  await loadEmojiIndex() // 发送会更新「缓存副本名」映射
  return list[list.length - 1] || null
}

/* ---------------- 界面状态的写侧 ---------------- */

// 组件只读 store，写一律走这里：字段的**形状**于是成了 action 的入参契约，
// 而不再是「11 个组件都知道那个字段长什么样」。契约由 scripts/store-writes.test.mjs
// 的源码扫描守着（组件里不许出现 `store.x =`）。

/** 设置面板开关（侧边栏齿轮打开 / 面板自己关） */
export function setSettingsOpen(on) {
  store.settingsOpen = on
}

/** 首次运行标记（首启自动开设置面板，保存后清掉） */
export function setFirstRun(on) {
  store.firstRun = on
}

/** 搜索结果定位请求（openHit 设置，聊天窗口定位完调 clearLocate 收尾） */
export function setLocate(payload) {
  store.locate = payload
}

/** 定位已消费（聊天窗口定位完清空，避免重复滚动） */
export function clearLocate() {
  store.locate = null
}

/** 拖放悬停的会话 key（'' = 不在任何联系人上） */
export function setDragHover(key) {
  store.dragHoverKey = key || ''
}

/**
 * 表情索引：`list → file / cacheFile` 的映射只在这里做一次。
 * EmojiPicker 拿到新列表、loadEmojiIndex 拉完索引，都走这里 ——
 * 之前两处各写一遍同样的 map+filter。
 */
export function setStickerIndex(list) {
  const items = list || []
  store.emojiFiles = items.map((e) => e.file)
  store.emojiCacheFiles = items.map((e) => e.cacheFile).filter(Boolean)
}

/* ---------------- 事件迁移的执行者 ---------------- */

/**
 * 事件入口：先补齐 I/O 前置（消息类事件得先把历史拉回来，消息才算真正入列），
 * 再交给 lib/transitions.js 做迁移，最后按顺序执行它返回的 effect 清单。
 */
async function dispatchEvent(event, payload) {
  if (event === 'msg-in' || event === 'msg-out') {
    if (payload?.key) await ensureChat(payload.key)
  }
  const { effects } = transitions.applyEvent(store, event, payload)
  await runEffects(effects)
}

/** effect 清单的执行者：IPC 往返、通知、列表重载、窗口动作都在这边 */
async function runEffects(effects) {
  for (const e of effects) {
    if (e.type === 'mark_read') {
      try {
        await ipc.markRead(e.key, e.pkts)
      } catch (err) {
        console.error('mark_read failed', err)
      }
    } else if (e.type === 'mark_out_read') {
      try {
        await ipc.markOutRead(e.key, e.pkts)
      } catch (err) {
        console.error('mark_out_read failed', err)
      }
    } else if (e.type === 'notify') {
      notify(e.key, displayName(e.key), previewText(e.msg))
    } else if (e.type === 'reload_people') {
      loadUsers()
      loadSessions()
    } else if (e.type === 'open_chat') {
      await openChatAndFocus(e.key, e.logLabel)
    }
  }
}

/**
 * 切会话 + 把窗口弄到前面。**切会话必须先做** —— show/setFocus 抛错不能挡住
 * openChat（Wayland 重映射后窗口操作偶发异常，open-unread / open-chat 都踩过）。
 */
async function openChatAndFocus(key, logLabel) {
  try {
    if (key) await openChat(key)
    const w = getCurrentWindow()
    await w.show()
    await w.unminimize()
    await w.setFocus()
  } catch (e) {
    console.error(logLabel, e)
  }
}

/* ---------------- 启动 ---------------- */

let bootedOnce = false

export async function boot() {
  if (bootedOnce) return
  bootedOnce = true

  await refreshConfig()
  // 自定义表情库（文件名集合用于聊天里识别表情并按缩略渲染）；
  // 失败不阻塞启动：表情面板打开时会自己再拉一次
  loadEmojiIndex().catch((e) => console.error('list_emojis failed', e))

  // 窗口焦点跟踪：失焦时来消息弹通知；重新聚焦自动标记已读
  getCurrentWindow().onFocusChanged(({ payload: focused }) => {
    dispatchEvent(focused ? 'window-focus' : 'window-blur', {})
  })

  // 十一个订阅都走 dispatchEvent：迁移在 lib/transitions.js，这里只补 I/O 前置与执行 effect
  await ipc.listenEvent(ipc.EVT.usersUpdated, () => dispatchEvent('users-updated', {}))
  await ipc.listenEvent(ipc.EVT.msgIn, (p) => dispatchEvent('msg-in', p))
  // 广播信箱里「自己发出的广播」：后端已落库，这里只上屏。
  // 不算未读、不弹通知、不标已读 —— 是自己发的，不是对端消息。
  await ipc.listenEvent(ipc.EVT.msgOut, (p) => dispatchEvent('msg-out', p))
  // 托盘唤起主窗口 / 点击系统通知：跳到对应会话（没未读就退到最近活动会话）
  await ipc.listenEvent(ipc.EVT.openUnread, () => dispatchEvent('open-unread', {}))
  await ipc.listenEvent(ipc.EVT.openChat, (p) => dispatchEvent('open-chat', p))
  await ipc.listenEvent(ipc.EVT.msgRead, (p) => dispatchEvent('msg-read', p))
  await ipc.listenEvent(ipc.EVT.msgDelivered, (p) => dispatchEvent('msg-delivered', p))
  await ipc.listenEvent(ipc.EVT.fileProgress, (p) => dispatchEvent('file-progress', p))
  await ipc.listenEvent(ipc.EVT.msgRecalled, (p) => dispatchEvent('msg-recalled', p))
  await ipc.listenEvent(ipc.EVT.msgUnlocked, (p) => dispatchEvent('msg-unlocked', p))
  await ipc.listenEvent(ipc.EVT.absenceInfo, (p) => dispatchEvent('absence-info', p))

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
  if (store.firstRun) setSettingsOpen(true)

  store.booted = true
}
