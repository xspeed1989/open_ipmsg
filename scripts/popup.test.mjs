// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { computePopupPosition } from '../src/lib/popup.js'

// 面板尺寸取自 EmojiPicker 实际样式：width 264 + padding，高度按 5 行估 176
const PANEL_W = 264
const PANEL_H = 176
const MARGIN = 8
const GAP = 8

test('默认出现在锚点上方，左缘与按钮对齐', () => {
  const pos = computePopupPosition(
    { left: 40, top: 600, bottom: 634, width: 36 },
    PANEL_W, PANEL_H, 980, 660,
  )
  assert.equal(pos.top, 600 - PANEL_H - GAP)
  assert.equal(pos.left, 40)
})

test('右侧越界时贴视口右缘，不超出屏幕', () => {
  const pos = computePopupPosition(
    { left: 900, top: 600, bottom: 634, width: 36 },
    PANEL_W, PANEL_H, 980, 660,
  )
  assert.equal(pos.left, 980 - MARGIN - PANEL_W)
})

test('左侧贴边时留出边距', () => {
  const pos = computePopupPosition(
    { left: 2, top: 600, bottom: 634, width: 36 },
    PANEL_W, PANEL_H, 980, 660,
  )
  assert.equal(pos.left, MARGIN)
})

test('上方空间不足时翻转到锚点下方', () => {
  const pos = computePopupPosition(
    { left: 40, top: 100, bottom: 134, width: 36 },
    PANEL_W, PANEL_H, 980, 660,
  )
  assert.equal(pos.top, 134 + GAP)
  assert.ok(pos.top > 100, '面板出现在按钮下方')
})

test('视口比面板还窄时兜底到左边距', () => {
  const pos = computePopupPosition(
    { left: 40, top: 600, bottom: 634, width: 36 },
    PANEL_W, PANEL_H, 200, 660,
  )
  assert.equal(pos.left, MARGIN)
})
