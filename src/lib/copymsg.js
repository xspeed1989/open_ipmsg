/**
 * 右键「复制消息内容」：把一条历史消息整理成要放进剪贴板的文本。
 *
 * 文本消息取正文；文件消息带正文时取正文（附件名单独复制意义不大），
 * 没有正文时逐行列出附件名；两者皆无返回空串，由调用方提示不可复制。
 */

/**
 * @param {{kind?:string, text?:string, files?:{name?:string}[]}|null|undefined} msg
 * @returns {string} 空串表示没有可复制的内容
 */
export function copyTextOf(msg) {
  if (!msg) return ''
  const text = (msg.text || '').trim()
  if (text) return text
  const names = (msg.files || []).map((f) => f.name || '').filter(Boolean)
  return names.join('\n')
}
