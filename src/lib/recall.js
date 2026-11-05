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

/**
 * 已撤回的气泡上是否显示「重新编辑」。
 *
 * 撤回范围放宽到附件之后，这一条变得必要：附件消息撤回后没有正文可恢复，
 * 按钮若照旧按 dir === 'out' 出现，就是个点了没反应的死按钮。
 * 判据与 recalledEditState 同源，避免两处各写一遍 kind === 'text'。
 */
export function canReedit(message) {
  return recalledEditState(message) !== null
}

/**
 * 右键菜单里是否显示「撤回」。
 *
 * 能撤回的是**我方发出的、还没撤回的**任何消息：文本、文件、图片、剪贴板贴图、
 * 表情、文件夹在后端落库的 kind 都是 'text' 或 'file'，撤回语义与文字消息相同
 * （附件消息会额外撤销本机的附件槽并发 RELEASEFILES，见 net.rs recall_message）。
 *
 * 两种例外：
 * - 广播信箱（`255.255.255.255`）一次发给多人，DELMSG 表达不了，后端也会拒绝；
 *   这里直接不给入口，而不是让用户点了再看报错。
 * - 已经撤回的消息没有第二次可撤回的东西。
 *
 * kind 缺失的老记录按文本处理：后端只认 `kind === 'file'` 才做撤槽那一套，
 * 缺字段就当非附件，两边判断一致。
 *
 * @param {{dir?:string, recalled?:boolean, kind?:string}|null|undefined} message
 * @param {{broadcast?:boolean}} [opts] broadcast：当前会话是不是广播信箱
 */
export function canRecall(message, opts = {}) {
  if (!message) return false
  if (message.dir !== 'out') return false
  if (message.recalled === true) return false
  if (opts.broadcast === true) return false
  return true
}
