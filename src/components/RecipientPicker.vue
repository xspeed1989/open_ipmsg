<script setup>
// 多选接收人弹窗（转发 / 批量发送共用）：只列在线用户，排除当前会话
import { ref, computed } from 'vue'
import { store, displayName } from '../store'
import { t } from '../lib/i18n'
import Avatar from './Avatar.vue'

const props = defineProps({
  title: { type: String, default: () => t('picker.defaultTitle') },
  /** 排除的会话 key（如当前会话，避免发给自己正在看的会话） */
  excludeKey: { type: String, default: '' },
})
const emit = defineEmits(['confirm', 'cancel'])

const picked = ref(new Set())

/** 可选的用户：在线且不是当前会话 */
const candidates = computed(() =>
  store.users.filter((u) => u.key && u.key !== props.excludeKey)
)

function toggle(u) {
  const s = new Set(picked.value)
  if (s.has(u.key)) s.delete(u.key)
  else s.add(u.key)
  picked.value = s
}

function confirm() {
  emit('confirm', [...picked.value])
}
function cancel() {
  emit('cancel')
}
</script>

<template>
  <div class="rp-mask" @mousedown.self="cancel">
    <div class="rp-box">
      <div class="rp-head">
        <span>{{ title }}</span>
        <button class="rp-x" :title="t('picker.cancelEsc')" @click="cancel">✕</button>
      </div>
      <div class="rp-list">
        <div v-if="!candidates.length" class="rp-empty">
          {{ t('picker.empty') }}
        </div>
        <div
          v-for="u in candidates"
          :key="u.key"
          class="rp-row"
          :class="{ on: picked.has(u.key) }"
          @click="toggle(u)"
        >
          <i class="rp-check">{{ picked.has(u.key) ? '✓' : '' }}</i>
          <Avatar :name="u.nickname || u.user || '?'" :seed="u.key" :size="30" />
          <div class="rp-mid">
            <div class="rp-nick">{{ displayName(u.key) || u.key }}</div>
            <div class="rp-sub">{{ u.host || '' }} · {{ u.ip || '' }}</div>
          </div>
        </div>
      </div>
      <div class="rp-foot">
        <button class="rp-cancel" @click="cancel">{{ t('cancel') }}</button>
        <button class="rp-ok" :disabled="!picked.size" @click="confirm">
          {{ t('picker.send', { n: picked.size }) }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.rp-mask {
  position: fixed;
  inset: 0;
  z-index: 60;
  background: rgba(0, 0, 0, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
}
.rp-box {
  width: 340px;
  max-height: 480px;
  display: flex;
  flex-direction: column;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 10px;
  box-shadow: 0 10px 40px var(--c-shadow);
  overflow: hidden;
}
.rp-head {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 14px;
  font-size: 14px;
  font-weight: 600;
  border-bottom: 1px solid var(--c-hairline);
}
.rp-x {
  width: 22px;
  height: 22px;
  border-radius: 4px;
  color: var(--c-sub);
}
.rp-x:hover {
  background: var(--c-hover);
  color: var(--c-text);
}
.rp-list {
  flex: 1;
  overflow-y: auto;
  padding: 6px;
  min-height: 120px;
}
.rp-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  border-radius: 6px;
  cursor: pointer;
}
.rp-row:hover {
  background: var(--c-list-hover);
}
.rp-row.on {
  background: var(--c-list-active);
}
.rp-check {
  flex: none;
  width: 16px;
  height: 16px;
  border-radius: 4px;
  border: 1px solid var(--c-weak);
  color: transparent;
  font-style: normal;
  font-size: 11px;
  line-height: 16px;
  text-align: center;
}
.rp-row.on .rp-check {
  background: var(--c-accent);
  border-color: var(--c-accent);
  color: #fff;
}
.rp-mid {
  flex: 1;
  min-width: 0;
}
.rp-nick {
  font-size: 13.5px;
  line-height: 18px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.rp-sub {
  font-size: 11.5px;
  color: var(--c-sub);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.rp-empty {
  padding: 30px 0;
  text-align: center;
  color: var(--c-sub);
  font-size: 13px;
}
.rp-foot {
  flex: none;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 10px 14px;
  border-top: 1px solid var(--c-hairline);
}
.rp-cancel {
  padding: 5px 14px;
  border-radius: 5px;
  color: var(--c-text);
  background: var(--c-hover);
}
.rp-ok {
  padding: 5px 14px;
  border-radius: 5px;
  background: var(--c-accent);
  color: #fff;
}
.rp-ok:disabled {
  opacity: 0.5;
}
</style>