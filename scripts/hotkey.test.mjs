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

test('空格键能录成 Space，无法表达的键被拒绝', () => {
  assert.equal(comboFromEvent({ key: ' ', code: 'Space', ctrlKey: true }), 'Ctrl+Space')
  assert.equal(normalizeCombo('ctrl+space'), 'Ctrl+Space')
  // '+' 在 '+' 分隔的规范形里无法表达，直接拒绝而不是产出坏串
  assert.equal(comboFromEvent({ key: '+', ctrlKey: true }), null)
})

test('macOS 的 Option 合成字符不会产出不可解析的组合键', () => {
  // 按住 Option 再按 A：e.key 是 'å'，e.code 仍是 'KeyA'
  assert.equal(comboFromEvent({ key: 'å', code: 'KeyA', altKey: true }), 'Alt+A')
  // 拿不到 code 时拒绝合成字符，不猜
  assert.equal(comboFromEvent({ key: 'å', altKey: true }), null)
})

test('未知键名与越界 F 键不算合法热键', () => {
  assert.equal(isValidCombo('Ctrl+Foobar'), false)
  assert.equal(isValidCombo('Ctrl+F0'), false)
  assert.equal(isValidCombo('Ctrl+F99'), false)
  assert.equal(isValidCombo('Ctrl+F24'), true)
  assert.equal(normalizeCombo('Alt+Å'), null)
})

test('规范名可往返：normalizeCombo 幂等', () => {
  assert.equal(normalizeCombo('Alt+Up'), 'Alt+ArrowUp')
  assert.equal(normalizeCombo('Alt+ArrowUp'), 'Alt+ArrowUp')
  assert.equal(isValidCombo('Alt+ArrowUp'), true)
  assert.equal(comboFromEvent({ key: 'ArrowUp', code: 'ArrowUp', altKey: true }), 'Alt+ArrowUp')
})

import { isWaylandUA } from '../src/lib/hotkey.js'

test('Wayland 会话粗判只看显式 Wayland 标记', () => {
  assert.equal(isWaylandUA('Mozilla/5.0 (X11; Linux x86_64)'), false)
  assert.equal(isWaylandUA('Mozilla/5.0 (Wayland; Linux x86_64)'), true)
  assert.equal(isWaylandUA(''), false)
})
