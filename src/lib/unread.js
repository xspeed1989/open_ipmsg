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
