// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isSealedEnvelope, isPasswordEnvelope, showsSealTag, sealedPreviewKey } from '../src/lib/secret.js'
import { t, setLocale } from '../src/lib/i18n.js'

test('自己发出的封书消息不是「未开封的信封」：正文本来就是自己写的', () => {
  // 出站记录只有 secret，没有 unlocked 字段（旧记录也一样）——
  // 曾经的判定只看 secret && !unlocked，导致自己的封书消息被渲染成
  // 「封书：对方发来的保密消息」，点开封还报「找不到该消息」
  assert.equal(isSealedEnvelope({ dir: 'out', secret: true }), false)
  assert.equal(isSealedEnvelope({ dir: 'out', secret: true, unlocked: false }), false)
})

test('自己发出的密码消息同样不挡正文', () => {
  assert.equal(isSealedEnvelope({ dir: 'out', locked: true, unlocked: false }), false)
  assert.equal(isPasswordEnvelope({ dir: 'out', locked: true, unlocked: false }), false)
})

test('对端发来的未开封封书：显示信封占位', () => {
  assert.equal(isSealedEnvelope({ dir: 'in', secret: true, unlocked: false }), true)
  assert.equal(isPasswordEnvelope({ dir: 'in', secret: true, unlocked: false }), false)
})

test('开封后的封书（unlocked=true）才显示正文', () => {
  // 后端不再对入站封书自动开封：无密码封书落库就是 unlocked=false，
  // 收件人点「打开（开封）」之后才是这个形状
  assert.equal(isSealedEnvelope({ dir: 'in', secret: true, unlocked: true }), false)
})

test('对端发来的密码锁未开封：信封占位用密码文案', () => {
  const m = { dir: 'in', secret: false, locked: true, unlocked: false }
  assert.equal(isSealedEnvelope(m), true)
  assert.equal(isPasswordEnvelope(m), true)
})

test('字段缺失/空值一律不当作信封', () => {
  assert.equal(isSealedEnvelope(null), false)
  assert.equal(isSealedEnvelope(undefined), false)
  assert.equal(isSealedEnvelope({}), false)
  assert.equal(isSealedEnvelope({ dir: 'in' }), false)
  assert.equal(isSealedEnvelope({ dir: 'in', secret: true, unlocked: undefined }), true,
    'unlocked 缺失即未开封（历史记录里不会写 false）')
})

test('封书标记：收发两端都标，普通消息不标', () => {
  assert.equal(showsSealTag({ dir: 'out', secret: true }), true)
  assert.equal(showsSealTag({ dir: 'in', secret: true, unlocked: true }), true)
  assert.equal(showsSealTag({ dir: 'out', secret: false }), false)
  assert.equal(showsSealTag({}), false)
})

test('通知预览：未开封的信封不显示正文，只报「未开封」', () => {
  setLocale('zh-CN')
  // 无密码封书（后端 2026-09 起不再自动开封，入站就是 unlocked=false）
  const sealed = { dir: 'in', kind: 'text', secret: true, locked: false, unlocked: false, text: '密信正文' }
  assert.equal(sealedPreviewKey(sealed), 'preview.sealed')
  assert.equal(t(sealedPreviewKey(sealed)), '[封书] 未开封')
  // 密码锁未开封
  const locked = { dir: 'in', kind: 'text', secret: false, locked: true, unlocked: false, text: '口令内容' }
  assert.equal(sealedPreviewKey(locked), 'preview.locked')
  assert.equal(t(sealedPreviewKey(locked)), '[密码锁] 未开封')
  // 开封后 / 自己的出站封书 / 普通消息：照常显示正文
  assert.equal(sealedPreviewKey({ ...sealed, unlocked: true }), null)
  assert.equal(sealedPreviewKey({ dir: 'out', secret: true, text: '我写的' }), null)
  assert.equal(sealedPreviewKey({ dir: 'in', text: '普通消息' }), null)
  setLocale('en')
  assert.equal(t(sealedPreviewKey(sealed)), '[Sealed message] unopened')
  setLocale('zh-CN')
})

test('信封占位文案不再自带「打开」：按钮就写着「打开（开封）」', () => {
  // 「…保密消息，点击打开」+ 按钮「打开（开封）」＝同一行两个「打开」
  setLocale('zh-CN')
  assert.ok(
    !t('chat.secretSealed').includes('打开'),
    '占位文案不该重复按钮的动词'
  )
  assert.ok(t('chat.unlock').includes('打开'), '按钮才是那个「打开」')
  setLocale('en')
  assert.ok(!t('chat.secretSealed').toLowerCase().includes('open'))
  setLocale('zh-CN')
})
