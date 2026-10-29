/**
 * 新会话的第一条消息也要提醒（通知 + 未读）。
 *
 * 缺陷：后端把消息**先落库再发事件**（src-tauri/src/net.rs:2091），而
 * store.dispatchEvent 对 msg-in 会先 await ensureChat(key)；会话没加载过时
 * ensureChat 拉回的历史里已经含刚到的这条，于是 transitions.onMsgIn 的
 * 同包号去重把它当成「重投」，未读不加、通知不弹 —— 某个会话在本进程里收到的
 * 第一条消息永远不会提醒（第二条起正常，因为会话已在内存里，不会再拉历史）。
 *
 * 修法不是放宽去重，而是**别让历史里那份重复计入**：ensureChat 加载历史时把
 * 本次事件正在投递的那条排除掉（事件载荷才是权威副本，insertMessage 会并入，
 * 并保留历史里的已读/附件状态）。
 *
 * 跨的 seam 与生产一致：真 store.js + 真 ipc.js + mockIPC 后端，事件真投递给
 * store 注册的监听器（见 scripts/helpers/store-harness.mjs）。
 */
import { test, before, beforeEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore, inRecord, PEER } from './helpers/store-harness.mjs'

let h
before(async () => { h = await startStore() })
beforeEach(async () => { await h.reset() })

test('会话未加载且后端已落库：首条消息仍计未读、仍要通知', async () => {
  // 后端先 upsert 再 emit —— 所以 get_history 里已经有这条
  h.backend.get_history = () => [inRecord()]
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.chats[PEER].msgs.length, 1, '上屏只留一条（不重复）')
  assert.equal(h.store.unread[PEER], 1, '首条消息要计未读')
  assert.equal(h.store.unreadTs[PEER], 1_700_000_000, '未读时间戳取本条消息')
})

test('首条消息即当前会话且聚焦：不计未读，但要回已读回执', async () => {
  h.store.activeKey = PEER
  h.store.windowFocused = true
  h.backend.get_history = () => [inRecord()]
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.unread[PEER], undefined, '当前会话不该长未读')
  assert.equal(h.store.chats[PEER].msgs[0].read, true, '乐观置位已读')
  assert.deepEqual(h.calls('mark_read')[0]?.args, { key: PEER, pkts: [101] })
})

test('会话已加载时的真重投（同包号）仍然不重复计未读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.chats[PEER].msgs.length, 1, '同包号只留一条')
  assert.equal(h.store.unread[PEER], 1, '重投不再加未读')
})

test('历史里的其他消息不受影响：只排除正在投递的那一条', async () => {
  const old = inRecord({ pkt: 99, text: '更早的消息', ts: 1_600_000_000 })
  h.backend.get_history = () => [old, inRecord()]
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.chats[PEER].msgs.length, 2, '历史那条要保留')
  assert.deepEqual(h.store.chats[PEER].msgs.map((m) => m.pkt), [99, 101])
  assert.equal(h.store.unread[PEER], 1, '只有新到的那条算未读')
})
