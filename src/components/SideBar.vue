<script setup>
// 左侧功能栏：我的头像 / 消息 / 通讯录 / 设置
import { computed } from 'vue'
import { store } from '../store'
import Avatar from './Avatar.vue'

const totalUnread = computed(() =>
  Object.values(store.unread).reduce((a, b) => a + b, 0)
)

function openSettings() {
  store.settingsOpen = true
}
</script>

<template>
  <nav class="rail">
    <div class="self" title="个人信息与设置">
      <Avatar
        :name="store.config?.nickname || '我'"
        :seed="'self-' + (store.config?.hostname || 'me')"
        :size="34"
      />
    </div>

    <button class="nav-btn" :class="{ active: store.page === 'chat' }" title="消息" @click="store.page = 'chat'">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
        <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.6c-.5.42-1.3.07-1.3-.6V5.5z"
          stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
      </svg>
      <i v-if="totalUnread" class="badge">{{ totalUnread > 99 ? '99+' : totalUnread }}</i>
    </button>

    <button class="nav-btn" :class="{ active: store.page === 'contacts' }" title="通讯录" @click="store.page = 'contacts'">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
        <rect x="4" y="3" width="16" height="18" rx="2.5" stroke="currentColor" stroke-width="1.6" />
        <circle cx="12" cy="10" r="2.6" stroke="currentColor" stroke-width="1.6" />
        <path d="M7.5 17.5c1-2 2.8-3 4.5-3s3.5 1 4.5 3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
      </svg>
    </button>

    <div class="spacer"></div>

    <button class="nav-btn" title="设置" @click="openSettings">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
        <circle cx="12" cy="12" r="3.2" stroke="currentColor" stroke-width="1.6" />
        <path d="M12 2.8l1 2.6a7 7 0 0 1 2.4 1l2.7-.8 1.6 2.8-1.9 2a7 7 0 0 1 .2 2.6l2 1.9-1.5 2.8-2.8-.7a7 7 0 0 1-2.3 1.4L13 21.2h-3.2l-.5-2.8a7 7 0 0 1-2.3-1.4l-2.8.7-1.5-2.8 2-1.9a7 7 0 0 1 0-2.6l-1.9-2 1.6-2.8 2.7.8a7 7 0 0 1 2.4-1l1-2.6z"
          stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" />
      </svg>
    </button>
  </nav>
</template>

<style scoped>
.rail {
  width: var(--rail-w);
  flex: none;
  background: var(--c-rail);
  border-right: 1px solid var(--c-hairline);
  display: flex;
  flex-direction: column;
  align-items: center;
  padding-top: 12px;
}
.self {
  margin-bottom: 14px;
  cursor: pointer;
  transition: opacity 0.15s;
}
.self:hover {
  opacity: 0.85;
}
.nav-btn {
  position: relative;
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #6b6b6b;
  margin-bottom: 4px;
}
.nav-btn:hover {
  background: rgba(0, 0, 0, 0.06);
  color: #333;
}
.nav-btn.active {
  color: var(--c-accent);
  background: rgba(7, 193, 96, 0.1);
}
.badge {
  position: absolute;
  top: 2px;
  right: 0;
  min-width: 15px;
  height: 15px;
  padding: 0 4px;
  border-radius: 8px;
  background: var(--c-danger);
  color: #fff;
  font-size: 10px;
  font-style: normal;
  line-height: 15px;
  text-align: center;
}
.spacer {
  flex: 1;
}
.rail > :last-child {
  margin-bottom: 14px;
}
</style>
