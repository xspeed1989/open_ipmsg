// node --test scripts/ —— 撤回后重新编辑的纯状态转换
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { recalledEditState } from '../src/lib/recall.js'

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
