// node --test scripts/ —— i18n 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import {
  locale, setLocale, t, dayLabel, detectLocale, isSupported,
  SUPPORTED_LANGS, LANG_NAMES,
} from '../src/lib/i18n.js'
import { composeReplyBody, quotePreview } from '../src/lib/reply.js'
import { forwardPayload, mergeForward } from '../src/lib/forward.js'

const settingsSource = readFileSync(
  new URL('../src/components/SettingsModal.vue', import.meta.url),
  'utf8'
)
const i18nSource = readFileSync(new URL('../src/lib/i18n.js', import.meta.url), 'utf8')

// dayLabel 的时间戳是「秒」，与 Date.getTime() 的「毫秒」相差 1000 倍
const MS = 1000
const DAY = 86400
const todayMidnight = () => {
  const now = new Date()
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / MS
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
  // 通过 setLocale 拿不到内部表，但源码就摆在旁边：直接解析两个字典字面量，
  // 全量比对 key 集合。曾经 18 个协议扩展项的英文被中文覆盖而 en 又没有定义，
  // 英文界面于是静默显示中文（不是显示 key，肉眼极难发现）。
  const keysOf = (name) => {
    const body = i18nSource.match(new RegExp(`const ${name} = \\{([\\s\\S]*?)\\n\\}`))?.[1]
    assert.ok(body, `应从源码里解析出 ${name} 字典`)
    return new Set([...body.matchAll(/^\s*'([^']+)':/gm)].map((m) => m[1]))
  }
  const zhKeys = keysOf('zh')
  const enKeys = keysOf('en')
  assert.ok(zhKeys.size > 200, `zh 字典 key 数量异常：${zhKeys.size}`)
  assert.deepEqual(
    [...zhKeys].filter((k) => !enKeys.has(k)).sort(),
    [],
    'en 字典缺这些 key：英文界面会回退成中文'
  )
  assert.deepEqual(
    [...enKeys].filter((k) => !zhKeys.has(k)).sort(),
    [],
    'zh 字典缺这些 key：中文界面会回退成英文或显示 key 本身'
  )
})

test('「自动打开封书」开关的文案中英都有，不漏翻', () => {
  setLocale('zh-CN')
  assert.equal(t('settings.autoOpenSecret'), '自动打开封书')
  assert.notEqual(
    t('settings.autoOpenSecretHint'),
    'settings.autoOpenSecretHint',
    'zh 字典缺少说明文案'
  )
  setLocale('en')
  // 这里必须断言具体英文文案：t() 在 en 缺 key 时会回退到 zh 字典，
  // 只断言「不等于 key 本身」会把「英文界面显示中文」当成通过
  assert.equal(t('settings.autoOpenSecret'), 'Open sealed messages automatically')
  assert.match(
    t('settings.autoOpenSecretHint'),
    /^Show incoming sealed messages/,
    'en 字典缺说明文案（否则英文界面会回退成中文）'
  )
  setLocale('zh-CN')
})

