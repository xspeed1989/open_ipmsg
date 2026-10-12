// node --test scripts/ —— 接收图片「永远停在下载中」的回归用例
//
// 真实故障（2026-09，两端均为本客户端，局域网，加密开启）：
// 对端发剪贴板图片，接收方界面有卡片但一直显示「下载中」，图片出不来；
// 再发一次就好了，非必现。
//
// 时序根因：后端收下附件后先 emit msg-in（记录 state=downloading）、随即自动
// 下载并在完成后 emit file-progress(done=true, path=...)。前端 msg-in 处理是
// async —— 该会话尚未加载时要先 await getHistory() 才把消息插进 msgs，而
// file-progress 撞进这个窗口时找不到消息，完成事件被永久丢弃：卡片停在「下载中」，
// 直到重启（重读历史）才恢复。
//
// 修复：落不下去的进度事件先按 key/pkt/file_id 暂存，等消息真正入列后回放。
//
// 这里只测这个 module 自己的数学。**端到端时序**（msg-in 等 getHistory 期间完成事件
// 到达，图片仍然显示出来）已搬到 scripts/store-events.characterization.test.mjs：
// 那边跨的是真 store + mockIPC，不再是正则抠函数体。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  applyFileProgress,
  bufferFileProgress,
  replayFileProgress,
  PROGRESS_BUFFER_MAX,
} from '../src/lib/fileprogress.js'

/** 后端落库的入站记录快照：自动接收的附件此刻是 downloading、还没有 path */
const incomingMsg = (over = {}) => ({
  dir: 'in',
  kind: 'file',
  pkt: 42,
  ts: 1_700_000_000,
  peer: { key: '10.0.0.9', ip: '10.0.0.9' },
  files: [{ id: 7, name: '剪贴板图片.png', size: 1234, state: 'downloading' }],
  ...over,
})

const doneEvent = (over = {}) => ({
  key: '10.0.0.9',
  pkt: 42,
  file_id: 7,
  transferred: 1234,
  total: 1234,
  done: true,
  path: '/dl/剪贴板图片.png',
  ...over,
})

const progressEvent = (over = {}) => ({
  key: '10.0.0.9',
  pkt: 42,
  file_id: 7,
  transferred: 400,
  total: 1234,
  done: false,
  ...over,
})

test('完成事件先于消息入列：暂存后回放，卡片不再停在「下载中」', () => {
  const buffer = new Map()
  const chat = { msgs: [] } // msg-in 还在等 getHistory，这条消息不在内存里
  const ev = doneEvent()

  assert.equal(applyFileProgress(chat, ev), false, '消息未入列时落不下去')
  bufferFileProgress(buffer, ev)

  chat.msgs.push(incomingMsg()) // getHistory 返回，消息此刻才入列
  assert.equal(replayFileProgress(buffer, chat, '10.0.0.9'), 1)

  const f = chat.msgs[0].files[0]
  assert.equal(f.state, 'done', '图片必须变成已完成，而不是停在下载中')
  assert.equal(f.path, '/dl/剪贴板图片.png', '完成事件里的落盘路径要写进卡片')
  assert.equal(buffer.size, 0, '命中后应从暂存里移除')
})

test('消息先入列时直接落地，不产生暂存', () => {
  const buffer = new Map()
  const chat = { msgs: [incomingMsg()] }

  assert.equal(applyFileProgress(chat, doneEvent()), true)
  assert.equal(chat.msgs[0].files[0].state, 'done')
  assert.equal(buffer.size, 0)
})

test('只回放同一会话的暂存事件', () => {
  const buffer = new Map()
  const chat = { msgs: [incomingMsg()] }
  bufferFileProgress(buffer, doneEvent({ key: '10.0.0.8' }))

  assert.equal(replayFileProgress(buffer, chat, '10.0.0.9'), 0, '别的会话的事件不能套到本会话上')
  assert.equal(buffer.size, 1, '不匹配的事件留在暂存里等对应消息')

  bufferFileProgress(buffer, doneEvent())
  assert.equal(replayFileProgress(buffer, chat, '10.0.0.9'), 1)
  assert.equal(chat.msgs[0].files[0].state, 'done')
  assert.equal(buffer.size, 1, '别的会话那条仍留着')
})

test('附件 ID 对不上时不乱写，等真正入列后再回放', () => {
  const buffer = new Map()
  const chat = { msgs: [incomingMsg()] }
  bufferFileProgress(buffer, doneEvent({ file_id: 99 }))

  assert.equal(replayFileProgress(buffer, chat, '10.0.0.9'), 0)
  assert.equal(chat.msgs[0].files[0].state, 'downloading', '不能把别的附件的完成态套上来')
  assert.equal(buffer.size, 1)
})

test('同一附件的多条进度只保留最新一条', () => {
  const buffer = new Map()
  bufferFileProgress(buffer, progressEvent({ transferred: 100 }))
  bufferFileProgress(buffer, progressEvent({ transferred: 900 }))
  bufferFileProgress(buffer, doneEvent())

  assert.equal(buffer.size, 1, '同一 key/pkt/file_id 只有一条')

  const chat = { msgs: [incomingMsg()] }
  assert.equal(replayFileProgress(buffer, chat, '10.0.0.9'), 1)
  const f = chat.msgs[0].files[0]
  assert.equal(f.state, 'done')
  assert.equal(f.transferred, 1234, '回放的是最后一条（完成），不是中途进度')
})

test('暂存窗口有上限：永不匹配的事件不会无限堆积', () => {
  const buffer = new Map()
  for (let i = 0; i < PROGRESS_BUFFER_MAX + 10; i++) {
    bufferFileProgress(buffer, doneEvent({ pkt: 1000 + i }))
  }
  assert.equal(buffer.size, PROGRESS_BUFFER_MAX, '超出上限后淘汰最旧的')
})

test('失败态的 error 落进卡片，后续 done 覆盖为已完成', () => {
  const chat = { msgs: [incomingMsg()] }
  assert.equal(applyFileProgress(chat, { ...doneEvent(), done: false, error: '传输中断' }), true)
  const f = chat.msgs[0].files[0]
  assert.equal(f.state, 'failed')
  assert.equal(f.error, '传输中断')

  assert.equal(applyFileProgress(chat, doneEvent()), true)
  assert.equal(f.state, 'done', '重试成功后必须回到已完成')
  assert.equal(f.path, '/dl/剪贴板图片.png')
})

test('进度事件的 total 缺失时退回附件公告的大小', () => {
  const chat = { msgs: [incomingMsg()] }
  applyFileProgress(chat, progressEvent({ total: 0 }))
  assert.equal(chat.msgs[0].files[0].total, 1234)
})

