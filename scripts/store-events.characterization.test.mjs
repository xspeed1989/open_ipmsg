/**
 * 事件迁移 characterization 网 —— 把 `store.js` 今天的事件→状态行为**钉住**。
 *
 * 这批用例刻意不描述「应该怎样」，只记录「现在怎样」：候选 2 要把这半边抽成
 * 独立 module，抽之前先把行为锁死，抽之后**断言一个字都不许改**（测试不随实现
 * 变动而变，才是「描述行为而非实现」）。所以要改这里的断言，先问自己：
 * 我是要改行为，还是要把测试改成迁就实现？
 *
 * 跨的 seam 与调用方相同：真 store + 真 ipc.js，后端是 mockIPC adapter，
 * 事件从 `plugin:event|listen` 拦下来的 handler 真投进去。
 */
import { test, before, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore, inRecord, outRecord, PEER } from './helpers/store-harness.mjs'

let h
before(async () => { h = await startStore() })
beforeEach(async () => { await h.reset() })

/* ---------------- 接线 ---------------- */

test('启动：11 个后端事件订阅 + 窗口焦点两条都注册了', () => {
  assert.deepEqual([...h.listeners.keys()].sort(), [
    'absence-info', 'file-progress', 'msg-delivered', 'msg-in', 'msg-out',
    'msg-read', 'msg-recalled', 'msg-unlocked', 'open-chat', 'open-unread',
    'tauri://blur', 'tauri://focus', 'users-updated',
  ])
})

test('启动：11 个订阅都早于 loadUsers / loadSessions（否则事件会丢）', () => {
  const lastListen = h.bootLog.map((e) => e.cmd).lastIndexOf('plugin:event|listen')
  const firstUsers = h.bootLog.findIndex((e) => e.cmd === 'get_users')
  const firstSessions = h.bootLog.findIndex((e) => e.cmd === 'list_sessions')
  assert.ok(lastListen >= 0 && firstUsers > lastListen, 'get_users 晚于最后一个 listen')
  assert.ok(firstSessions > lastListen, 'list_sessions 晚于最后一个 listen')
})

/* ---------------- 每个订阅一条 ---------------- */

test('users-updated：重拉在线用户与会话，并重建中栏列表', async () => {
  h.backend.get_users = [{ key: PEER, ip: PEER, nickname: '对端', host: 'h', group: 'g', user: 'u' }]
  h.backend.list_sessions = [{ key: '10.0.0.8', nickname: '离线的人', unread: 2, unread_ts: 1 }]
  h.emit('users-updated', {}) // 回调是 fire-and-forget，等状态自己长出来
  await h.waitUntil(() => h.store.sessions.length === 1 && h.store.users.length === 1)

  assert.equal(h.store.userMap[PEER].nickname, '对端')
  assert.deepEqual(h.store.sessionList.map((s) => s.key).sort(), ['10.0.0.8', PEER].sort())
})

test('msg-in（非当前会话）：上屏 + 未读 +1 + 记时间戳', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.chats[PEER].msgs.length, 1)
  assert.equal(h.store.unread[PEER], 1)
  assert.equal(h.store.unreadTs[PEER], 1_700_000_000)
  assert.equal(h.store.lastTs[PEER], 1_700_000_000)
  assert.equal(h.store.peerMeta[PEER].nickname, '对端')
  // 通知分支（本机 UA 不是 Linux、shim 的 Notification 已拒）不在这里钉：
  // 按 ADR-0001，通知属于留在 store 的那半边，不进事件迁移的 characterization。
  assert.equal(h.countOf('plugin:notification|notify'), 0)
})

test('msg-in（当前会话且聚焦）：不进未读，且回已读回执', async () => {
  h.store.activeKey = PEER
  h.store.windowFocused = true
  const p = h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  await p

  assert.equal(h.store.unread[PEER], undefined, '当前会话不该有未读')
  assert.equal(h.store.chats[PEER].msgs[0].read, true, '本地乐观置位已读')
  const marks = h.calls('mark_read')
  assert.equal(marks.length, 1)
  assert.deepEqual(marks[0].args, { key: PEER, pkts: [101] })
  assert.ok(
    h.firstAt('get_history') < h.firstAt('mark_read'),
    '顺序：先把消息真正入列（get_history）再标已读',
  )
})

test('msg-in 重投（同包号同方向）：原地替换，不重复上屏、不重复计未读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: true })

  assert.equal(h.store.chats[PEER].msgs.length, 1, '同包号只留一条')
  assert.equal(h.store.unread[PEER], 1, '重投不再加未读')
})

test('msg-out（广播信箱以外）：只上屏，不动未读', async () => {
  await h.emit('msg-out', { key: PEER, msg: outRecord() })

  assert.equal(h.store.chats[PEER].msgs.length, 1)
  assert.equal(h.store.unread[PEER], undefined)
})

test('open-unread：跳到最近有未读的会话并标已读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  await h.emit('open-unread', {})

  assert.equal(h.store.activeKey, PEER)
  assert.equal(h.store.unread[PEER], undefined, '未读清零')
  assert.equal(h.store.chats[PEER].msgs[0].read, true)
  assert.ok(h.countOf('mark_read') >= 1)
  // 切会话之后要把窗口弄到前面（三个窗口命令都发过）
  for (const cmd of ['plugin:window|show', 'plugin:window|unminimize', 'plugin:window|set_focus']) {
    assert.ok(h.countOf(cmd) >= 1, `缺 ${cmd}`)
  }
})

