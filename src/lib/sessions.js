/**
 * 「广播」信箱的固定会话 key（与后端 `state::BROADCAST_SESSION_KEY` 一致）。
 * 它不对应任何对端：收到的广播与本地发出的广播都归到这一个常驻条目里。
 */
export const BROADCAST_SESSION_KEY = '255.255.255.255'

/** 该会话 key 是否是广播信箱（伪 key，不是合法对端地址） */
export const isBroadcastSession = (key) => key === BROADCAST_SESSION_KEY

/** 清掉后端可能带进来的控制字符，避免污染界面 */
function clean(s) {
  return typeof s === 'string' ? s.replace(/[\u0000-\u001f\u007f]/g, '') : ''
}

/**
 * 广播信箱里某条消息的发送方名。
 *
 * 该会话没有"对端"，不能用会话 key 取名，只能看每条记录自己的 peer 快照：
 * 自己发的算「我」，别人发的用它的昵称（退化为用户名/IP）。昵称文案由调用方
 * 注入，便于单测与多语言。
 *
 * @param {{dir?:string, peer?:{nickname?:string, user?:string, ip?:string, key?:string}}} m
 * @param {{me:string, unknown:string}} texts
 */
export function senderOf(m, texts) {
  if (!m) return ''
  if (m.dir === 'out') return texts.me
  return (
    clean(m.peer?.nickname) ||
    clean(m.peer?.user) ||
    senderIp(m) ||
    texts.unknown
  )
}

/**
 * 发送方 IP：同一昵称可能有多台机器，气泡标签旁边给出 IP 便于区分。
 * 旧记录没有发送方快照（`peer.key` 是伪造的广播地址）时返回空串，宁可不显示。
 */
export function senderIp(m) {
  if (!m) return ''
  const ip = clean(m.peer?.ip)
  if (ip) return ip
  const key = clean(m.peer?.key)
  return key && !isBroadcastSession(key) ? key : ''
}

/**
 * 中栏联系人列表的数据合并：在线用户 ∪ 离线历史会话。
 * 同一 key 同时在线又有历史时只保留在线条目（在线优先）。
 * 置顶条目（`pinned`，广播信箱）永远排在最前，不参与在线优先的顺序。
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
  // 稳定的两段划分：置顶条目保持后端给的相对顺序，其余保持在线优先的顺序
  const pinned = merged.filter((s) => s.pinned)
  return pinned.length ? [...pinned, ...merged.filter((s) => !s.pinned)] : merged
}