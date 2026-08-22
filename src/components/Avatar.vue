<script setup>
// 头像组件：无外部图片资源，按 key 哈希取色 + 昵称首字
import { computed } from 'vue'

const props = defineProps({
  name: { type: String, default: '' },
  seed: { type: String, default: '' },
  size: { type: Number, default: 40 },
})

const PALETTE = [
  '#7bb083', '#e6a23c', '#8f9dde', '#d98bab', '#67b5b5',
  '#b58cd9', '#c98a7a', '#7fa650', '#5f9ea8', '#a68a78',
]

const bg = computed(() => {
  const s = (props.seed || props.name || '?').toString()
  let h = 0
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0
  return PALETTE[h % PALETTE.length]
})

const char = computed(() => {
  const n = (props.name || '').trim()
  if (!n) return '?'
  const first = Array.from(n)[0]
  return /[a-z]/i.test(first) ? first.toUpperCase() : first
})
</script>

<template>
  <div
    class="avatar"
    :style="{ width: size + 'px', height: size + 'px', background: bg, fontSize: size * 0.42 + 'px', borderRadius: Math.max(4, size * 0.1) + 'px' }"
  >
    {{ char }}
  </div>
</template>

<style scoped>
.avatar {
  flex: none;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  user-select: none;
}
</style>
