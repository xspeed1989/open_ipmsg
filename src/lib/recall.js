/**
 * 为“重新编辑”生成输入区状态。
 * 只有自己已撤回的文本消息可恢复；正文保持原样，不做 trim 或换行归一化。
 */
export function recalledEditState(message) {
  if (
    message?.dir !== 'out' ||
    message?.kind !== 'text' ||
    message?.recalled !== true
  ) return null

  return {
    draft: typeof message.text === 'string' ? message.text : '',
    replyTarget: null,
  }
}
