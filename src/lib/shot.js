/**
 * 截图遮罩的几何纯函数（无 DOM、无 Tauri 依赖，node --test 直接跑）。
 * 坐标系：一律 CSS 像素；与整幅抓屏图像的换算只发生在 cssRectToImageRect。
 */

/** 拖拽两点 → 归一化矩形，并夹取到 bounds 内 */
export function rectFromDrag(a, b, bounds) {
  return clampRect(
    {
      x: Math.min(a.x, b.x),
      y: Math.min(a.y, b.y),
      w: Math.abs(a.x - b.x),
      h: Math.abs(a.y - b.y),
    },
    bounds,
  )
}

/** 把矩形夹进 bounds：先夹左上再夹右下，宽高不为负 */
export function clampRect(r, b) {
  const x1 = Math.min(Math.max(r.x, b.x), b.x + b.w)
  const y1 = Math.min(Math.max(r.y, b.y), b.y + b.h)
  const x2 = Math.min(Math.max(r.x + r.w, b.x), b.x + b.w)
  const y2 = Math.min(Math.max(r.y + r.h, b.y), b.y + b.h)
  return { x: x1, y: y1, w: Math.max(0, x2 - x1), h: Math.max(0, y2 - y1) }
}

/** 选区是否达到可确认的最小尺寸（默认 3×3） */
export function canConfirm(r, min = 3) {
  return !!r && r.w >= min && r.h >= min
}

const CORNERS = ['nw', 'ne', 'se', 'sw']

/** 命中测试：角优先于边，边优先于内部 */
export function hitTestHandle(r, pt, tol = 6) {
  const nearL = Math.abs(pt.x - r.x) <= tol
  const nearR = Math.abs(pt.x - (r.x + r.w)) <= tol
  const nearT = Math.abs(pt.y - r.y) <= tol
  const nearB = Math.abs(pt.y - (r.y + r.h)) <= tol
  const spanX = pt.x >= r.x - tol && pt.x <= r.x + r.w + tol
  const spanY = pt.y >= r.y - tol && pt.y <= r.y + r.h + tol
  const corners = { nw: nearL && nearT, ne: nearR && nearT, se: nearR && nearB, sw: nearL && nearB }
  for (const h of CORNERS) {
    if (corners[h]) return h
  }
  if (spanY && nearL) return 'w'
  if (spanY && nearR) return 'e'
  if (spanX && nearT) return 'n'
  if (spanX && nearB) return 's'
  if (pt.x > r.x && pt.x < r.x + r.w && pt.y > r.y && pt.y < r.y + r.h) return 'inside'
  return 'outside'
}

/** 拖动某个手柄：被拖的边跟随指针，其余边不动 */
export function resizeRect(r, handle, pt, bounds) {
  let x1 = r.x
  let y1 = r.y
  let x2 = r.x + r.w
  let y2 = r.y + r.h
  if (handle.includes('w')) x1 = pt.x
  if (handle.includes('e')) x2 = pt.x
  if (handle.includes('n')) y1 = pt.y
  if (handle.includes('s')) y2 = pt.y
  return clampRect(
    { x: Math.min(x1, x2), y: Math.min(y1, y2), w: Math.abs(x2 - x1), h: Math.abs(y2 - y1) },
    bounds,
  )
}

/** 平移整个选区。
 *  这里是「整体位移 + 位置夹取」，不是 clampRect 的裁剪语义：
 *  拖到边界时选区必须保持尺寸被挡住，而不是被压扁（否则用户一拖到边就丢选区）。 */
export function moveRect(r, dx, dy, bounds) {
  const maxX = Math.max(bounds.x, bounds.x + bounds.w - r.w)
  const maxY = Math.max(bounds.y, bounds.y + bounds.h - r.h)
  const x = Math.min(Math.max(r.x + dx, bounds.x), maxX)
  const y = Math.min(Math.max(r.y + dy, bounds.y), maxY)
  return { ...r, x, y }
}

const NUDGE = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }

/** 方向键微调（1px；按住 Shift 由调用方把 step 换成 10） */
export function nudgeRect(r, key, step, bounds) {
  const d = NUDGE[key]
  return d ? moveRect(r, d[0] * step, d[1] * step, bounds) : r
}
