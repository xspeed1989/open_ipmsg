// node --test scripts/ —— 后端错误本地化单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import { setLocale, t } from '../src/lib/i18n.js'
import { describeError, ERROR_CODES } from '../src/lib/errors.js'

const CJK = /[\u4e00-\u9fff]/

test('带错误码的后端错误按码取译文（中英各一份）', () => {
  setLocale('zh-CN')
  assert.equal(describeError('E_UNLOCK_BAD_PASSWORD|密码错误'), '密码错误')
  setLocale('en')
  assert.equal(describeError('E_UNLOCK_BAD_PASSWORD|密码错误'), 'Wrong password')
  setLocale('zh-CN')
})

test('带细节占位符的码：细节填进译文，不再露出中文前缀', () => {
  setLocale('en')
  const msg = describeError('E_READ_FAILED|No such file or directory')
  assert.equal(msg, 'Read failed: No such file or directory')
  assert.ok(!CJK.test(msg))
  setLocale('zh-CN')
  assert.equal(describeError('E_READ_FAILED|No such file'), '读取失败：No such file')
})

test('没法识别的码：走兜底且不把码本身显示给用户', () => {
  setLocale('en')
  const msg = describeError('E_SOMETHING_NEW|内部细节')
  assert.ok(!msg.includes('E_SOMETHING_NEW'), `不该把错误码甩给用户：${msg}`)
  assert.ok(!CJK.test(msg), `英文界面不该出现中文：${msg}`)
  setLocale('zh-CN')
})

test('没有码的中文错误：英文界面不透出中文，中文界面保留原文', () => {
  setLocale('en')
  const en = describeError('找不到该消息')
  assert.ok(!CJK.test(en), `英文界面不该出现中文：${en}`)
  assert.notEqual(en, '找不到该消息')

  setLocale('zh-CN')
  const zh = describeError('找不到该消息')
  assert.ok(zh.includes('找不到该消息'), `中文界面应保留原文便于排查：${zh}`)
})

test('没有码的英文错误：两种语言都保留细节', () => {
  setLocale('en')
  assert.ok(describeError('permission denied').includes('permission denied'))
  setLocale('zh-CN')
  assert.ok(describeError('permission denied').includes('permission denied'))
  setLocale('zh-CN')
})

test('Error 对象与空值都能兜住，不抛异常', () => {
  setLocale('zh-CN')
  assert.ok(describeError(new Error('E_UNLOCK_NOT_FOUND|找不到该消息')).includes('找不到该消息'))
  assert.ok(describeError('').length > 0)
  assert.ok(describeError(null).length > 0)
  assert.ok(describeError(undefined).length > 0)
})

test('Rust 侧写入的错误码与前端表一一对应（防两边漂移）', () => {
  // 后端加码后如果前端表没跟上，界面会走兜底句（原文是中文，英文界面还好，
  // 但中文界面会丢掉原本的措辞）。这里直接扫 Rust 源码，把两边的码对齐。
  const dir = new URL('../src-tauri/src/', import.meta.url)
  const found = new Set()
  for (const file of readdirSync(dir)) {
    if (!file.endsWith('.rs')) continue
    const code = readFileSync(new URL(file, dir), 'utf8')
    for (const m of code.matchAll(/"([A-Z][A-Z0-9_]{2,})\|/g)) found.add(m[1])
  }
  assert.ok(found.size >= 20, `应从 Rust 源码里扫到足够多的错误码，实际 ${found.size}`)
  const known = new Set(ERROR_CODES)
  const missing = [...found].filter((c) => !known.has(c)).sort()
  assert.deepEqual(
    missing,
    [],
    `Rust 用了这些码但前端 ERROR_CODES 里没有（补码 + 加中英文案）：${missing.join(', ')}`
  )
  // 截图模块的码不是字面量拼接（ShotErr::code()），单独守一遍
  const shot = readFileSync(new URL('screenshot.rs', dir), 'utf8')
  for (const code of ['CAPTURE_FAILED', 'CAPTURE_BUSY', 'PORTAL_MISSING', 'PORTAL_DENIED', 'PORTAL_TIMEOUT', 'DECODE_FAILED', 'MAC_PERMISSION']) {
    assert.match(shot, new RegExp(`"${code}"`), `screenshot.rs 应定义 ${code}`)
    assert.ok(known.has(code), `前端 ERROR_CODES 缺少截图码 ${code}`)
  }
})

test('每种错误码在两种语言里都有译文，且英文译文不含中文', () => {
  assert.ok(ERROR_CODES.length >= 15, `错误码表太小：${ERROR_CODES.length}`)
  const missing = []
  for (const code of ERROR_CODES) {
    setLocale('zh-CN')
    if (t(`err.${code}`) === `err.${code}`) missing.push(`${code}: 缺中文`)
    setLocale('en')
    const en = t(`err.${code}`)
    if (en === `err.${code}`) missing.push(`${code}: 缺英文`)
    else if (CJK.test(en)) missing.push(`${code}: 英文译文里还有中文「${en}」`)
  }
  setLocale('zh-CN')
  assert.deepEqual(missing, [], `错误码文案不完整：\n  ${missing.join('\n  ')}`)
})
