// node --test scripts/ —— 群发扇出单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { groupPayload, sendToEach } from '../src/lib/groupsend.js'

/* ---------------- groupPayload ---------------- */

test('纯文本群发：载荷是文本，连续空行压成一行', () => {
  const p = groupPayload('第一行\n\n\n\n第二行\n\n', [])
  assert.deepEqual(p, { kind: 'text', text: '第一行\n\n第二行' })
})

test('带附件的群发：载荷是文件，文字作为说明一起发出', () => {
  const p = groupPayload('看这个', ['/tmp/a.pdf'])
  assert.deepEqual(p, { kind: 'files', paths: ['/tmp/a.pdf'], text: '看这个' })
})

test('只有附件没有文字：文本为空串而不是 undefined', () => {
  const p = groupPayload('', ['/tmp/a.png'])
  assert.deepEqual(p, { kind: 'files', paths: ['/tmp/a.png'], text: '' })
})

/* ---------------- sendToEach ---------------- */

test('逐个单发：全部成功时计数正确且无失败', async () => {
  const seen = []
  const r = await sendToEach(['a', 'b', 'c'], async (k) => seen.push(k))
  assert.deepEqual(seen, ['a', 'b', 'c'])
  assert.deepEqual(r, { ok: 3, fails: [] })
})

test('逐个单发：单个收件人失败不中断其余，失败者被记录', async () => {
  const seen = []
  const r = await sendToEach(['a', 'b', 'c'], async (k) => {
    seen.push(k)
    if (k === 'b') throw new Error('对方不在线')
  })
  assert.deepEqual(seen, ['a', 'b', 'c'], 'b 失败后 c 仍要继续发')
  assert.deepEqual(r, { ok: 2, fails: ['b'] })
})

test('未选任何收件人时不发送', async () => {
  let called = 0
  const r = await sendToEach([], async () => called++)
  assert.equal(called, 0)
  assert.deepEqual(r, { ok: 0, fails: [] })
})
