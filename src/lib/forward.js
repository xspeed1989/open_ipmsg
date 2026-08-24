/**
 * 消息转发：判定一条消息能否转发，并整理成可发送的载荷。
 *
 * 文本消息原样转发正文；文件/图片消息需要每个文件都有本地路径
 * （自己发过的 / 已下载完成的）才能转发，未下载的提示原因。
 */

/**
 * @param {{kind?:string, text?:string, files?:{name?:string, path?:string}[]}|null|undefined} msg
 * @returns {{ok:true, kind:'text', text:string} |
 *           {ok:true, kind:'files', paths:string[]} |
 *           {ok:false, reason:string}}
 */
export function forwardPayload(msg) {
  if (!msg) return { ok: false, reason: '没有可转发的内容' }
  if (msg.kind === 'file') {
    const files = msg.files || []
    const missing = files.filter((f) => !f.path)
    if (files.length === 0) return { ok: false, reason: '没有可转发的内容' }
    if (missing.length) {
      return {
        ok: false,
        reason: `「${missing[0].name || '文件'}」尚未下载，无法转发`,
      }
    }
    return { ok: true, kind: 'files', paths: files.map((f) => f.path) }
  }
  const text = (msg.text || '').trim()
  if (!text) return { ok: false, reason: '没有可转发的内容' }
  return { ok: true, kind: 'text', text }
}