test('open-chat：按给定 key 切会话，并补一次标已读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  await h.emit('open-chat', { key: PEER })

  assert.equal(h.store.activeKey, PEER)
  assert.equal(h.store.unread[PEER], undefined)
  assert.ok(h.countOf('plugin:window|set_focus') >= 1, '窗口要置前')
})

test('open-chat 没有 key：什么都不做（连窗口动作都不发）', async () => {
  await h.emit('open-chat', { key: '' })
  assert.equal(h.store.activeKey, null)
  assert.equal(h.countOf('plugin:window|set_focus'), 0)
})

test('msg-read：出站那条翻成已读，并且把已送达一并置上', async () => {
  h.store.chats[PEER] = { msgs: [outRecord({ pkt: 201, read: false, delivered: false })] }
  h.emit('msg-read', { key: PEER, pkt: 201 })

  const m = h.store.chats[PEER].msgs[0]
  assert.equal(m.read, true)
  assert.equal(m.delivered, true, '已读蕴含已送达')
})

test('msg-delivered：只翻已送达，不冒充已读', async () => {
  h.store.chats[PEER] = { msgs: [outRecord({ pkt: 201, read: false, delivered: false })] }
  h.emit('msg-delivered', { key: PEER, pkt: 201 })

  const m = h.store.chats[PEER].msgs[0]
  assert.equal(m.delivered, true)
  assert.equal(m.read, false)
})

test('file-progress：消息已入列时直接落到附件上', () => {
  h.store.chats[PEER] = {
    msgs: [inRecord({ kind: 'file', pkt: 101, files: [{ id: 7, name: 'x.png', size: 10, state: 'downloading' }] })],
  }
  h.emit('file-progress', { key: PEER, pkt: 101, file_id: 7, transferred: 10, total: 10, done: true, path: '/dl/x.png' })

  const f = h.store.chats[PEER].msgs[0].files[0]
  assert.equal(f.state, 'done')
  assert.equal(f.path, '/dl/x.png')
})

test('file-progress 早于消息入列（get_history 慢）：暂存后回放，不再卡在「下载中」', async () => {
  // 这条钉的是真机故障的时序：msg-in 先进来（要 await get_history），
  // 下载完成事件在这个窗口里到达，消息还没进 msgs
  let release
  h.backend.get_history = () => new Promise((resolve) => {
    release = () => resolve([inRecord({
      kind: 'file', pkt: 101,
      files: [{ id: 7, name: '剪贴板图片.png', size: 10, state: 'downloading' }],
    })])
  })

  const pending = h.emit('msg-in', {
    key: PEER,
    msg: inRecord({ kind: 'file', pkt: 101, files: [{ id: 7, name: '剪贴板图片.png', size: 10, state: 'downloading' }] }),
    resend: false,
  })
  assert.equal(h.store.chats[PEER], undefined, '此刻会话还没建好（窗口就开在这里）')

  h.emit('file-progress', { key: PEER, pkt: 101, file_id: 7, transferred: 10, total: 10, done: true, path: '/dl/剪贴板图片.png' })
  release()
  await pending

  const f = h.store.chats[PEER].msgs[0].files[0]
  assert.equal(f.state, 'done', '完成事件必须被回放')
  assert.equal(f.path, '/dl/剪贴板图片.png')
})

test('msg-recalled：标记撤回并清掉未读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord({ pkt: 101 }), resend: false })
  h.emit('msg-recalled', { key: PEER, pkt: 101, peer: '对端' })

  assert.equal(h.store.chats[PEER].msgs[0].recalled, true)
  assert.equal(h.store.unread[PEER], undefined)
  assert.equal(h.store.unreadTs[PEER], undefined)
})

test('msg-unlocked：开封后 unlocked/locked/read 三处一起翻', () => {
  h.store.chats[PEER] = {
    msgs: [inRecord({ pkt: 101, secret: true, locked: false, unlocked: false, read: false })],
  }
  h.emit('msg-unlocked', { key: PEER, pkt: 101 })

  const m = h.store.chats[PEER].msgs[0]
  assert.equal(m.unlocked, true)
  assert.equal(m.locked, false)
  assert.equal(m.read, true)
})

test('absence-info：把不在文案记到 userMap 上', () => {
  h.store.userMap[PEER] = { key: PEER, nickname: '对端', absence_text: '' }
  h.emit('absence-info', { key: PEER, text: '开会中，稍后回' })

  assert.equal(h.store.userMap[PEER].absence_text, '开会中，稍后回')
})

/* ---------------- 窗口焦点 ---------------- */

test('窗口失焦/聚焦：翻转 windowFocused，聚焦时对当前会话补标已读', async () => {
  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  assert.equal(h.store.unread[PEER], 1, '先造出一条未读')

  h.emit('tauri://blur', undefined)
  assert.equal(h.store.windowFocused, false)

  h.store.activeKey = PEER
  await h.emit('tauri://focus', undefined)

  assert.equal(h.store.windowFocused, true)
  assert.equal(h.store.unread[PEER], undefined, '聚焦后当前会话清零')
  assert.equal(h.store.chats[PEER].msgs[0].read, true)
})
