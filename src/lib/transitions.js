/**
 * 后端事件 → 状态迁移（架构评审候选 2 抽出的 module，边界见 docs/adr/0001）。
 *
 * 三条约定：
 *   - **就地改 `state`**：state 就是 store 那个 reactive 对象，整对象替换会让依赖
 *     具体字段的 computed 重新包装；「纯」在这里的意思是**不碰 I/O、输出只由输入决定**；
 *   - **要做的事以 effect 清单返回**，按顺序交给调用方执行 —— 「什么时候发已读回执、
 *     什么时候落库」这类顺序事故因此从散落的 await 变成可断言的返回值；
 *   - **只覆盖事件驱动的那半边**：用户动作（发送、切会话）与启动/加载留在 store，
 *     它们按需调用这里导出的写入原语（insertMessage / markChatRead），避免出现
 *     第二份「合并入列」与「标已读」逻辑。
 *
 * effect 词汇。实现都在 store：IPC 往返与窗口动作本来就是它的差事；通知与列表重载
 * 的实现也在 store，这里只表达「该做了」，因为**决定**属于事件迁移、**送达**不属于。
 *   { type: 'mark_read',      key, pkts }        // 入站已读回执
 *   { type: 'mark_out_read',  key, pkts }        // 本地把出站消息翻成已读（不发包）
 *   { type: 'notify',         key, msg }         // 弹系统通知（标题/正文由 store 渲染）
 *   { type: 'reload_people' }                    // 重拉在线用户与会话列表
 *   { type: 'open_chat',      key, logLabel }    // 切会话 + 窗口置前
 */
import { applyFileProgress, bufferFileProgress, replayFileProgress } from './fileprogress.js'
import { pickLatestActive, pickLatestUnread } from './unread.js'
import { unreadReceiptPkts } from './receipts.js'

/* ---------------- 写入原语（事件路径与用户动作路径共用） ---------------- */

/**
 * 消息入列。同包号同方向的报文**原地替换**，并保留本地运行时状态
 * （下载进度/保存路径/已读/失败原因）：对端「延迟发送」会以相同包号反复重发，
 * 既不刷屏也不丢已经完成的状态。
 *
 * 调用方负责先 `ensureChat` —— 历史加载是 I/O，属于 store。
 * @returns {boolean} 会话不存在时返回 false（调用方应先 ensureChat）
 */
export function insertMessage(state, key, msg) {
  const chat = state.chats[key]
  if (!chat) return false
  const idx = chat.msgs.findIndex((m) => m.dir === msg.dir && m.pkt === msg.pkt)
  if (idx >= 0) {
    const old = chat.msgs[idx]
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
    chat.msgs.splice(idx, 1, msg)
  } else {
    chat.msgs.push(msg)
  }
  if (msg.ts) state.lastTs[key] = Math.max(state.lastTs[key] || 0, msg.ts)
  if (msg.peer) state.peerMeta[key] = msg.peer
  // 实时入列是另一条路径：回放此前暂存的下载进度（含已完成）
  replayFileProgress(state.pendingFileProgress, chat, key)
  return true
}

/**
 * 把一个会话标成已读：未读计数清零 + 乐观置位 + 返回该回执的包号。
 * **回执本身（IPC）由调用方发** —— 「算什么」与「发什么」分开，顺序才可断言。
 * @returns {number[]} 需要回执的包号（无可回执时为 []）
 */
export function markChatRead(state, key) {
  if (!key) return []
  // 未读数先清零：哪怕这个会话里没有需要回执的消息，提示也该消失
  if (state.unread[key]) delete state.unread[key]
  delete state.unreadTs[key]

  const chat = state.chats[key]
  if (!chat) return []
  const unread = chat.msgs.filter((m) => m.dir === 'in' && !m.read)
  if (!unread.length) return []
  const pkts = [...new Set(unread.map((m) => m.pkt))]
  for (const m of unread) m.read = true // 乐观置位，避免连发时重复回执
  return pkts
}

/** 会话可见（打开着且窗口聚焦）→ 收到的消息视为已读，不必进未读 */
export function isChatVisible(state, key) {
  return state.activeKey === key && state.windowFocused
}

/* ---------------- 事件表 ---------------- */

/** 该会话里最近有未读的那个 key；没有未读就退到最近活动的会话 */
export function latestUnreadKey(state) {
  return pickLatestUnread(state.unread, state.unreadTs, state.lastTs)
    || pickLatestActive(state.lastTs, state.unreadTs)
}

/**
 * 后端事件入口：就地改 state，返回按顺序执行的 effect 清单。
 * 事件名与 `ipc.EVT` 的取值一致。
 */
