// node --test scripts/ —— 撤回的菜单条件与撤回后的渲染优先级
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { canRecall, canReedit, recalledEditState } from '../src/lib/recall.js'
import { isBroadcastSession, BROADCAST_SESSION_KEY } from '../src/lib/sessions.js'

const chatWindow = readFileSync(
  new URL('../src/components/ChatWindow.vue', import.meta.url),
  'utf8',
)

/* ---------- 菜单出现条件 ---------- */

test('自己发出的文本、文件、表情（kind 与 file/text 一样）都能撤回', () => {
  for (const kind of ['text', 'file']) {
    assert.equal(canRecall({ dir: 'out', kind }), true, kind + ' 应该能撤回')
  }
  // 图片/剪贴板贴图/表情/文件夹落库都是 file，前端不该按子类型再拦一次
  assert.equal(canRecall({ dir: 'out', kind: 'file', files: [{ name: 'a.png' }] }), true)
  assert.equal(canRecall({ dir: 'out', kind: 'file', files: [{ dir_entry: true }] }), true)
  // 老记录缺 kind 时按文本处理（与后端 unwrap_or("text") 一致）
  assert.equal(canRecall({ dir: 'out' }), true)
})

test('对方消息不能撤回', () => {
  assert.equal(canRecall({ dir: 'in', kind: 'text' }), false)
  assert.equal(canRecall({ dir: 'in', kind: 'file' }), false)
  assert.equal(canRecall({ dir: undefined, kind: 'text' }), false)
  assert.equal(canRecall(null), false)
  assert.equal(canRecall(undefined), false)
})

test('已撤回的消息不再显示撤回', () => {
  assert.equal(canRecall({ dir: 'out', kind: 'text', recalled: true }), false)
  assert.equal(canRecall({ dir: 'out', kind: 'file', recalled: true }), false)
  // recalled 只认严格 true，避免旧后端写 1/"true" 之外的假值干扰
  assert.equal(canRecall({ dir: 'out', kind: 'text', recalled: false }), true)
})

test('广播信箱不给撤回入口（一次发给多人，DELMSG 表达不了）', () => {
  assert.equal(isBroadcastSession(BROADCAST_SESSION_KEY), true)
  assert.equal(canRecall({ dir: 'out', kind: 'text' }, { broadcast: true }), false)
  assert.equal(canRecall({ dir: 'out', kind: 'file' }, { broadcast: true }), false)
  // 非广播会话不受影响
  assert.equal(canRecall({ dir: 'out', kind: 'text' }, { broadcast: false }), true)
  assert.equal(canRecall({ dir: 'out', kind: 'text' }, {}), true)
})

/* ---------- 模板锁：源码级约束 ---------- */

test('右键菜单的撤回项按 canRecall 判断，不再写死 kind === text', () => {
  const menuAt = chatWindow.indexOf('canRecall(ctxMenu.msg')
  assert.ok(menuAt > 0, '右键菜单必须用 canRecall 判断而不是写死 kind')
  const menu = chatWindow.slice(menuAt - 200, menuAt + 300)
  assert.match(menu, /canRecall\(ctxMenu\.msg, \{ broadcast: isBroadcast \}\)/)
  assert.doesNotMatch(menu, /kind === 'text'/)
  // 广播会话的判断来自当前会话 key，而不是消息字段
  assert.match(chatWindow, /const isBroadcast = computed\(\(\) => isBroadcastKey\(store\.activeKey\)\)/)
})

test('已撤回分支排在 msg-row 之前，附件卡片与图片预览都被挡住', () => {
  const recalledAt = chatWindow.indexOf('v-else-if="v.m.recalled"')
  const rowAt = chatWindow.indexOf('class="msg-row"')
  assert.ok(recalledAt > 0, '必须有 v-else-if="v.m.recalled" 分支')
  assert.ok(rowAt > 0, '必须有 msg-row')
  assert.ok(
    recalledAt < rowAt,
    'recalled 分支必须在 msg-row 之前，否则附件卡片/内联图片会照常渲染',
  )
})

test('撤回后「重新编辑」只对文本出现，附件不出现死按钮', () => {
  assert.equal(canReedit({ dir: 'out', kind: 'text', recalled: true, text: 'a' }), true)
  assert.equal(canReedit({ dir: 'out', kind: 'file', recalled: true, text: '' }), false)
  assert.equal(canReedit({ dir: 'out', kind: 'file', recalled: true, text: '配文' }), false)
  assert.equal(canReedit({ dir: 'in', kind: 'text', recalled: true, text: 'a' }), false)
  assert.equal(canReedit({ dir: 'out', kind: 'text', recalled: false, text: 'a' }), false)
  // 模板必须走这个判据，不能只按方向判断
  const notice = chatWindow.slice(
    chatWindow.indexOf('class="recall-notice"'),
    chatWindow.indexOf('class="recall-notice"') + 600,
  )
  assert.match(notice, /v-if="canReedit\(v\.m\)"/)
  assert.doesNotMatch(notice, /v-if="v\.m\.dir === 'out'"/)
  // 判据本身
  assert.equal(recalledEditState({ dir: 'out', kind: 'file', recalled: true }), null)
  // 文本撤回后即使没有正文（纯占位），编辑状态也只会是空草稿，不会串到附件上
  assert.deepEqual(recalledEditState({ dir: 'out', kind: 'text', recalled: true }), {
    draft: '',
    replyTarget: null,
  })
  assert.deepEqual(recalledEditState({ dir: 'out', kind: 'text', recalled: true, text: 'hi' }), {
    draft: 'hi',
    replyTarget: null,
  })
})

/* ---------- 撤回后重新编辑（原有语义，回归） ---------- */

test('自己撤回的文本按原样生成编辑状态，覆盖草稿并取消回复', () => {
  const state = recalledEditState({
    dir: 'out',
    kind: 'text',
    recalled: true,
    text: '  第一行\n第二行  ',
  })

  assert.deepEqual(state, {
    draft: '  第一行\n第二行  ',
    replyTarget: null,
  })
})

test('对方消息、未撤回消息和附件不能进入重新编辑', () => {
  assert.equal(recalledEditState({ dir: 'in', kind: 'text', recalled: true, text: 'a' }), null)
  assert.equal(recalledEditState({ dir: 'out', kind: 'text', recalled: false, text: 'b' }), null)
  assert.equal(recalledEditState({ dir: 'out', kind: 'file', recalled: true, text: 'c' }), null)
})
