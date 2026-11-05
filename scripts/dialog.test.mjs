// node --test scripts/ —— 应用内对话框服务单测（不依赖 DOM / Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  dialog, toast, alert, confirm, prompt, closeModal, dismissToast, resetDialogs, MAX_TOASTS,
} from '../src/lib/dialog.js'

test('toast：进队列、可手动关掉', () => {
  resetDialogs()
  const id = toast('保存成功')
  assert.equal(dialog.toasts.length, 1)
  assert.equal(dialog.toasts[0].text, '保存成功')
  dismissToast(id)
  assert.equal(dialog.toasts.length, 0)
})

test('toast：超过上限丢最旧的，不会无限堆叠', () => {
  resetDialogs()
  for (let i = 0; i < MAX_TOASTS + 2; i++) toast(`第 ${i} 条`)
  assert.equal(dialog.toasts.length, MAX_TOASTS)
  assert.equal(dialog.toasts.at(-1).text, `第 ${MAX_TOASTS + 1} 条`)
})

test('toast：到时自动消失', async () => {
  resetDialogs()
  toast('稍纵即逝', { duration: 5 })
  assert.equal(dialog.toasts.length, 1)
  await new Promise((r) => setTimeout(r, 25))
  assert.equal(dialog.toasts.length, 0)
})

test('toast：duration 为 0 时不自动消失（错误提示需要用户自己点掉）', async () => {
  resetDialogs()
  toast('需要手动关掉', { duration: 0 })
  await new Promise((r) => setTimeout(r, 15))
  assert.equal(dialog.toasts.length, 1)
  resetDialogs()
})

test('toast：带 action 的提示会留到用户点掉（否则入口一闪就没）', async () => {
  resetDialogs()
  let ran = 0
  const id = toast('已保存到「OpenIPMsg 截图」', { action: { text: '所在文件夹', run: () => ran++ } })
  const item = dialog.toasts.find((t) => t.id === id)
  assert.equal(item.action.text, '所在文件夹')
  // 没显式给 duration 时不自动消失：不然用户还没来得及点就没了
  await new Promise((r) => setTimeout(r, 15))
  assert.equal(dialog.toasts.length, 1, '带入口的提示不应自动消失')

  // 普通 toast 不变：默认仍会自动消失
  resetDialogs()
  toast('普通提示')
  await new Promise((r) => setTimeout(r, 15))
  assert.equal(dialog.toasts.length, 1, '默认时长内还在')
  resetDialogs()
})

test('toast：action 只接受 { text, run }，run 不是函数就当没有入口', () => {
  resetDialogs()
  const id = toast('x', { action: { text: '入口', run: 'not-a-function' } })
  const item = dialog.toasts.find((t) => t.id === id)
  assert.equal(item.action, undefined)
  resetDialogs()
})

test('confirm：确定 / 取消分别落定 true / false', async () => {
  resetDialogs()
  const yes = confirm('删除这条会话？')
  assert.equal(dialog.modal.kind, 'confirm')
  closeModal(true)
  assert.equal(await yes, true)

  const no = confirm('删除这条会话？')
  closeModal(false)
  assert.equal(await no, false)
})

test('confirm：无参关闭按「取消」处理', async () => {
  resetDialogs()
  const p = confirm('继续？')
  closeModal()
  assert.equal(await p, false)
})

test('prompt：返回输入值；关闭返回 null', async () => {
  resetDialogs()
  const p1 = prompt('请输入本机密码', { password: true })
  assert.equal(dialog.modal.kind, 'prompt')
  assert.equal(dialog.modal.password, true)
  closeModal('hunter2')
  assert.equal(await p1, 'hunter2')

  const p2 = prompt('请输入本机密码', { password: true })
  closeModal()
  assert.equal(await p2, null)
})

test('alert：关闭后落定（不带内容）', async () => {
  resetDialogs()
  const p = alert('导入完成：3 条')
  assert.equal(dialog.modal.kind, 'alert')
  closeModal()
  assert.equal(await p, undefined)
})

test('模态互斥：新框出现时旧框按取消落定，不留悬挂的 Promise', async () => {
  resetDialogs()
  const first = confirm('第一个')
  const second = confirm('第二个')
  assert.equal(await first, false, '被顶掉的旧框必须落定，否则调用方永远 await 住')
  assert.equal(dialog.modal.text, '第二个')
  closeModal(true)
  assert.equal(await second, true)
})