export function applyEvent(state, event, payload) {
  switch (event) {
    case 'msg-in': return onMsgIn(state, payload)
    case 'msg-out': return onMsgOut(state, payload)
    case 'msg-read': return onMsgRead(state, payload)
    case 'msg-delivered': return onMsgDelivered(state, payload)
    case 'file-progress': return onFileProgress(state, payload)
    case 'msg-recalled': return onMsgRecalled(state, payload)
    case 'msg-unlocked': return onMsgUnlocked(state, payload)
    case 'absence-info': return onAbsenceInfo(state, payload)
    case 'users-updated': return { effects: [{ type: 'reload_people' }] }
    case 'open-unread':
      return { effects: [{ type: 'open_chat', key: latestUnreadKey(state), logLabel: 'open-unread failed' }] }
    case 'open-chat':
      // 没有 key 就什么都不做（与旧的「if (!key) return」一致：连窗口动作都不做）
      return payload?.key
        ? { effects: [{ type: 'open_chat', key: payload.key, logLabel: 'open-chat failed' }] }
        : { effects: [] }
    case 'window-focus': return onWindowFocus(state)
    case 'window-blur': return onWindowBlur(state)
    default: return { effects: [] }
  }
}

/* ---------------- 各事件的迁移 ---------------- */

function onMsgIn(state, { key, msg, resend } = {}) {
  if (!key || !msg) return { effects: [] }
  // 对端「延迟发送」会以相同包号反复重发：不算新消息，不重复计未读、不重复通知
  const isRebroadcast = resend
    || (state.chats[key]?.msgs || []).some((m) => m.dir === 'in' && m.pkt === msg.pkt)
  insertMessage(state, key, msg)

  const effects = []
  // 「对方回话即视为已读」兜底：飞秋等实现不对带 READCHECKOPT 的消息回 READMSG，
  // 未读标记会永远挂着；对端既然发来了新消息，人就在对话里，把此前要求回执且未读的
  // 出站消息翻成已读（本地翻转 + 落库，不发包）。
  if (msg.dir === 'in' && !isRebroadcast) {
    const pkts = unreadReceiptPkts(state.chats[key]?.msgs)
    if (pkts.length) {
      for (const m of state.chats[key].msgs) {
        if (pkts.includes(m.pkt)) m.read = true
      }
      effects.push({ type: 'mark_out_read', key, pkts })
    }
  }
  if (isChatVisible(state, key)) {
    const pkts = markChatRead(state, key)
    if (pkts.length) effects.push({ type: 'mark_read', key, pkts })
  } else if (!isRebroadcast) {
    state.unread[key] = (state.unread[key] || 0) + 1
    state.unreadTs[key] = msg.ts || Math.floor(Date.now() / 1000)
    effects.push({ type: 'notify', key, msg })
  }
  return { effects }
}

function onMsgOut(state, { key, msg } = {}) {
  if (!key || !msg) return { effects: [] }
  insertMessage(state, key, msg)
  return { effects: [] }
}

function onMsgRead(state, { key, pkt } = {}) {
  // 对端回来的 READMSG：被读的是「我方发出」的那条
  const msg = state.chats[key]?.msgs.find((m) => m.pkt === pkt && m.dir === 'out')
  if (msg) {
    msg.read = true
    // 已读蕴含已送达：两条记录状态不自相矛盾（同后端 mark_out_read）
    msg.delivered = true
  }
  return { effects: [] }
}

function onMsgDelivered(state, { key, pkt } = {}) {
  // 送达确认（RECVMSG）：官方 Windows 客户端对普通消息只回这个、从不回 READMSG，
  // 没有这一档气泡会一直挂在「未读」。已读是更强的状态，不反向覆盖 read。
  const msg = state.chats[key]?.msgs.find((m) => m.pkt === pkt && m.dir === 'out')
  if (msg) msg.delivered = true
  return { effects: [] }
}

function onFileProgress(state, p) {
  if (!p?.key) return { effects: [] }
  // 消息还没入列（会话未加载时 msg-in 要先 await ensureChat）时不能直接丢：
  // 那条「下载完成」丢了，卡片就永远停在「下载中」。暂存，入列后回放。
  if (!applyFileProgress(state.chats[p.key], p)) {
    bufferFileProgress(state.pendingFileProgress, p)
  }
  return { effects: [] }
}

function onMsgRecalled(state, { key, pkt } = {}) {
  const msg = state.chats[key]?.msgs.find((m) => m.pkt === pkt)
  if (msg) msg.recalled = true
  delete state.unread[key]
  delete state.unreadTs[key]
  return { effects: [] }
}

function onMsgUnlocked(state, { key, pkt } = {}) {
  const msg = state.chats[key]?.msgs.find((m) => m.pkt === pkt)
  if (msg) {
    msg.unlocked = true
    msg.locked = false
    msg.read = true
  }
  return { effects: [] }
}

function onAbsenceInfo(state, { key, text } = {}) {
  if (state.userMap[key]) state.userMap[key].absence_text = text
  return { effects: [] }
}

function onWindowFocus(state) {
  state.windowFocused = true
  if (!state.activeKey) return { effects: [] }
  const pkts = markChatRead(state, state.activeKey)
  return { effects: pkts.length ? [{ type: 'mark_read', key: state.activeKey, pkts }] : [] }
}

function onWindowBlur(state) {
  state.windowFocused = false
  return { effects: [] }
}