test('设置页用到的每个 settings.* key 在英文下都不是中文（t() 缺 key 会回退 zh）', () => {
  // t() 的回退链是 dict[key] ?? zh[key] ?? key：en 缺 key 时英文界面会静默显示
  // 中文（不是显示 key 本身，肉眼很难发现）。这里把设置页用到的 key 全量枚举出来，
  // 逐个断言英文侧真的有译文 —— 曾经 18 个协议扩展项就是这样漏掉的。
  const keys = [
    ...new Set(
      [...settingsSource.matchAll(/t\(\s*'(settings\.[A-Za-z0-9_]+)'/g)].map((m) => m[1])
    ),
  ]
  assert.ok(keys.length >= 60, `应从设置页提取到全部文案 key，实际只拿到 ${keys.length} 个`)

  const CJK = /[\u4e00-\u9fff]/
  const leaks = []
  for (const key of keys) {
    setLocale('zh-CN')
    const zhText = t(key)
    if (zhText === key) leaks.push(`${key}: zh 缺 key`)
    setLocale('en')
    const enText = t(key)
    if (enText === key) leaks.push(`${key}: en 缺 key（界面会显示 key 本身）`)
    else if (CJK.test(enText)) leaks.push(`${key}: en 回退成中文「${enText.slice(0, 20)}…」`)
  }
  setLocale('zh-CN')
  assert.deepEqual(leaks, [], `设置页英文文案漏翻：\n  ${leaks.join('\n  ')}`)
})

test('界面文案必须走 t()：模板里不得硬编码中文', () => {
  // 硬编码的中文既不进字典、也不受语言切换控制 —— 英文界面就会直接露出中文。
  // 设置页的「IPMsg 协议扩展」标题与 agent 地址的「留空关闭」占位符就是这么漏的：
  // 它们没有 t() 调用，所以「按 key 查译文」的测试永远看不到它们。
  const files = [
    new URL('../src/App.vue', import.meta.url),
    ...readdirSync(new URL('../src/components/', import.meta.url))
      .filter((f) => f.endsWith('.vue'))
      .map((f) => new URL(`../src/components/${f}`, import.meta.url)),
  ]
  const CJK = /[\u4e00-\u9fff]/
  const leaks = []
  for (const url of files) {
    const body = readFileSync(url, 'utf8').match(/<template>([\s\S]*?)\n<\/template>/)?.[1]
    if (!body) continue
    const rendered = body
      .replace(/<!--[\s\S]*?-->/g, '') // 注释不渲染
      .replace(/\{\{[^}]*\}\}/g, '') // 插值里是 t(...) 调用
    for (const line of rendered.split('\n')) {
      if (CJK.test(line)) {
        leaks.push(`${url.pathname.split('/src/')[1]}: ${line.trim().slice(0, 70)}`)
      }
    }
  }
  assert.deepEqual(
    leaks,
    [],
    `模板里的硬编码中文（英文界面会露出中文，请改用 t()）:\n  ${leaks.join('\n  ')}`
  )
})

test('dayLabel：今天显示「今天 / Today」', () => {
  const mid = todayMidnight()
  setLocale('zh-CN')
  assert.equal(dayLabel(mid), '今天')
  assert.equal(dayLabel(mid + 10), '今天')
  setLocale('en')
  assert.equal(dayLabel(mid), 'Today')
  assert.equal(dayLabel(mid + 10), 'Today')
  setLocale('zh-CN')
})

test('dayLabel：今天以前一律显示日期，不再误标昨天/星期', () => {
  const mid = todayMidnight()
  const zhDate = /^(\d{4}年)?\d{1,2}月\d{1,2}日$/
  setLocale('zh-CN')
  // 昨天 23:59:59（差一秒）/ 3 天前 / 30 天前 / 400 天前：都必须是日期
  for (const ts of [mid - 1, mid - 3 * DAY + 100, mid - 30 * DAY, mid - 400 * DAY]) {
    const label = dayLabel(ts)
    assert.match(label, zhDate, `ts=${ts} 应显示日期，实际：${label}`)
    assert.notEqual(label, '昨天')
    assert.notEqual(label, '今天')
  }
  setLocale('en')
  const enDate = /^[A-Z][a-z]{2} \d{1,2}(, \d{4})?$/
  for (const ts of [mid - 1, mid - 3 * DAY + 100, mid - 30 * DAY, mid - 400 * DAY]) {
    const label = dayLabel(ts)
    assert.match(label, enDate, `ts=${ts} 应显示日期，实际：${label}`)
    assert.notEqual(label, 'Yesterday')
    assert.notEqual(label, 'Today')
  }
  setLocale('zh-CN')
})

test('dayLabel：同年显示月日，跨年带年份（中英）', () => {
  const now = new Date()
  const y = now.getFullYear()
  const todayStart = new Date(y, now.getMonth(), now.getDate()).getTime()
  // 同年：当年 1 月 15 日（今天若还没到 1/15 则跳过，避免把今天当过去）
  const jan15 = new Date(y, 0, 15, 12)
  if (jan15.getTime() < todayStart) {
    setLocale('zh-CN')
    assert.equal(dayLabel(jan15.getTime() / MS), '1月15日')
    setLocale('en')
    assert.equal(dayLabel(jan15.getTime() / MS), 'Jan 15')
  }
  // 跨年：去年 6 月 15 日
  const prev = new Date(y - 1, 5, 15, 12)
  setLocale('zh-CN')
  assert.equal(dayLabel(prev.getTime() / MS), `${y - 1}年6月15日`)
  setLocale('en')
  assert.equal(dayLabel(prev.getTime() / MS), `Jun 15, ${y - 1}`)
  setLocale('zh-CN')
})

test('dayLabel：无时间戳返回空串', () => {
  assert.equal(dayLabel(0), '')
  assert.equal(dayLabel(undefined), '')
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