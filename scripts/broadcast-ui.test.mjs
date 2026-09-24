import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const chat = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')

test('广播收到的每条消息都显示发送者，不依赖连续消息分组', () => {
  assert.match(chat, /v-if="isBroadcast && v\.m\.dir === 'in'" class="sender-line"/)
  assert.match(chat, /const merged =\s*!isBroadcast\.value && prev && !dayInserted &&/)
})
