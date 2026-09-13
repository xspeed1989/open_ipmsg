// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { forwardPayload } from '../src/lib/forward.js'
import { mergeSessions } from '../src/lib/sessions.js'
import {
  BROADCAST_SESSION_KEY, isBroadcastSession, senderOf, senderIp,
} from '../src/lib/sessions.js'

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

/* ---------------- 广播信箱（pinned 常驻条目） ---------------- */

test('广播信箱：置顶条目排在普通会话之前', () => {
  const list = mergeSessions(online, [
    { key: '10.0.0.9', nickname: '离线赵', last_ts: 900 },
    { key: '255.255.255.255', nickname: '', pinned: true, last_ts: 100 },
  ])
  assert.equal(list[0].key, '255.255.255.255', '置顶条目不管时间多旧都在最前')
  assert.equal(list[0].online, false, '广播信箱不是在线对端，不参与在线语义')
  assert.equal(list[1].key, '10.0.0.1', '在线用户紧随其后')
})

test('广播信箱：只有它自己时列表也不为空（可直接点进去发广播）', () => {
  const list = mergeSessions([], [{ key: '255.255.255.255', pinned: true, last_ts: 0 }])
  assert.equal(list.length, 1)
  assert.equal(list[0].key, '255.255.255.255')
})

/* ---------------- 广播信箱里的发送方归属 ---------------- */

const T = { me: '我', unknown: '未知' }

test('发送方：自己发的算「我」，不借用对端身份', () => {
  assert.equal(senderOf({ dir: 'out', peer: { nickname: 'ubuntu' } }, T), '我')
})

test('发送方：别人发的用它自己的昵称快照，而不是会话名', () => {
  const m = { dir: 'in', peer: { nickname: 'ubuntu', ip: '192.168.2.115' } }
  assert.equal(senderOf(m, T), 'ubuntu')
  assert.equal(senderIp(m), '192.168.2.115', '同一昵称多台机器时用 IP 区分')
})

test('发送方：昵称为空退化为用户名，再退化为 IP', () => {
  assert.equal(senderOf({ dir: 'in', peer: { user: 'win7', ip: '10.0.0.7' } }, T), 'win7')
  assert.equal(senderOf({ dir: 'in', peer: { ip: '10.0.0.7' } }, T), '10.0.0.7')
})

test('发送方：旧记录没有快照时给「未知」，不把伪造的广播地址当 IP 显示', () => {
  const legacy = { dir: 'in', peer: { key: '255.255.255.255', nickname: '' } }
  assert.equal(senderOf(legacy, T), '未知')
  assert.equal(senderIp(legacy), '', 'peer.key 是广播地址，不能当发送方 IP')
})

test('发送方：缺消息/缺 peer 都安全', () => {
  assert.equal(senderOf(undefined, T), '')
  assert.equal(senderOf({ dir: 'in' }, T), '未知')
  assert.equal(senderIp({}), '')
})

test('发送方：昵称里的控制字符被清掉，不污染界面', () => {
  assert.equal(senderOf({ dir: 'in', peer: { nickname: 'ub\u0000un\u001ftu' } }, T), 'ubuntu')
})

test('会话 key 判定：只有广播地址算广播信箱', () => {
  assert.equal(isBroadcastSession(BROADCAST_SESSION_KEY), true)
  assert.equal(isBroadcastSession('192.168.2.115'), false)
  assert.equal(isBroadcastSession(''), false)
})

/* ---------------- mergeForward（多选合并转发） ---------------- */

import { mergeForward } from '../src/lib/forward.js'

const nickOf = (dir) => (dir === 'out' ? '我' : '老王')

test('合并转发：按时间升序拼行，自己与对方分别标注', () => {
  const out = mergeForward(
    [
      { dir: 'in', ts: 200, text: '第二条' },
      { dir: 'out', ts: 100, text: '第一条' },
    ],
    nickOf
  )
  assert.equal(out, '【我】第一条\n【老王】第二条')
})

test('合并转发：附件消息在行尾追加占位，有正文时空格分隔', () => {
  const out = mergeForward(
    [
      { dir: 'in', ts: 1, text: '', files: [{ name: 'a.zip' }, { name: 'b.png' }] },
      { dir: 'out', ts: 2, text: '资料', files: [{ name: 'c.pdf' }] },
    ],
    nickOf
  )
  assert.equal(out, '【老王】[附件] a.zip, b.png\n【我】资料 [附件] c.pdf')
})

test('合并转发：全空的选中项拼不出内容，返回 null 交调用方提示', () => {
  assert.equal(
    mergeForward(
      [
        { dir: 'in', ts: 1, text: '   ' },
        { dir: 'out', ts: 2, text: '' },
      ],
      nickOf
    ),
    null
  )
})

test('合并转发：空数组与非空混合都稳定', () => {
  assert.equal(mergeForward([], nickOf), null)
  const mixed = mergeForward([{ dir: 'in', ts: 5, text: 'hi' }, { dir: 'in', ts: 6 }], nickOf)
  assert.equal(mixed, '【老王】hi')
})
