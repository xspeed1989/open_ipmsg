// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { unreadReceiptPkts } from '../src/lib/receipts.js'

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
