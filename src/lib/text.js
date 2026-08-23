/**
 * 拆出对端客户端追加的「延迟发送」尾注。
 *
 * IP Messenger / 飞秋 给不在线的用户发消息时，会先在发送方排队，等对方上线
 * 再补投，补投时在正文后自动追加分隔线与原始发送时间：
 *
 *   123
 *   ----
 *   (IPMsg Delayed Send: 08/22 15:02 )
 *
 * 这两行是对方客户端加的元信息，不是用户输入的内容，界面上单独当标记展示。
 *
 * @returns {{body: string, delayed: string|null}}
 *   body 为原始正文；delayed 为尾注里的原始发送时间，没有尾注时为 null
 *   （注意：有尾注但时间为空时是空串，与 null 语义不同）
 */
export function splitDelayedNote(text) {
  const t = text || ''
  const m = t.match(
    /\n?[ \t]*-{2,}[ \t]*\n?[ \t]*\(\s*IPMsg\s+Delayed\s+Send\s*:\s*([^)]*?)\s*\)\s*$/i
  )
  if (!m) return { body: t, delayed: null }
  return { body: t.slice(0, m.index).replace(/\s+$/, ''), delayed: m[1] || '' }
}

/**
 * 从剪贴板的 text/uri-list（或纯文本）里解析出本地文件路径。
 *
 * 文件管理器复制文件时，剪贴板里带的是 `file:///path` 形式的 URI；
 * 只有当所有非空行都是 file:// URI 时才认定为"复制了文件"，
 * 避免把用户粘贴的普通文本误当成文件路径。
 *
 * @returns {string[]} 解码后的绝对路径；不是文件列表时返回空数组
 */
export function parseFileUris(text) {
  const lines = (text || '')
    .split(/\r?\n/)
    .map((l) => l.trim())
    // GNOME 的 x-special/gnome-copied-files 首行是 copy/cut 动作名
    .filter((l) => l && l !== 'copy' && l !== 'cut' && !l.startsWith('#'))
  if (!lines.length || !lines.every((l) => /^file:\/\//i.test(l))) return []
  return lines
    .map((l) => {
      let p = l.replace(/^file:\/\/(localhost)?/i, '')
      try {
        p = decodeURIComponent(p)
      } catch {
        /* 非法转义时用原文 */
      }
      // Windows 的 file:///C:/x → /C:/x，需要去掉前导斜杠
      return /^\/[a-zA-Z]:/.test(p) ? p.slice(1) : p
    })
    .filter(Boolean)
}

/**
 * 把文本按关键词切成 [{text, hit}] 片段，供高亮渲染。
 * 大小写不敏感；关键词为空时原样返回单个片段。
 */
export function highlightParts(text, query) {
  const t = text ?? ''
  const q = (query || '').trim()
  if (!q) return [{ text: t, hit: false }]
  const lower = t.toLowerCase()
  const needle = q.toLowerCase()
  const out = []
  let i = 0
  for (;;) {
    const at = lower.indexOf(needle, i)
    if (at < 0) break
    if (at > i) out.push({ text: t.slice(i, at), hit: false })
    out.push({ text: t.slice(at, at + needle.length), hit: true })
    i = at + needle.length
  }
  if (i < t.length) out.push({ text: t.slice(i), hit: false })
  return out.length ? out : [{ text: t, hit: false }]
}

/**
 * 搜索结果里的一行摘要：截取命中位置前后各 radius 个字符，
 * 两端超出时用省略号，命中不到就退化成开头一段。
 */
export function makeSnippet(text, query, radius = 12) {
  const t = (text || '').replace(/\s+/g, ' ').trim()
  const q = (query || '').trim().toLowerCase()
  if (!q) return t.slice(0, radius * 2 + q.length)
  const at = t.toLowerCase().indexOf(q)
  if (at < 0) return t.slice(0, radius * 2)
  const start = Math.max(0, at - radius)
  const end = Math.min(t.length, at + q.length + radius)
  return (start > 0 ? '…' : '') + t.slice(start, end) + (end < t.length ? '…' : '')
}
