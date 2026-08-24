// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { composeReplyBody, quotePreview } from '../src/lib/reply.js'

test('composeReplyBody 组装引用+回复为发送正文', () => {
  assert.equal(
    composeReplyBody('明天下午开会', '收到'),
    '「明天下午开会」\n收到',
  )
})

test('引用原文里的多行/空白被压实为单行', () => {
  assert.equal(
    composeReplyBody('第一行\n第二行   加空格', 'ok'),
    '「第一行 第二行 加空格」\nok',
  )
})

test('引用为空时不加空引用块', () => {
  assert.equal(composeReplyBody('', 'ok'), 'ok')
  assert.equal(composeReplyBody(undefined, 'ok'), 'ok')
})

test('quotePreview：文本消息取正文并截断为单行摘要', () => {
  const long = '这是一条很长很长的消息内容用来验证摘要截断逻辑是否正确工作啊同志们'
  assert.ok(long.length > 30, '测试样本必须超过截断阈值')
  const p = quotePreview({ kind: 'text', text: long })
  assert.ok(p.startsWith('这是一条很长很长的消息内容用来验证摘要'))
  assert.ok(p.endsWith('…'), '超长时以省略号结尾')
  assert.ok(!p.includes('\n'), '摘要必须是单行')
  assert.ok(p.length <= 31, '摘要长度有上限')
})

test('quotePreview：文本里的空白被压实', () => {
  assert.equal(quotePreview({ kind: 'text', text: 'a\nb  c' }), 'a b c')
})

test('quotePreview：文件/图片消息用 [文件] 文件名', () => {
  assert.equal(
    quotePreview({ kind: 'file', files: [{ name: '报告.docx' }] }),
    '[文件] 报告.docx',
  )
  assert.equal(
    quotePreview({ kind: 'file', files: [] }),
    '[文件]',
  )
})

test('quotePreview：无内容时返回空串', () => {
  assert.equal(quotePreview(undefined), '')
  assert.equal(quotePreview({}), '')
})