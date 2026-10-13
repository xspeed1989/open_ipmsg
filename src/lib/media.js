/**
 * 聊天里图片/视频的显示尺寸（纯函数，便于 node --test 直接测）。
 *
 * 为什么需要「算出来再写进元素属性」，而不是只靠 CSS 的 max-width/max-height：
 * 气泡是**收缩宽度**（shrink-to-fit），它的宽度由内容的 `max-content` 决定，而图片的
 * `max-content` 是按**原始像素**参与计算的 —— 表情图常见 1506×1216 这种尺寸。只写
 * `max-width: min(260px, 100%)`（含百分比 → 对内在尺寸计算是不定值）和 `max-height`
 * 时，布局按 1506 撑、显示按 248 缩，于是气泡被撑到 `.bubble-wrap` 的 62% 上限，
 * 图片右边空出一大块底色（真机反馈）。
 *
 * 把可用尺寸算准、写进 `width`/`height` 属性后，布局与显示用同一个数，气泡就精确贴合。
 */

/** 气泡内媒体框（最长边上限），与 ChatWindow 的 .chat-img 保持一致 */
export const MEDIA_BOX_W = 260
export const MEDIA_BOX_H = 200

/**
 * 等比例缩放到媒体框内（只缩不放：小图保持原尺寸，不放大糊掉）。
 * 返回 null 表示尺寸未知（还没加载出来 / 拿不到 naturalWidth），此时交给 CSS 兜底。
 */
export function fitMedia(natW, natH) {
  const w = Number(natW)
  const h = Number(natH)
  if (!Number.isFinite(w) || !Number.isFinite(h) || w <= 0 || h <= 0) return null
  const k = Math.min(1, MEDIA_BOX_W / w, MEDIA_BOX_H / h)
  return {
    w: Math.max(1, Math.round(w * k)),
    h: Math.max(1, Math.round(h * k)),
  }
}
