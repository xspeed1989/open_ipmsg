/**
 * 应用内对话框服务：把原生 alert/confirm/prompt 换成自绘 UI 的唯一入口。
 *
 * 为什么要有这一层：原生弹窗是系统画的，和自绘界面割裂（样式、深色主题、语言
 * 都对不上），而且 `window.prompt` 连密码都是明文。这里把「提示/确认/输入」
 * 收敛成三个命令式 API，渲染交给 `DialogHost.vue`（唯一宿主，挂一次）。
 *
 * 纯逻辑 + vue 的响应式，不碰 DOM，所以能在 node 下直接测。
 *
 * 用法：
 *   toast(t('chat.unlockedOk'))                     // 轻提示，自动消失
 *   if (!(await confirm(t('list.deleteConfirm')))) return
 *   const pw = await prompt(t('chat.pwdPrompt'), { password: true })
 */
import { reactive } from 'vue'

/** 同屏最多几条 toast（超出丢最旧的，避免刷屏时糊满一屏） */
export const MAX_TOASTS = 4
/** 默认停留时长（毫秒）；`duration: 0` 表示不自动消失 */
export const TOAST_MS = 3200

export const dialog = reactive({
  /** [{ id, text, kind }]，kind: 'info' | 'error' */
  toasts: [],
  /** 当前模态：{ id, kind, text, okLabel?, cancelLabel?, danger?, password?, placeholder?, resolve } */
  modal: null,
})

let seq = 0
const timers = new Map()

/** 轻提示：进队列并定时消失，返回 id（可手动 dismissToast） */
export function toast(text, { kind = 'info', duration = TOAST_MS } = {}) {
  const id = ++seq
  dialog.toasts.push({ id, text: String(text ?? ''), kind })
  while (dialog.toasts.length > MAX_TOASTS) dismissToast(dialog.toasts[0].id)
  if (duration > 0) timers.set(id, setTimeout(() => dismissToast(id), duration))
  return id
}

/** 关掉一条 toast（用户点击 / 到时 / 超限淘汰都走这里，定时器一并清掉） */
export function dismissToast(id) {
  const timer = timers.get(id)
  if (timer) {
    clearTimeout(timer)
    timers.delete(id)
  }
  const at = dialog.toasts.findIndex((item) => item.id === id)
  if (at >= 0) dialog.toasts.splice(at, 1)
}

function openModal(kind, text, opts = {}) {
  // 同一时刻只留一个模态：被顶掉的旧框按「取消」落定。
  // 否则它的 Promise 永远悬着，调用方的 await 之后的代码再也不会执行。
  if (dialog.modal) closeModal()
  return new Promise((resolve) => {
    dialog.modal = { id: ++seq, kind, text: String(text ?? ''), ...opts, resolve }
  })
}

/** 关闭当前模态；无参时按该模态的「取消」语义落定（confirm→false，prompt→null） */
export function closeModal(result) {
  const modal = dialog.modal
  if (!modal) return
  dialog.modal = null
  let value = result
  if (value === undefined) {
    if (modal.kind === 'confirm') value = false
    else if (modal.kind === 'prompt') value = null
  }
  modal.resolve(value)
}

/** 模态信息框（需要用户读完再走的多行内容，如导入汇总）；返回 Promise<void> */
export const alert = (text, opts) => openModal('alert', text, opts)

/** 模态确认框；返回 Promise<boolean> */
export const confirm = (text, opts) => openModal('confirm', text, opts)

/** 模态输入框（密码锁解锁用 password: true）；返回 Promise<string|null> */
export const prompt = (text, opts) => openModal('prompt', text, opts)

/** 清空全部状态（测试与窗口重建用） */
export function resetDialogs() {
  for (const id of [...timers.keys()]) dismissToast(id)
  const modal = dialog.modal
  if (modal) {
    dialog.modal = null
    modal.resolve(modal.kind === 'confirm' ? false : modal.kind === 'prompt' ? null : undefined)
  }
  dialog.toasts.splice(0)
}
