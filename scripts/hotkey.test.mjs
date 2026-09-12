// node --test scripts/ —— 快捷键串纯函数单测
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  normalizeCombo, isValidCombo, comboFromEvent,
} from '../src/lib/hotkey.js'

test('归一化：大小写/别名/修饰键顺序', () => {
  assert.equal(normalizeCombo('alt+a'), 'Alt+A')
  assert.equal(normalizeCombo('shift+ctrl+s'), 'Ctrl+Shift+S')
  assert.equal(normalizeCombo('cmd+shift+a'), 'CmdOrCtrl+Shift+A')
  assert.equal(normalizeCombo('super+A'), 'CmdOrCtrl+A')
  assert.equal(normalizeCombo('  Alt + A '), 'Alt+A')
  assert.equal(normalizeCombo('enter'), 'Enter')
  assert.equal(normalizeCombo(''), null)
  assert.equal(normalizeCombo('a+b'), null) // 两个非修饰键非法
  assert.equal(normalizeCombo(null), null)
})

test('必须是「修饰键 + 主键」，纯修饰键或单键不算有效全局热键', () => {
  assert.equal(isValidCombo('Alt+A'), true)
  assert.equal(isValidCombo('A'), false)
  assert.equal(isValidCombo('Shift'), false)
  assert.equal(isValidCombo('Ctrl+Shift'), false)
  // 设置页用它标出「配置里存着一个不可用的快捷键」（手改配置 / 跨平台拷配置）
  assert.equal(isValidCombo(''), false)
  assert.equal(isValidCombo(undefined), false)
})

test('从键盘事件录制组合键', () => {
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: true, shiftKey: false, metaKey: false }), 'Alt+A')
  assert.equal(comboFromEvent({ key: 'S', ctrlKey: true, altKey: false, shiftKey: true, metaKey: false }), 'Ctrl+Shift+S')
  assert.equal(comboFromEvent({ key: 'Meta', ctrlKey: false, altKey: false, shiftKey: false, metaKey: true }), null)
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false }), null)
})
