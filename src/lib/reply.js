/**
 * 右键回复的消息引用：把「被引用的原文」变成发送正文的一部分。
 *
 * 引用随消息作为普通文本发送（IPMsg 协议没有引用字段）：
 * 对方（含飞秋等老客户端）收到的就是「「原文」\n回复内容」这样的文本，
 * 零协议改动、互操作最稳。本地 UI 用 quotePreview 生成摘要条。
 */

/** 把消息内容压成单行摘要（空白压实），用于引用条显示 */
function collapse(s) {
  return String(s || '').replace(/\s+/g, ' ').trim()
}

/**
 * 生成消息的引用摘要。
 * 文本消息取正文；文件/图片消息显示「[文件] 文件名」。
 * 超长截断为单行（最长约 30 字 + 省略号）。
 *
 * @param {{kind?:string, text?:string, files?:{name?:string}[]}|null|undefined} msg
 * @returns {string}
 */
export function quotePreview(msg) {
  if (!msg) return ''
  if (msg.kind === 'file') {
    const name = msg.files?.[0]?.name
    return name ? `[文件] ${collapse(name)}` : '[文件]'
  }
  const text = collapse(msg.text)
  if (!text) return ''
  if (text.length > 30) return text.slice(0, 30) + '…'
  return text
}

/**
 * 组装发送正文：「{引用原文}」\n{回复内容}。
 * 原文不存在时只返回回复内容本身。
 *
 * @param {string|undefined} quoteText 被引用的原文摘要
 * @param {string} replyText 回复内容
 * @returns {string}
 */
export function composeReplyBody(quoteText, replyText) {
  const q = collapse(quoteText)
  const body = replyText.replace(/\n{3,}/g, '\n\n').trimEnd()
  if (!q) return body
  return `「${q}」\n${body}`
}