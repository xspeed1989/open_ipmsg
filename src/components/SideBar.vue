<script setup>
// 左侧功能栏：头像（点击进设置）+ 底部设置齿轮
import { store } from '../store'
import { t } from '../lib/i18n'
import Avatar from './Avatar.vue'

function openSettings() {
  store.settingsOpen = true
}
</script>

<template>
  <nav class="rail">
    <div class="self" :title="t('sidebar.profile')" @click="openSettings">
      <Avatar
        :name="store.config?.nickname || t('me')"
        :seed="'self-' + (store.config?.hostname || 'me')"
        :size="34"
      />
    </div>

    <div class="spacer"></div>

    <button class="nav-btn" :title="t('sidebar.settings')" @click="openSettings">
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
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--c-sub);
}
.nav-btn:hover {
  background: var(--c-hover);
  color: var(--c-text);
}
.spacer {
  flex: 1;
}
.rail > :last-child {
  margin-bottom: 14px;
}
</style>
