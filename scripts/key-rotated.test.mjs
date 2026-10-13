import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const chat = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')
const i18n = readFileSync(new URL('../src/lib/i18n.js', import.meta.url), 'utf8')

// 对端密钥轮换（原版 IPMsg 的 DecryptErr 自动回复）：后端 net.rs 用同一个协议级 token
// 触发自愈（撤缓存 + 重取公钥），前端只负责按语言设置换成本地化提示 + 重发。

test('识别协议级 token，且只对入站消息生效', () => {
  assert.match(
    chat,
    /function isPeerKeyRotated\(m\) \{\s*return m\.dir === 'in' && typeof m\.text === 'string' && m\.text\.includes\('DecryptErr'\)/
  )
})

test('提示分支必须排在通用正文分支之前（否则原文先命中）', () => {
  const rotated = chat.indexOf('isPeerKeyRotated(v.m)')
  const body = chat.indexOf('v-else-if="bodyOf(v.m)"')
  assert.ok(rotated > 0, '模板里要有密钥轮换分支')
  assert.ok(body > 0, '通用正文分支还在')
  assert.ok(rotated < body, '密钥轮换分支要排在前面')
})

test('重发发的是提示之前、我自己发出的最后一条文本；找不到就不静默失败', () => {
  assert.match(chat, /x\.dir === 'out' && x\.text/)
  assert.match(chat, /await sendTextTo\(store\.activeKey, prev\.text, false, false\)/)
  assert.match(chat, /t\('chat\.resendUnavailable'\)/)
})

test('三条文案中英各一条（漏翻会被计数抓到）', () => {
  for (const key of ['chat.peerKeyRotated', 'chat.resend', 'chat.resendUnavailable']) {
    const n = i18n.split(key).length - 1
    assert.ok(n >= 2, `${key} 应该中英各一条，实际 ${n}`)
  }
})
