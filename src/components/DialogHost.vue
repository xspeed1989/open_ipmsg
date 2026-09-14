<script setup>
/**
 * 应用内对话框的**唯一**渲染宿主（挂在 App.vue 与图片查看器窗口入口）。
 *
 * 逻辑在 `lib/dialog.js`，这里只负责画：右下角 toast 堆叠 + 居中模态。
 * 视觉沿用设置弹窗那套（遮罩 + 卡片 + btn-primary/btn-plain + 全局主题变量），
 * 免得同一应用里出现两种弹窗长相。
 *
 * 快捷键：Esc = 取消/关闭，Enter = 确定（输入框里回车即提交）。
 */
import { computed, inject, nextTick, ref, watch } from 'vue'
import { dialog, closeModal, dismissToast } from '../lib/dialog'
import { t } from '../lib/i18n'

// toast 落点：主窗口落在**聊天记录区顶端**的锚点里（ChatWindow 用 provide 交出
// 元素 ref —— 用 CSS 选择器的话，Teleport 在元素入 DOM 之前解析会静默不渲染）；
// 图片查看器那种没有三栏布局的独立窗口没有锚点，就原地渲染在整窗顶端
const props = defineProps({
  toastArea: { type: String, default: 'window' },
})

const anchorEl = inject('toastAnchor', null)
const teleportTarget = computed(() =>
  props.toastArea === 'chat' && anchorEl?.value ? anchorEl.value : null
)

const modal = computed(() => dialog.modal)
const input = ref('')
const inputEl = ref(null)
const okBtn = ref(null)

// 打开模态时把焦点放进去：键盘用户不必先 Tab 一圈才够得到按钮
watch(modal, async (m) => {
  if (!m) return
  input.value = m.value ?? ''
  await nextTick()
  if (m.kind === 'prompt') inputEl.value?.focus()
  else okBtn.value?.focus()
})

/** 取消：confirm → false，prompt → null，alert → 关闭 */
function cancel() {
  const m = dialog.modal
  if (!m) return
  closeModal(m.kind === 'prompt' ? null : m.kind === 'confirm' ? false : undefined)
}

function submit() {
  const m = dialog.modal
  if (!m) return
  closeModal(m.kind === 'prompt' ? input.value : true)
}

function onKeydown(e) {
  if (!dialog.modal) return
  if (e.key === 'Escape') {
    e.preventDefault()
    cancel()
  } else if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    submit()
  }
}

const isMultiline = (text) => String(text ?? '').includes('\n')
</script>

<template>
  <div class="dialog-layer">
    <!-- 轻提示：贴所在区域顶端居中（主窗口 = 聊天记录区锚点内），自动消失、点一下即关。
         没有锚点时（图片窗口）disabled 让它在原地渲染，不会丢提示 -->
    <Teleport :to="teleportTarget" :disabled="!teleportTarget">
      <div class="toasts" role="status" aria-live="polite">
        <div
          v-for="item in dialog.toasts"
          :key="item.id"
          class="toast"
          :class="item.kind"
          :title="t('dialog.dismiss')"
          @click="dismissToast(item.id)"
        >
          <span class="toast-text">{{ item.text }}</span>
        </div>
      </div>
    </Teleport>

    <!-- 模态：确认 / 输入 / 需要读完的信息 -->
    <div v-if="modal" class="overlay" @click.self="cancel" @keydown="onKeydown">
      <div class="card" role="dialog" aria-modal="true" :aria-label="modal.text">
        <p class="text" :class="{ multiline: isMultiline(modal.text) }">{{ modal.text }}</p>
        <input
          v-if="modal.kind === 'prompt'"
          ref="inputEl"
          v-model="input"
          class="prompt-input"
          :type="modal.password ? 'password' : 'text'"
          :placeholder="modal.placeholder || ''"
          spellcheck="false"
          @keydown="onKeydown"
        />
        <div class="actions">
          <button
            v-if="modal.kind !== 'alert'"
            type="button"
            class="btn-plain"
            @click="cancel"
          >
            {{ modal.cancelLabel || t('cancel') }}
          </button>
          <button
            ref="okBtn"
            type="button"
            class="btn-primary"
            :class="{ danger: modal.danger }"
            @click="submit"
          >
            {{ modal.okLabel || t('dialog.ok') }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.dialog-layer {
  position: fixed;
  inset: 0;
  z-index: 200;
  /* 整层不接鼠标事件，交互元素各自打开：不然窗口会被一层透明遮罩挡住 */
  pointer-events: none;
}

/* ---------- toast ---------- */
/* 贴着锚点顶端居中：主窗口的锚点在聊天记录区内（消息列表顶端），
   图片查看器没有锚点，退回宿主自身（整窗顶端居中） */
.toasts {
  position: absolute;
  top: 12px;
  left: 0;
  right: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  /* teleport 到锚点后不再继承宿主的 pointer-events，容器自己关掉，
     只有 toast 本体可点，免得挡住聊天区顶部的点击 */
  pointer-events: none;
}
.toast {
  pointer-events: auto;
  cursor: pointer;
  background: var(--c-card);
  color: var(--c-text);
  border: 1px solid var(--c-hairline);
  border-left: 3px solid var(--c-accent);
  border-radius: 6px;
  box-shadow: 0 6px 20px var(--c-shadow);
  padding: 9px 12px;
  font-size: 12.5px;
  line-height: 1.5;
  max-width: min(360px, calc(100% - 24px));
  animation: toast-in 0.16s ease-out;
}
.toast.error {
  border-left-color: var(--c-danger);
}
.toast-text {
  white-space: pre-wrap;
  word-break: break-word;
  user-select: text;
}
@keyframes toast-in {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}

/* ---------- 模态 ---------- */
.overlay {
  pointer-events: auto;
  position: absolute;
  inset: 0;
  background: var(--c-mask);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}
.card {
  width: 360px;
  max-width: 100%;
  background: var(--c-card);
  border-radius: 10px;
  box-shadow: 0 12px 40px var(--c-shadow);
  padding: 18px 18px 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.text {
  margin: 0;
  font-size: 13.5px;
  line-height: 1.6;
  color: var(--c-text);
  user-select: text;
}
/* 多行内容（导入汇总这类）左对齐更好读 */
.text.multiline {
  white-space: pre-wrap;
  text-align: left;
  max-height: 40vh;
  overflow-y: auto;
}
.prompt-input {
  height: 32px;
  border: 1px solid var(--c-border);
  border-radius: 4px;
  background: var(--c-card-alt);
  color: var(--c-text);
  padding: 0 8px;
  font-size: 13px;
}
.prompt-input:focus {
  border-color: var(--c-accent);
}
.actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
/* 破坏性操作（删除会话）的确定键：红底，避免误点 */
.btn-primary.danger {
  background: var(--c-danger);
}
</style>
