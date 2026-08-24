/**
 * 计算弹出面板的 fixed 定位，使其「跟随」触发按钮：
 * 默认贴在锚点正上方、左缘对齐；上方空间不足翻转到下方；
 * 左右越界时夹回视口内。
 *
 * @param {{left:number, top:number, bottom:number, width:number}} anchor
 *        触发按钮的 getBoundingClientRect()
 * @param {number} panelW  面板宽度
 * @param {number} panelH  面板高度（估估值即可，仅用于上下翻转判断）
 * @param {number} viewportW 视口宽
 * @param {number} viewportH 视口高
 * @param {number} [margin=8] 距视口边缘的最小间距
 * @param {number} [gap=8] 面板与锚点的间距
 * @returns {{left:number, top:number}}
 */
export function computePopupPosition(
  anchor,
  panelW,
  panelH,
  viewportW,
  viewportH,
  margin = 8,
  gap = 8,
) {
  const maxLeft = Math.max(margin, viewportW - margin - panelW)
  const left = Math.min(Math.max(anchor.left, margin), maxLeft)

  let top = anchor.top - panelH - gap
  if (top < margin) {
    top = anchor.bottom + gap
  }
  return { left, top }
}
