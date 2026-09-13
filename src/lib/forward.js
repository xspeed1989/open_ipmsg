/**
 * 消息转发：判定一条消息能否转发，并整理成可发送的载荷。
 *
 * 文本消息原样转发正文；文件/图片消息需要每个文件都有本地路径
 * （自己发过的 / 已下载完成的）才能转发，未下载的提示原因。
 * 提示文案与拼装标记按当前界面语言取（见 lib/i18n）。
 */

import { t } from './i18n.js'

/**
 * @param {{kind?:string, text?:string, files?:{name?:string, path?:string}[]}|null|undefined} msg
 * @returns {{ok:true, kind:'text', text:string} |
 *           {ok:true, kind:'files', paths:string[]} |
 *           {ok:false, reason:string}}
 */
export function forwardPayload(msg) {
  if (!msg) return { ok: false, reason: t('forward.noContent') }
  if (msg.kind === 'file') {
    const files = msg.files || []
    const missing = files.filter((f) => !f.path)
    if (files.length === 0) return { ok: false, reason: t('forward.noContent') }
    if (missing.length) {
      return {
        ok: false,
        reason: t('forward.notDownloaded', { name: missing[0].name || t('forward.fileName') }),
      }
    }
    return { ok: true, kind: 'files', paths: files.map((f) => f.path) }
  }
  const text = (msg.text || '').trim()
  if (!text) return { ok: false, reason: t('forward.noContent') }
  return { ok: true, kind: 'text', text }
}

/**
 * 多选合并转发：把选中的多条消息拼成一条文本（微信式，每行带发送者前缀）。
 *
 * 按时间升序排列；附件不传内容，只在行尾追加 `[附件] 名字, 名字` 占位；
 * 单条消息正文与附件都为空时跳过该行；一行都拼不出来时返回 null，
 * 由调用方提示「没有可转发的内容」。
 *
 * @param {{dir?:string, ts?:number, text?:string, files?:{name?:string}[]}[]} msgs
 * @param {(dir?: string, msg?: object) => string} nickOf 由方向/消息取显示昵称
 *        （'我' / 对方昵称；广播会话里每条消息的发送方各不相同，故回传整条消息）
 * @returns {string|null}
 */
export function mergeForward(msgs, nickOf) {
  const sorted = [...(msgs || [])].sort((a, b) => (a.ts || 0) - (b.ts || 0))
  const lines = []
  for (const m of sorted) {
    const body = (m.text || '').trim()
    const att = (m.files || [])
      .map((f) => f.name || '')
      .filter(Boolean)
      .join(', ')
    const content = body + (att ? (body ? ' ' : '') + `${t('attach.tag')} ` + att : '')
    if (!content) continue
    lines.push(`${t('merge.qOpen')}${nickOf(m.dir, m)}${t('merge.qClose')}${content}`)
  }
  return lines.length ? lines.join('\n') : null
}