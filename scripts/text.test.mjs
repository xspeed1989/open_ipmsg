// node --test scripts/  —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { splitDelayedNote } from '../src/lib/text.js'

test('剥离真实抓包里的「延迟发送」尾注', () => {
  // 以下样本取自 diag.log 中 DESKTOP-D4D3HL5 实际发来的报文
  assert.deepEqual(splitDelayedNote('123\n----\n(IPMsg Delayed Send: 08/22 15:02 )'), {
    body: '123',
    delayed: '08/22 15:02',
  })
  assert.deepEqual(splitDelayedNote('23\n----\n(IPMsg Delayed Send: 19:18 )'), {
    body: '23',
    delayed: '19:18',
  })
  // 带附件的消息正文为空，报文以换行开头
  assert.deepEqual(splitDelayedNote('\n----\n(IPMsg Delayed Send: 08/22 23:55 )'), {
    body: '',
    delayed: '08/22 23:55',
  })
})

test('普通消息原样返回', () => {
  assert.deepEqual(splitDelayedNote('你好'), { body: '你好', delayed: null })
  assert.deepEqual(splitDelayedNote(''), { body: '', delayed: null })
  assert.deepEqual(splitDelayedNote(undefined), { body: '', delayed: null })
})

test('正文里的分隔线与括号不会被误伤', () => {
  // 用户自己打的分隔线：后面没有尾注，必须完整保留
  const t = '第一段\n----\n第二段'
  assert.deepEqual(splitDelayedNote(t), { body: t, delayed: null })
  // 尾注只认结尾处的那一段，正文中间出现的原样保留
  const t2 = '(IPMsg Delayed Send: 1/1 0:00 ) 这句是正文'
  assert.deepEqual(splitDelayedNote(t2), { body: t2, delayed: null })
  // 多行正文 + 尾注
  assert.deepEqual(splitDelayedNote('a\nb\n----\n(IPMsg Delayed Send: 08/22 20:00 )'), {
    body: 'a\nb',
    delayed: '08/22 20:00',
  })
})

import { parseFileUris } from '../src/lib/text.js'

test('解析文件管理器复制的 file:// URI', () => {
  assert.deepEqual(parseFileUris('file:///home/allen/a.txt'), ['/home/allen/a.txt'])
  // 多个文件 + GNOME 的动作首行
  assert.deepEqual(parseFileUris('copy\nfile:///tmp/a%20b.png\nfile:///tmp/%E5%9B%BE.jpg'), [
    '/tmp/a b.png',
    '/tmp/图.jpg',
  ])
  // Windows 形式
  assert.deepEqual(parseFileUris('file:///C:/Users/x/a.doc'), ['C:/Users/x/a.doc'])
  assert.deepEqual(parseFileUris('file://localhost/tmp/a.txt'), ['/tmp/a.txt'])
})

test('普通文本不会被当成文件路径', () => {
  assert.deepEqual(parseFileUris('你好'), [])
  assert.deepEqual(parseFileUris('/home/allen/a.txt'), []) // 裸路径不算
  assert.deepEqual(parseFileUris('file:///tmp/a.txt\n这是一段说明'), []) // 混杂文本不算
  assert.deepEqual(parseFileUris(''), [])
  assert.deepEqual(parseFileUris(undefined), [])
})

import { pickLatestUnread } from '../src/lib/unread.js'

test('托盘唤起时挑最新的未读会话', () => {
  // 取时间戳最大的未读会话
  assert.equal(
    pickLatestUnread({ a: 1, b: 2, c: 3 }, { a: 100, b: 300, c: 200 }, {}),
    'b'
  )
  // 未读数为 0 的会话不参与，哪怕时间更新
  assert.equal(pickLatestUnread({ a: 1, b: 0 }, { a: 100, b: 999 }, {}), 'a')
  // 缺 unreadTs 时退化用 lastTs
  assert.equal(pickLatestUnread({ a: 1, b: 1 }, {}, { a: 5, b: 9 }), 'b')
  // 两者都缺按 0，仍要返回一个未读会话而不是空
  assert.equal(pickLatestUnread({ a: 1 }, {}, {}), 'a')
})

test('没有未读时不跳转', () => {
  assert.equal(pickLatestUnread({}, {}, {}), '')
  assert.equal(pickLatestUnread({ a: 0, b: 0 }, { a: 9 }, {}), '')
  assert.equal(pickLatestUnread(), '')
})

import { applyTheme } from '../src/lib/theme.js'

test('主题应用到 <html data-theme>', () => {
  // 用最小替身模拟 documentElement
  const el = {
    attrs: {},
    setAttribute(k, v) { this.attrs[k] = v },
    removeAttribute(k) { delete this.attrs[k] },
  }
  assert.equal(applyTheme('dark', el), 'dark')
  assert.equal(el.attrs['data-theme'], 'dark')
  assert.equal(applyTheme('light', el), 'light')
  assert.equal(el.attrs['data-theme'], 'light')
  // 跟随系统时必须移除属性，交给 CSS 的 prefers-color-scheme
  assert.equal(applyTheme('system', el), 'system')
  assert.equal(el.attrs['data-theme'], undefined)
  // 非法值/缺省一律按跟随系统处理
  assert.equal(applyTheme(undefined, el), 'system')
  assert.equal(applyTheme('rainbow', el), 'system')
  assert.equal(el.attrs['data-theme'], undefined)
})

import { highlightParts, makeSnippet } from '../src/lib/text.js'

test('关键词高亮切片', () => {
  assert.deepEqual(highlightParts('明天开会开心', '开'), [
    { text: '明天', hit: false },
    { text: '开', hit: true },
    { text: '会', hit: false },
    { text: '开', hit: true },
    { text: '心', hit: false },
  ])
  // 大小写不敏感，且保留原文大小写
  assert.deepEqual(highlightParts('Hello World', 'hello'), [
    { text: 'Hello', hit: true },
    { text: ' World', hit: false },
  ])
  // 空关键词/空文本
  assert.deepEqual(highlightParts('abc', ''), [{ text: 'abc', hit: false }])
  assert.deepEqual(highlightParts('', 'x'), [{ text: '', hit: false }])
  // 不命中
  assert.deepEqual(highlightParts('abc', 'z'), [{ text: 'abc', hit: false }])
})

test('搜索结果摘要截取到命中位置', () => {
  const long = '前'.repeat(40) + '关键词' + '后'.repeat(40)
  const s = makeSnippet(long, '关键词', 5)
  assert.ok(s.includes('关键词'))
  assert.ok(s.startsWith('…') && s.endsWith('…'), '两端都该有省略号')
  assert.ok(s.length < 20, `摘要应该很短，实际 ${s.length}`)
  // 命中在开头时不加前省略号
  assert.ok(!makeSnippet('关键词在开头', '关键词', 5).startsWith('…'))
  // 多余空白折叠
  assert.equal(makeSnippet('a   b', 'a', 5), 'a b')
})
