/**
 * 中栏联系人列表的数据合并：在线用户 ∪ 离线历史会话。
 * 同一 key 同时在线又有历史时只保留在线条目（在线优先）。
 *
 * @param {Array} online  在线用户（get_users 结果）
 * @param {Array} offline 历史会话（list_sessions 结果）
 * @returns {Array<{key:string, online:boolean, [k:string]:any}>}
 */
export function mergeSessions(online, offline) {
  const onlineList = Array.isArray(online) ? online : []
  const offlineList = Array.isArray(offline) ? offline : []
  const onlineKeys = new Set(onlineList.map((u) => u && u.key).filter(Boolean))
  const merged = onlineList.map((u) => ({ ...u, online: true }))
  for (const s of offlineList) {
    if (!s || !s.key || onlineKeys.has(s.key)) continue
    merged.push({ ...s, online: false })
  }
  return merged
}