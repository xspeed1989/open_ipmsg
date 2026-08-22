<script setup>
// 简易表情面板（常用 Unicode Emoji）
const emit = defineEmits(['pick'])

const EMOJIS = [
  '😊', '😂', '🤣', '😍', '😜', '😎', '🤔', '😅', '😭', '😡',
  '🥺', '😴', '🤯', '🥳', '😇', '🙃', '😱', '🤡',
  '👍', '👎', '👌', '✌️', '🤝', '🙏', '👏', '💪', '🫶', '👋',
  '❤️', '💔', '💯', '🔥', '🎉', '🎁', '⚡', '☀️', '🌙', '🌈',
  '🍎', '🍜', '☕', '🍺', '⚽', '🚀', '💻', '📎',
]

function pick(e) {
  emit('pick', e)
}
</script>

<template>
  <div class="emoji-panel" @mousedown.stop>
    <button v-for="e in EMOJIS" :key="e" class="emoji" @click="pick(e)">{{ e }}</button>
  </div>
</template>

<style scoped>
.emoji-panel {
  position: absolute;
  bottom: 118px;
  right: 12px;
  width: 264px;
  padding: 8px;
  background: #fff;
  border: 1px solid var(--c-hairline);
  border-radius: 8px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.12);
  display: grid;
  grid-template-columns: repeat(10, 1fr);
  gap: 2px;
  z-index: 30;
}
.emoji {
  font-size: 17px;
  line-height: 26px;
  border-radius: 4px;
}
.emoji:hover {
  background: #f0f0f0;
}
</style>
