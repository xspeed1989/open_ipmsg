// node --test scripts/ —— 设置页样式契约（不依赖 Tauri / 浏览器运行时）
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const source = readFileSync(new URL('../src/components/SettingsModal.vue', import.meta.url), 'utf8')
const css = source.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''

function declarations(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const matches = css.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}\\s*\\{([^}]*)\\}`, 'g'))
  return Array.from(matches, (match) => match[1]).join('\n')
}

test('协议扩展设置按纵向表单布局，不退化为 si-row 的横向挤压', () => {
  assert.match(declarations('.adv-col'), /flex-direction\s*:\s*column\b/)
  assert.match(declarations('.adv-line'), /display\s*:\s*flex\b/)
  assert.match(declarations('.adv-line'), /align-items\s*:\s*center\b/)
  assert.match(declarations('.adv-input'), /width\s*:\s*100%/)
})

test('截图设置分区包含热键录制与自动复制开关', () => {
  assert.match(source, /shot_hotkey/, '设置页必须能改截图热键')
  assert.match(source, /shot_copy_clipboard/, '设置页必须有「确认后复制到剪贴板」开关')
})

test('截图热键输入框是只读录制控件，不做自由文本输入', () => {
  const css = source.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''
  assert.match(css, /\.hotkey-input/, '热键录制控件需要独立样式（避免与普通输入框混淆）')
  assert.match(source, /readonly/, '热键框必须 readonly，值只能由按键录制写入')
})
