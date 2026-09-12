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

/** 命中测试：角优先于边，边优先于内部。
 *
 *  容差按轴收紧到 min(tol, 边长/3)：canConfirm 允许 3×3 的小选区，如果容差
 *  固定成 tol=6 的「半径」，那么任何 ≤2×tol（12px）的选区内部点都必然落在
 *  手柄窗口里，'inside' 永远不可达 —— 小选区就只能被拉伸、无法整体移动。
 *  收紧后 50×40 的选区行为与固定 tol 完全一致，小选区则始终留有内部命中区。 */
export function hitTestHandle(r, pt, tol = 6) {
  const tx = Math.min(tol, r.w / 3)
  const ty = Math.min(tol, r.h / 3)
  const nearL = Math.abs(pt.x - r.x) <= tx
  const nearR = Math.abs(pt.x - (r.x + r.w)) <= tx
  const nearT = Math.abs(pt.y - r.y) <= ty
  const nearB = Math.abs(pt.y - (r.y + r.h)) <= ty
  const spanX = pt.x >= r.x - tx && pt.x <= r.x + r.w + tx
  const spanY = pt.y >= r.y - ty && pt.y <= r.y + r.h + ty
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

/**
 * CSS 像素矩形 → 整幅抓屏图像里的物理像素矩形。
 * k 用「本屏 slice 宽 ÷ 窗口 CSS 宽」实时算出，不信任 devicePixelRatio：
 * 本机 KDE Wayland 的 GDK scale_factor 报 2，而真实比例是 1.25。
 */
export function cssRectToImageRect(css, slice, cssWidth) {
  const k = slice.w / cssWidth
  return {
    x: slice.x + Math.round(css.x * k),
    y: slice.y + Math.round(css.y * k),
    w: Math.max(1, Math.round(css.w * k)),
    h: Math.max(1, Math.round(css.h * k)),
  }
}

/** 马赛克块：对齐到 block 网格，返回覆盖 rect 的块矩形列表 */
export function mosaicBlocks(rect, block) {
  const out = []
  const x0 = Math.floor(rect.x / block) * block
  const y0 = Math.floor(rect.y / block) * block
  for (let y = y0; y < rect.y + rect.h; y += block) {
    for (let x = x0; x < rect.x + rect.w; x += block) {
      out.push({ x, y, w: block, h: block })
    }
  }
  return out
}

/** 箭头两翼端点（配合实心三角头部） */
export function arrowHead(from, to, size = 12) {
  const dx = to.x - from.x
  const dy = to.y - from.y
  const len = Math.hypot(dx, dy) || 1
  const ux = dx / len
  const uy = dy / len
  const bx = to.x - ux * size
  const by = to.y - uy * size
  const hx = -uy * size * 0.5
  const hy = ux * size * 0.5
  return { p1: { x: bx + hx, y: by + hy }, p2: { x: bx - hx, y: by - hy } }
}

/** 撤销栈：压入快照并裁到上限（返回新数组，保持不可变便于单测） */
export function pushUndo(stack, snapshot, limit = 20) {
  const next = (stack || []).concat([snapshot])
  return next.length > limit ? next.slice(next.length - limit) : next
}

/** 工具栏贴合：优先选区下方 → 上方 → 窗口内，横向夹在窗口内 */
export function toolbarPlacement(sel, win, bar, gap = 8) {
  let y = sel.y + sel.h + gap
  let flip = 'below'
  if (y + bar.h > win.h) {
    y = sel.y - bar.h - gap
    flip = 'above'
  }
  if (y < 0) {
    y = Math.min(Math.max(0, sel.y + sel.h - bar.h - gap), Math.max(0, win.h - bar.h))
    flip = 'inside'
  }
  const x = Math.min(Math.max(0, sel.x), Math.max(0, win.w - bar.w))
  return { x, y, flip }
}
