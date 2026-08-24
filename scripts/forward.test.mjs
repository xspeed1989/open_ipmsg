// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { forwardPayload } from '../src/lib/forward.js'
import { mergeSessions } from '../src/lib/sessions.js'

/* ---------------- forwardPayload ---------------- */

test('文本消息可转发，原样携带正文', () => {
  const p = forwardPayload({ kind: 'text', text: '明天开会' })
  assert.deepEqual(p, { ok: true, kind: 'text', text: '明天开会' })
})

test('文件消息：所有文件都有本地路径时可转发', () => {
  const p = forwardPayload({
    kind: 'file',
    files: [
      { id: 1, name: 'a.pdf', path: '/tmp/a.pdf' },
      { id: 2, name: 'b.png', path: '/tmp/b.png' },
    ],
  })
  assert.deepEqual(p, { ok: true, kind: 'files', paths: ['/tmp/a.pdf', '/tmp/b.png'] })
})

test('文件消息：存在本地没有的路径（未下载）时不可转发并说明原因', () => {
  const p = forwardPayload({
    kind: 'file',
    files: [
      { id: 1, name: 'a.pdf', path: '/tmp/a.pdf' },
      { id: 2, name: 'b.zip', path: '' },
    ],
  })
  assert.equal(p.ok, false)
  assert.match(p.reason, /b\.zip/)
})

test('文件消息：完全无路径也不可转发', () => {
  const p = forwardPayload({ kind: 'file', files: [{ id: 1, name: 'x.zip' }] })
  assert.equal(p.ok, false)
})

test('缺字段/空消息不可转发', () => {
  assert.equal(forwardPayload(undefined).ok, false)
  assert.equal(forwardPayload({}).ok, false)
  assert.equal(forwardPayload({ kind: 'text', text: '' }).ok, false)
})

/* ---------------- mergeSessions ---------------- */

const online = [
  { key: '10.0.0.1', nickname: '小王', group: '财务' },
  { key: '10.0.0.2', nickname: '老李', group: '开发' },
]
const offline = [
  { key: '10.0.0.1', nickname: '小王', group: '财务', last_ts: 500 },
  { key: '10.0.0.9', nickname: '离线赵', group: '财务', last_ts: 300 },
]

test('合并列表：在线优先标记，离线会话补进来', () => {
  const list = mergeSessions(online, offline)
  assert.equal(list.length, 3)
  const byKey = Object.fromEntries(list.map((s) => [s.key, s]))
  assert.equal(byKey['10.0.0.1'].online, true, '在线用户保持在线标记')
  assert.equal(byKey['10.0.0.9'].online, false, '只有历史没有在线的用户标记为离线')
  assert.equal(byKey['10.0.0.9'].nickname, '离线赵', '离线用户带昵称')
})

test('同一 key 同时在线与有历史时只保留在线条目', () => {
  const list = mergeSessions(online, offline)
  assert.equal(list.filter((s) => s.key === '10.0.0.1').length, 1)
})

test('参数缺省安全', () => {
  assert.deepEqual(mergeSessions(undefined, undefined), [])
  assert.deepEqual(mergeSessions(online, undefined).length, 2)
})