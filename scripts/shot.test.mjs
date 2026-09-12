// node --test scripts/ —— 截图选区几何纯函数单测（不依赖 Tauri / 浏览器）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle,
  resizeRect, moveRect, nudgeRect,
} from '../src/lib/shot.js'

const B = { x: 0, y: 0, w: 100, h: 80 }

test('拖拽两个方向都得到归一化矩形', () => {
  assert.deepEqual(rectFromDrag({ x: 10, y: 20 }, { x: 40, y: 60 }, B), { x: 10, y: 20, w: 30, h: 40 })
  // 反向拖（从右下往左上）结果相同
  assert.deepEqual(rectFromDrag({ x: 40, y: 60 }, { x: 10, y: 20 }, B), { x: 10, y: 20, w: 30, h: 40 })
})

test('拖出边界被夹回窗口内', () => {
  assert.deepEqual(rectFromDrag({ x: -20, y: -20 }, { x: 200, y: 200 }, B), B)
  assert.deepEqual(clampRect({ x: 90, y: 70, w: 40, h: 40 }, B), { x: 90, y: 70, w: 10, h: 10 })
})

test('最小可确认尺寸是 3×3', () => {
  assert.equal(canConfirm({ x: 0, y: 0, w: 2, h: 9 }), false)
  assert.equal(canConfirm({ x: 0, y: 0, w: 3, h: 3 }), true)
  assert.equal(canConfirm(null), false)
})

test('手柄命中：角优先于边，边优先于内部', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.equal(hitTestHandle(r, { x: 11, y: 11 }), 'nw')
  assert.equal(hitTestHandle(r, { x: 59, y: 11 }), 'ne')
  assert.equal(hitTestHandle(r, { x: 59, y: 49 }), 'se')
  assert.equal(hitTestHandle(r, { x: 11, y: 49 }), 'sw')
  assert.equal(hitTestHandle(r, { x: 35, y: 10 }), 'n')
  assert.equal(hitTestHandle(r, { x: 60, y: 30 }), 'e')
  assert.equal(hitTestHandle(r, { x: 35, y: 30 }), 'inside')
  assert.equal(hitTestHandle(r, { x: 90, y: 30 }), 'outside')
})

test('小选区（3×3）内部仍能命中 inside，不被手柄吃光', () => {
  // 回归：容差若写成固定 tol=6 的半径，3×3 选区的任何内部点都会落到手柄上，
  // 于是小选区只能被拉伸、无法整体移动（Task 7 的 move 分支变成死代码）
  const small = { x: 10, y: 10, w: 3, h: 3 }
  assert.equal(hitTestHandle(small, { x: 11.5, y: 11.5 }), 'inside')
  assert.equal(hitTestHandle(small, { x: 10, y: 10 }), 'nw')
  assert.equal(hitTestHandle(small, { x: 11.5, y: 13 }), 's')
})

test('拖手柄只动被拖的边，且夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(resizeRect(r, 'se', { x: 80, y: 70 }, B), { x: 10, y: 10, w: 70, h: 60 })
  assert.deepEqual(resizeRect(r, 'nw', { x: 0, y: 0 }, B), { x: 0, y: 0, w: 60, h: 50 })
  // 拖过头：夹回边界，不允许负宽
  assert.deepEqual(resizeRect(r, 'se', { x: -50, y: -50 }, B), { x: 0, y: 0, w: 10, h: 10 })
})

test('移动与方向键微调都夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(moveRect(r, 5, -5, B), { x: 15, y: 5, w: 50, h: 40 })
  assert.deepEqual(moveRect(r, 999, 999, B), { x: 50, y: 40, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowRight', 1, B), { x: 11, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowLeft', 10, B), { x: 0, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'KeyQ', 1, B), r)
})
