/**
 * 隐藏到托盘时不许吞通知（windowVisible 那半边）。
 *
 * 缺陷：通知决策在**前端**——`transitions.isChatVisible` 判定「会话正开着且窗口
 * 聚焦」就把消息当已读、不弹通知。而 `windowFocused` 只由 Tauri 的 focus/blur
 * 事件维护，可应用里的 hide() 有好几处（关窗到托盘、Wayland 重映射、托盘唤起）
 * 并不保证产生 blur。一旦停在「窗口藏着、状态却自称聚焦」，那个会话的每条消息
 * 都会被标已读且不弹通知 —— 用户看到的就是「消息在聊天里能看，通知没弹」。
 *
 * 修法：可见性改成**后端事实**（window-visibility 事件），三个条件
 * （activeKey === key && windowVisible && windowFocused）才允许吞通知。
 * 跨的 seam 与生产一致：真 store.js + 真 ipc.js + mockIPC 后端（见
 * scripts/helpers/store-harness.mjs），事件真投递给 store 注册的监听器。
 */
import { test, before, beforeEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore, inRecord, PEER } from './helpers/store-harness.mjs'

let h
before(async () => {
  h = await startStore()
  // 通知命令只在 Linux/Windows 上走原生路径（macOS 走插件）。Node 默认 UA 是
  // "Node.js/xx"，这里改成 Linux，让 notify 落到 ipc.notifyMessage 上。
  Object.defineProperty(navigator, 'userAgent', {
    value: 'Mozilla/5.0 (X11; Linux x86_64)',
    configurable: true,
  })
})
beforeEach(async () => {
  await h.reset()
  h.store.activeKey = PEER
})

/** 已写入的 notify scope 诊断行（后端命令流水里取 detail） */
const notifyLines = () => h.calls('diag_log')
  .filter((e) => e.args?.scope === 'notify')
  .map((e) => String(e.args.detail))

test('隐藏时不算可见：不标已读、要计未读、要弹通知', async () => {
  h.store.windowFocused = true // 焦点事件还没来（或是假的）
  await h.emit('window-visibility', { visible: false })

  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.windowVisible, false, '后端说藏起来了')
  assert.equal(h.store.windowFocused, false, '隐藏时焦点一并清掉（不能自称聚焦）')
  assert.equal(h.store.unread[PEER], 1, '藏起来时收到的消息必须计未读')
  assert.equal(h.countOf('mark_read'), 0, '不能悄悄标已读')
  await h.waitUntil(() => h.countOf('notify_message') === 1)
  assert.ok(notifyLines().some((l) => l.startsWith('notified')), '决策日志记 notified')
})

test('可见且聚焦才算可见：标已读、不弹通知', async () => {
  await h.emit('window-visibility', { visible: true })
  await h.emit('tauri://focus', undefined)

  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.unread[PEER], undefined, '当前会话不该长未读')
  assert.deepEqual(h.calls('mark_read')[0]?.args, { key: PEER, pkts: [101] })
  assert.equal(h.countOf('notify_message'), 0, '看得见就别打扰')
  await h.waitUntil(() => notifyLines().some((l) => l.startsWith('skipped:chat-visible')))
})

test('可见但不聚焦（窗口在后面）仍要通知', async () => {
  await h.emit('window-visibility', { visible: true })
  await h.emit('tauri://blur', undefined)

  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  assert.equal(h.store.unread[PEER], 1)
  await h.waitUntil(() => h.countOf('notify_message') === 1)
})

test('hide→show 最终停在 visible=true，不会卡在 false 把之后的通知全吞掉', async () => {
  // 托盘唤起在 Wayland 上是 hide 再 show 两连击（raise_main_window 的重映射）
  await h.emit('window-visibility', { visible: false })
  await h.emit('window-visibility', { visible: true })

  assert.equal(h.store.windowVisible, true, '收尾必须是 true')
  // 焦点不会由可见性事件反推（那是焦点事件的活）—— 保守起来宁可多弹一条
  assert.equal(h.store.windowFocused, false)
  await h.emit('tauri://focus', undefined)
  assert.equal(h.store.windowFocused, true, '真正的焦点事件到了才置回')

  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })
  assert.equal(h.store.unread[PEER], undefined, '恢复可见+聚焦后照常当已读')
})

test('四种通知决策分支都留一行日志（key / 包号 / 时间 / 长度，不含正文）', async () => {
  await h.emit('window-visibility', { visible: false })
  // 1) notified
  await h.emit('msg-in', { key: PEER, msg: inRecord({ text: '隐私正文' }), resend: false })
  // 2) skipped:rebroadcast（同包号重投）
  await h.emit('msg-in', { key: PEER, msg: inRecord({ text: '隐私正文' }), resend: false })
  // 3) skipped:chat-visible
  await h.emit('window-visibility', { visible: true })
  await h.emit('tauri://focus', undefined)
  await h.emit('msg-in', { key: PEER, msg: inRecord({ pkt: 102 }), resend: false })
  // 4) skipped:no-msg（载荷缺消息体）
  await h.emit('msg-in', { key: PEER })

  await h.waitUntil(() => {
    const lines = notifyLines()
    return ['notified', 'skipped:rebroadcast', 'skipped:chat-visible', 'skipped:no-msg']
      .every((d) => lines.some((l) => l.startsWith(d)))
  })
  const lines = notifyLines()
  const notified = lines.find((l) => l.startsWith('notified'))
  assert.match(notified, new RegExp(`key=${PEER} pkt=101 ts=1700000000 len=4`))
  // 隐私：正文不进日志
  assert.ok(!lines.some((l) => l.includes('隐私正文')), '正文不许进诊断日志')
})

test('通知 IPC 失败不吞：重试一次、记 failed、消息仍然算未读', async () => {
  h.backend.notify_message = () => { throw new Error('dbus down') }
  await h.emit('window-visibility', { visible: false })

  await h.emit('msg-in', { key: PEER, msg: inRecord(), resend: false })

  await h.waitUntil(() => h.countOf('notify_message') === 2, { ticks: 400 })
  assert.equal(h.countOf('notify_message'), 2, '失败后只重试一次（共两次调用）')
  const lines = notifyLines()
  assert.ok(lines.some((l) => l.startsWith('notify-ipc-retry key=' + PEER)), '第一次失败留痕')
  assert.ok(lines.some((l) => l.startsWith('notify-ipc-failed key=' + PEER)), '两次都失败也要留痕')
  assert.equal(h.store.unread[PEER], 1, '通知失败不影响未读记账（消息不会凭空消失）')
})
