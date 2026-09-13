/**
 * 封书（SECRETOPT）/ 密码锁（PASSWORDOPT）在消息流里的展示判定。
 *
 * 「未开封的信封」是**收件人视角**的概念：对端发来的封书/密码消息在开封前
 * 只显示占位文案；自己发出的消息从来没有这一说 —— 正文就是我写的，官方
 * IPMsg 的寄件方窗口也是直接显示正文。
 *
 * 记录字段（见后端 net.rs 落库）：
 * - `dir`      'in' | 'out'
 * - `secret`   本次是否以封书（SECRETEXOPT）发出
 * - `locked`   是否密码锁且当前仍未开封
 * - `unlocked` 是否已开封。出站记录**没有这个字段**（旧记录同样如此），
 *              曾经的判定漏掉方向、把 `secret && !unlocked` 当成信封，
 *              于是自己的封书消息显示成「对方发来的保密消息」，点开封
 *              又因后端只查入站记录而报「找不到该消息」。
 */

/** 这条消息是否要显示成「未开封的信封」（只可能是收件人侧） */
export function isSealedEnvelope(m) {
  if (!m || m.dir !== 'in') return false
  return envelopeOf(m)
}

/** 信封是否属于密码锁（决定占位文案走密码还是封书） */
export function isPasswordEnvelope(m) {
  if (!m || m.dir !== 'in') return false
  return Boolean(m.locked) && m.unlocked !== true
}

/** 是否在时间行标出「封书」（收发两端都标：封书是这条消息的属性） */
export function showsSealTag(m) {
  return Boolean(m && m.secret === true)
}

/**
 * 未开封的信封在「本来会显示正文」的地方（系统通知预览等）该用的文案键；
 * 不是信封或已经开封时返回 null，调用方照常显示正文。
 *
 * 只看记录字段的纯函数，所以通知预览这种拿不到组件状态的地方也能用；
 * 气泡本身用 [`isSealedEnvelope`] 直接挡渲染。
 */
export function sealedPreviewKey(m) {
  if (!isSealedEnvelope(m)) return null
  return isPasswordEnvelope(m) ? 'preview.locked' : 'preview.sealed'
}

/** 收件人侧未开封：密码锁（locked）或未开封的封书 */
function envelopeOf(m) {
  if (Boolean(m.locked) && m.unlocked !== true) return true
  return m.secret === true && m.unlocked !== true
}
