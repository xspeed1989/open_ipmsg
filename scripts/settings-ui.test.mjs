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
