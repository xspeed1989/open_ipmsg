/**
 * 文件下载进度事件的落点与暂存（接收方向）。
 *
 * 为什么需要暂存：后端收下附件后**先**发 msg-in（记录里 state=downloading），
 * **随即**自动下载，完成后发 file-progress(done=true, path=...)。前端 msg-in
 * 处理是 async —— 该会话还没加载时得先 `await getHistory()` 才把消息插进
 * `msgs`；file-progress 落在这个窗口里时消息还不在内存中，若直接丢弃，这条
 * 附件的「下载完成」就永远丢了：卡片停在「下载中」，图片出不来，直到重启
 * （重读历史）才恢复。真机表现为「对端发的剪贴板图片收不到，再发一次就好」。
 *
 * 所以事件落不下去时按 key/pkt/file_id 暂存，等消息真正入列（ensureChat 的
 * 历史加载 / pushMsg 的实时入列）后回放。回放是幂等的：命中即移除。
 */

/** 一条进度事件唯一标识一条附件的暂存键 */
export function progressKey(key, pkt, fileId) {
  return `${key}\u0000${pkt}\u0000${fileId}`
}

/**
 * 把一条 file-progress 落到已入列的消息上。
 * 消息或附件还没进内存时返回 false —— 调用方应先暂存，等入列后回放。
 *
 * @param {{msgs?: Array<object>}} chat 该会话的 store 条目
 * @param {object} p 后端 file-progress 事件负载
 * @returns {boolean} 是否真的写进去了
 */
export function applyFileProgress(chat, p) {
  const msg = (chat?.msgs || []).find((m) => m.pkt === p.pkt && m.dir === 'in')
  if (!msg) return false
  const f = (msg.files || []).find((x) => x.id === p.file_id)
  if (!f) return false
  f.transferred = p.transferred
  f.total = p.total || f.size
  // 加密流下载标记：UI 据此区分「下载中（加密）」与「解密完成」状态
  if (typeof p.enc === 'boolean') f.enc = p.enc
  if (p.error) {
    f.state = 'failed'
    f.error = p.error
  } else if (p.done) {
    f.state = 'done'
    f.path = p.path || f.path
  }
  return true
}

/** 暂存窗口上限：对端撤销/过期而永远等不到消息的事件不能无限堆积 */
export const PROGRESS_BUFFER_MAX = 256

/**
 * 暂存一条落不下去的进度事件。同一附件只保留最新一条（后续进度覆盖前面）。
 */
export function bufferFileProgress(buffer, p) {
  const k = progressKey(p.key, p.pkt, p.file_id)
  buffer.delete(k) // 先删再插：让「最新」同时也是 LRU 顺序里的最后一个
  buffer.set(k, p)
  while (buffer.size > PROGRESS_BUFFER_MAX) {
    buffer.delete(buffer.keys().next().value)
  }
}

/**
 * 消息入列后回放该会话的暂存事件，返回命中条数。
 * 命中即从暂存移除；不匹配（别的会话 / 该附件还没入列）的原样留着。
 */
export function replayFileProgress(buffer, chat, key) {
  if (!buffer.size) return 0
  let hits = 0
  for (const [k, p] of [...buffer]) {
    if (p.key !== key) continue
    if (applyFileProgress(chat, p)) {
      buffer.delete(k)
      hits += 1
    }
  }
  return hits
}
