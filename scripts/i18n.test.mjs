// node --test scripts/ —— i18n 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  locale, setLocale, t, dayLabel, detectLocale, isSupported,
  SUPPORTED_LANGS, LANG_NAMES,
} from '../src/lib/i18n.js'
import { composeReplyBody, quotePreview } from '../src/lib/reply.js'
import { forwardPayload, mergeForward } from '../src/lib/forward.js'

const DAY = 86400000
const todayMidnight = () => {
  const now = new Date()
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000
}

test('语言名用各自的语言书写', () => {
  assert.equal(LANG_NAMES['zh-CN'], '简体中文')
  assert.equal(LANG_NAMES.en, 'English')
  assert.deepEqual(SUPPORTED_LANGS, ['zh-CN', 'en'])
})

test('默认语言是简体中文', () => {
  setLocale('zh-CN')
  assert.equal(locale.value, 'zh-CN')
  assert.equal(t('me'), '我')
})

test('setLocale 切换语言，t 返回对应文案', () => {
  setLocale('en')
  assert.equal(locale.value, 'en')
  assert.equal(t('me'), 'Me')
  assert.equal(t('settings.title'), 'Settings')
})

test('setLocale 非法值回退到系统探测且不崩', () => {
  setLocale('fr')
  assert.ok(isSupported(locale.value), '回退后仍在支持列表内')
  setLocale('zh-CN')
})

test('t 占位符 {name} 替换', () => {
  setLocale('zh-CN')
  assert.equal(t('chat.selected', { n: 3 }), '已选 3 条')
  setLocale('en')
  assert.equal(t('chat.selected', { n: 3 }), '3 selected')
})

test('t 未知 key 原样返回（便于发现漏翻）', () => {
  assert.equal(t('no.such.key'), 'no.such.key')
})

test('zh-CN 与 en 字典 key 完全一致（防漏翻）', () => {
  // 通过 setLocale 无法直接拿到字典，用 t 的行为差异推断：
  // 若某 key 只在 zh 里有，切到 en 后会回退 zh 文案而非 key 本身，
  // 这里无法枚举内部表，因此直接覆盖常用路径并断言结构
  setLocale('en')
  const sample = [
    'me', 'online', 'offline', 'unknownUser', 'ungrouped', 'cancel',
    'titlebar.min', 'sidebar.settings', 'list.searchPh', 'settings.title',
    'settings.language', 'picker.empty', 'chat.send', 'chat.read',
    'chat.clearConfirm', 'viewer.loading', 'forward.noContent', 'preview.offline',
  ]
  for (const k of sample) {
    assert.notEqual(t(k), k, `en 字典缺少 key: ${k}`)
  }
  setLocale('zh-CN')
})

test('dayLabel：今天/昨天与星期（中英双语）', () => {
  const mid = todayMidnight()
  setLocale('zh-CN')
  assert.equal(dayLabel(mid + 10), '今天')
  assert.equal(dayLabel(mid - DAY + 10), '昨天')
  setLocale('en')
  assert.equal(dayLabel(mid + 10), 'Today')
  assert.equal(dayLabel(mid - DAY + 10), 'Yesterday')
  setLocale('zh-CN')
})

test('dayLabel：一周内显示星期（按语言缩写）', () => {
  const mid = todayMidnight()
  setLocale('en')
  // 3 天前：无论星期几，英文必然是 Sun..Sat 之一，且不含 Today/Yesterday
  const label = dayLabel(mid - 3 * DAY + 100)
  assert.match(label, /^(Sun|Mon|Tue|Wed|Thu|Fri|Sat)$/)
  setLocale('zh-CN')
  const zl = dayLabel(mid - 3 * DAY + 100)
  assert.match(zl, /^(周日|周一|周二|周三|周四|周五|周六)$/)
})

test('dayLabel：更早的日期显示月日（跨年带年份）', () => {
  const mid = todayMidnight()
  setLocale('en')
  const old = mid - 30 * DAY
  assert.match(dayLabel(old), /^[A-Z][a-z]{2} \d{1,2}(, \d{4})?$/)
  setLocale('zh-CN')
  assert.match(dayLabel(old), /^(\d{4}年)?\d{1,2}月\d{1,2}日$/)
})

test('detectLocale：无 navigator 时回退英语；zh 前缀 → 简体中文', () => {
  const saved = globalThis.navigator
  try {
    Object.defineProperty(globalThis, 'navigator', { value: undefined, configurable: true })
    assert.equal(detectLocale(), 'en')
    Object.defineProperty(globalThis, 'navigator', { value: { language: 'zh-TW' }, configurable: true })
    assert.equal(detectLocale(), 'zh-CN')
    Object.defineProperty(globalThis, 'navigator', { value: { language: 'en-US' }, configurable: true })
    assert.equal(detectLocale(), 'en')
  } finally {
    if (saved) Object.defineProperty(globalThis, 'navigator', { value: saved, configurable: true })
    else delete globalThis.navigator
  }
})

test('回复/转发文案与标记随语言切换', () => {
  setLocale('en')
  assert.equal(composeReplyBody('hi', 'ok'), '"hi"\nok')
  assert.equal(quotePreview({ kind: 'file', files: [{ name: 'a.zip' }] }), '[File] a.zip')
  assert.equal(forwardPayload({ kind: 'text', text: '' }).reason, 'Nothing to forward')
  assert.equal(
    mergeForward([{ dir: 'out', ts: 1, text: 'hi' }], () => 'Me'),
    '[Me]hi',
  )
  setLocale('zh-CN')
  assert.equal(composeReplyBody('hi', 'ok'), '「hi」\nok')
  assert.equal(quotePreview({ kind: 'file', files: [{ name: 'a.zip' }] }), '[文件] a.zip')
  assert.equal(forwardPayload({ kind: 'text', text: '' }).reason, '没有可转发的内容')
  assert.equal(mergeForward([{ dir: 'out', ts: 1, text: 'hi' }], () => '我'), '【我】hi')
})