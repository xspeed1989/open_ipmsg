// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { unreadReceiptPkts, deliveryState } from '../src/lib/receipts.js'

const storeSource = readFileSync(new URL('../src/store.js', import.meta.url), 'utf8')
const chatSource = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')
const ipcSource = readFileSync(new URL('../src/lib/ipc.js', import.meta.url), 'utf8')
const i18nSource = readFileSync(new URL('../src/lib/i18n.js', import.meta.url), 'utf8')

test('挑出「要求回执且仍未读」的出站消息包号', () => {
  const msgs = [
    { dir: 'out', pkt: 101, rcpt: true, read: false },
    { dir: 'out', pkt: 102, rcpt: true, read: true },
    { dir: 'in', pkt: 103, rcpt: true, read: false },
    { dir: 'out', pkt: 104, rcpt: false, read: false }, // 文件消息不带 rcpt
    { dir: 'out', pkt: 105, rcpt: true, read: false },
  ]
  assert.deepEqual(unreadReceiptPkts(msgs), [101, 105])
})

test('空会话与全部已读都返回空数组', () => {
  assert.deepEqual(unreadReceiptPkts([]), [])
  assert.deepEqual(unreadReceiptPkts([{ dir: 'out', pkt: 1, rcpt: true, read: true }]), [])
  assert.deepEqual(unreadReceiptPkts(undefined), [])
})

test('rcpt 字段缺失的旧记录不参与（避免误标）', () => {
  const msgs = [{ dir: 'out', pkt: 9, read: false }]
  assert.deepEqual(unreadReceiptPkts(msgs), [])
})

/* ---------- 出站气泡的送达状态：未读 / 已送达 / 已读 ----------
   官方 Windows 客户端对普通消息只回 RECVMSG、从不回 READMSG（recvdlg.cpp 的
   READMSG 只在封书开封时发）。没有「已送达」这个中间态，气泡就一直挂在未读。 */

test('已送达是独立于已读的中间态', () => {
  assert.equal(deliveryState({ dir: 'out', rcpt: true, read: false, delivered: false }), 'unread')
  assert.equal(deliveryState({ dir: 'out', rcpt: true, read: false, delivered: true }), 'delivered')
  assert.equal(deliveryState({ dir: 'out', rcpt: true, read: true, delivered: true }), 'read')
  assert.equal(
    deliveryState({ dir: 'out', rcpt: true, read: true }),
    'read',
    '已读是更强的状态：delivered 缺失（READMSG 先到 / 老记录）也显示已读',
  )
})

test('不请求回执、离线排队中、入站消息都不显示状态标签', () => {
  assert.equal(deliveryState({ dir: 'out', rcpt: false, read: false }), null, '文件消息不带回执')
  assert.equal(
    deliveryState({ dir: 'out', rcpt: true, read: false, queued: true }),
    null,
    '离线排队中显示「离线留言」而不是未读',
  )
  assert.equal(deliveryState({ dir: 'in', rcpt: true, read: false }), null)
  assert.equal(deliveryState({ dir: 'out', read: false }), null, 'rcpt 缺失的旧记录')
  assert.equal(deliveryState(undefined), null)
})

test('气泡标签直接用 deliveryState 的返回值当 i18n 后缀与 CSS 类', () => {
  const span = chatSource.match(/<span v-if="deliveryState\(v\.m\)"[\s\S]*?<\/span>/)
  assert.ok(span, '气泡要改用 deliveryState 决定标签')
  assert.match(span[0], /:class="deliveryState\(v\.m\)"/)
  assert.match(span[0], /t\([`'"]chat\.\$\{deliveryState\(v\.m\)\}[`'"]\)/)
  for (const key of ['chat.read', 'chat.unread', 'chat.delivered']) {
    const hits = i18nSource.split(`'${key}'`).length - 1
    assert.ok(hits >= 2, `${key} 中英文案都要有（当前 ${hits} 处）`)
  }
})

test('msg-delivered 事件常量与订阅都在', async () => {
  // 取值不再手抄：EVT 的每个后端事件都指向 wire 产物里的同一个名字
  const { EVT } = await import('../src/lib/ipc.js')
  const { WIRE } = await import('../src/lib/wire.generated.js')
  assert.equal(EVT.msgDelivered, WIRE.evt['msg-delivered'])
  assert.equal(EVT.msgIn, WIRE.evt['msg-in'])
  assert.equal(EVT.fileProgress, WIRE.evt['file-progress'])
  assert.match(storeSource, /ipc\.EVT\.msgDelivered/)
})

// 「收到 msg-delivered 把出站气泡翻成已送达」的旧用例靠正则抠 onMsgDelivered 的函数体，
// 已退役：该函数搬进 lib/transitions.js，行为改由
// scripts/store-events.characterization.test.mjs 跨真 store 的 msg-delivered 用例守着。
// （保留这条注释是为了让下一个看到「少了一条用例」的人知道它是被替掉的，不是丢了。）
