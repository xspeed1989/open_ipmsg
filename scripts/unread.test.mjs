// node --test scripts/  —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { applyUnreadFromSessions } from '../src/lib/unread.js'

const S = (key, unread, unread_ts = 0) => ({ key, unread, unread_ts })

test('启动补数：监听就绪前落库的离线留言补出未读红点', () => {
  // WebView 就绪前到达的 msg-in 事件被丢弃，但后端已落库且 read=false；
  // 会话摘要的 unread 就是该状态的投影，必须能被补进未读表
  const unread = {}
  const unreadTs = {}
  applyUnreadFromSessions(unread, unreadTs, {}, [
    S('10.0.0.9', 2, 1725000000),
    S('10.0.0.8', 1, 1724000000),
  ])
  assert.equal(unread['10.0.0.9'], 2)
  assert.equal(unreadTs['10.0.0.9'], 1725000000)
  assert.equal(unread['10.0.0.8'], 1)
  assert.equal(unreadTs['10.0.0.8'], 1724000000)
})

test('已在线计数的会话不重复记账（实时事件已算过）', () => {
  const unread = { '10.0.0.9': 1 }
  const unreadTs = { '10.0.0.9': 111 }
  applyUnreadFromSessions(unread, unreadTs, {}, [S('10.0.0.9', 1, 222)])
  assert.equal(unread['10.0.0.9'], 1, '保持实时计数，不被摘要覆盖')
  assert.equal(unreadTs['10.0.0.9'], 111)
})

test('已加载聊天的会话跳过（内存态含乐观已读/实时计数）', () => {
  // 用户已打开会话并全部读完：摘要可能仍是后端未落库的旧计数，
  // 若补进来就会出现「已读又变未读」的幽灵红点
  const unread = {}
  const unreadTs = {}
  const chats = { '10.0.0.9': { msgs: [] } }
  applyUnreadFromSessions(unread, unreadTs, chats, [S('10.0.0.9', 3, 123)])
  assert.equal(unread['10.0.0.9'], undefined, '已加载会话不补数')
})

test('摘要未读为 0 的会话不写入（空键不污染未读表）', () => {
  const unread = {}
  const unreadTs = {}
  applyUnreadFromSessions(unread, unreadTs, {}, [S('10.0.0.9', 0)])
  assert.deepEqual(unread, {})
  assert.deepEqual(unreadTs, {})
})

test('容错：旧版后端摘要没有 unread 字段时安全跳过', () => {
  const unread = {}
  applyUnreadFromSessions(unread, {}, {}, [{ key: '10.0.0.9', last_ts: 5 }])
  assert.deepEqual(unread, {})
})

test('unread_ts 缺失时补 0（托盘唤起排序退化为 lastTs 兜底）', () => {
  const unread = {}
  const unreadTs = {}
  applyUnreadFromSessions(unread, unreadTs, {}, [S('10.0.0.9', 1)])
  assert.equal(unread['10.0.0.9'], 1)
  assert.equal(unreadTs['10.0.0.9'], 0)
})

test('多次调用幂等：启动补数 + users-updated 刷新都安全', () => {
  const unread = {}
  const unreadTs = {}
  applyUnreadFromSessions(unread, unreadTs, {}, [S('10.0.0.9', 2, 111)])
  // 刷新时摘要仍是同一份持久化状态
  applyUnreadFromSessions(unread, unreadTs, {}, [S('10.0.0.9', 2, 111)])
  assert.equal(unread['10.0.0.9'], 2)
  assert.equal(unreadTs['10.0.0.9'], 111)
})