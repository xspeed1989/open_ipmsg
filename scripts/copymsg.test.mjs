// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { copyTextOf } from '../src/lib/copymsg.js'

test('文本消息：复制正文原样', () => {
  assert.equal(copyTextOf({ kind: 'text', text: '明天下午开会\n带上纪要' }), '明天下午开会\n带上纪要')
})

test('正文两端空白先裁掉', () => {
  assert.equal(copyTextOf({ kind: 'text', text: '  hi  \n ' }), 'hi')
})

test('文件消息带正文时复制正文（附件名单独复制意义不大）', () => {
  const m = {
    kind: 'file',
    text: '资料在这里',
    files: [{ name: 'a.zip' }, { name: 'b.pdf' }],
  }
  assert.equal(copyTextOf(m), '资料在这里')
})

test('文件消息无正文时逐行复制附件名', () => {
  const m = {
    kind: 'file',
    text: ' ',
    files: [{ name: 'a.zip' }, { name: 'b.pdf' }],
  }
  assert.equal(copyTextOf(m), 'a.zip\nb.pdf')
})

test('空消息返回空串，调用方据此提示不可复制', () => {
  assert.equal(copyTextOf({ kind: 'text', text: '' }), '')
  assert.equal(copyTextOf({ kind: 'file', files: [] }), '')
  assert.equal(copyTextOf(null), '')
})
