/**
 * 从未读表里挑出「最近收到消息的那个会话」。
 *
 * 从托盘唤起主窗口时用它决定跳到哪个会话：未读数为 0 的会话不参与，
 * 时间戳缺失时退化用该会话的最后消息时间，仍然缺失按 0 处理。
 * 时间戳相同则取先遍历到的（插入顺序），保证结果稳定。
 *
 * @param {Record<string, number>} unread   key -> 未读数
 * @param {Record<string, number>} unreadTs key -> 最近一条未读消息的时间戳
 * @param {Record<string, number>} lastTs   key -> 会话最后一条消息的时间戳
 * @returns {string} 会话 key；没有未读时返回空串
 */
export function pickLatestUnread(unread = {}, unreadTs = {}, lastTs = {}) {
  let best = ''
  let bestTs = -1
  for (const [key, n] of Object.entries(unread)) {
    if (!n) continue
    const ts = unreadTs[key] ?? lastTs[key] ?? 0
    if (ts > bestTs) {
      bestTs = ts
      best = key
    }
  }
  return best
}

/**
 * 挑出「最近有消息活动」的会话（不限未读）：托盘双击时若没有未读，
 * 就退到它——保证双击托盘总能切到一个会话，而不是什么都不发生。
 * @returns {string} 会话 key；完全没有历史时返回空串
 */
export function pickLatestActive(lastTs = {}, unreadTs = {}) {
  let best = ''
  let bestTs = -1
  const keys = new Set([...Object.keys(lastTs), ...Object.keys(unreadTs)])
  for (const key of keys) {
    const ts = lastTs[key] ?? unreadTs[key] ?? 0
    if (ts > bestTs) {
      bestTs = ts
      best = key
    }
  }
  return best
}

/**
 * 启动补数：把历史会话摘要里的未读计数并入未读表。
 *
 * WebView 监听就绪前到达的 msg-in 事件会被丢弃——对端离线留言在我方上线
 * 瞬间重投（首投即落库、事件却没人接）就属此列，之后的同包号重投又会被
 * 后端去重标记成 resend，前端因此永远收不到未读增量，红点与托盘闪烁缺失。
 * 会话摘要的 unread 是后端持久化 read 标志的投影，用它把遗漏补回来：
 * - 聊天已加载过的会话跳过：内存里已有这些消息（含乐观已读/实时计数）；
 * - 未读表里已有计数的会话跳过：实时事件已经记过账，避免重复；
 * 因此本函数只增不覆盖，多次调用（启动 + users-updated 刷新）都安全。
 *
 * @param {Record<string, number>} unread   key -> 未读数（原地修改）
 * @param {Record<string, number>} unreadTs key -> 最近未读消息时间戳（原地修改）
 * @param {Record<string, object>} chats    已加载的聊天表（key -> chat）
 * @param {Array<object>} sessions list_sessions 返回的会话摘要
 */
export function applyUnreadFromSessions(unread, unreadTs, chats, sessions) {
  for (const s of sessions || []) {
    if (!s || !s.key || !s.unread) continue
    if (chats?.[s.key]) continue
    if (!unread[s.key]) {
      unread[s.key] = s.unread
      unreadTs[s.key] = s.unread_ts || 0
    }
  }
}
