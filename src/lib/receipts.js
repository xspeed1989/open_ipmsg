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
