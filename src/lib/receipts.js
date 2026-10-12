/**
 * 挑出会话里「要求已读回执但还没被对端确认」的出站消息包号。
 *
 * 用于「对方回话即视为已读」的兜底：飞秋等实现不会对我们带
 * IPMSG_READCHECKOPT 的消息回 READMSG，未读标记会永远挂着；
 * 对端一旦发来新消息，说明人就在对话里，把这些旧出站消息翻成已读。
 *
 * 只认 rcpt === true 的记录：文件消息从不请求回执，历史里的老记录
 * 缺 rcpt 字段时也不参与，避免误标。
 *
 * @param {Array<{dir?:string, pkt?:number, rcpt?:boolean, read?:boolean}>} [msgs]
 * @returns {number[]}
 */
export function unreadReceiptPkts(msgs) {
  if (!Array.isArray(msgs)) return []
  const out = []
  for (const m of msgs) {
    if (m && m.dir === 'out' && m.rcpt === true && m.read !== true) {
      if (typeof m.pkt === 'number') out.push(m.pkt)
    }
  }
  return out
}

/**
 * 出站气泡的送达状态：'read'（已读）/ 'delivered'（已送达）/ 'unread'（未读）。
 * 不显示状态标签时返回 null。
 *
 * 为什么要有「已送达」这个中间态：官方 Windows 客户端对**普通消息**只回
 * RECVMSG（送达确认）、从不回 READMSG（已读通知）——它的 READMSG 只在封书
 * 被点「打开」时才发（官方 recvdlg.cpp）。只认 READMSG 的话，给原生客户端
 * 发消息就永远显示「未读」，只能等对方回话走兜底，用户看到的就是
 * 「本端无法及时已读」。所以把送达确认落成独立的一档。
 *
 * 已读是更强的状态：read=true 时即便 delivered 缺失（READMSG 先到、老记录）
 * 也一律显示「已读」，不能倒退。
 *
 * @param {{dir?:string, rcpt?:boolean, read?:boolean, delivered?:boolean, queued?:boolean}} [m]
 * @returns {'read'|'delivered'|'unread'|null} 同时用作 i18n 后缀与 CSS 类名
 */
export function deliveryState(m) {
  if (!m || m.dir !== 'out' || m.rcpt !== true || m.queued) return null
  if (m.read === true) return 'read'
  if (m.delivered === true) return 'delivered'
  return 'unread'
}
