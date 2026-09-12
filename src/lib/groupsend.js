// 群发扇出：输入框文本 + 待发送附件 → 多个会话（原「批量发送 / 多选群发」合并后的共用逻辑）。
// 一律逐个会话单发，各自保留已读回执；不使用官方 MULTICASTOPT 同文多投
// （该发送路径已移除，接收侧仍按官方语义兼容，见 docs/ipmsg-protocol-gap.md）。

/** 群发载荷：有附件就走文件（路径只落盘一次，多收件人复用），否则纯文本。
 * 文本统一压掉连续空行并去掉尾部空白，避免把整段排版空白发出去。 */
export function groupPayload(text, paths = []) {
  const body = String(text ?? '').replace(/\n{3,}/g, '\n\n').trimEnd()
  return paths?.length
    ? { kind: 'files', paths: [...paths], text: body }
    : { kind: 'text', text: body }
}

/** 逐个会话单发并聚合结果：单个收件人失败不影响其余，
 * 返回成功数 ok 与失败收件人 key 列表 fails（调用方据此提示「成功 N 人；失败 M 人」）。 */
export async function sendToEach(keys, sendOne) {
  const fails = []
  let ok = 0
  for (const key of keys) {
    try {
      await sendOne(key)
      ok += 1
    } catch {
      fails.push(key)
    }
  }
  return { ok, fails }
}
