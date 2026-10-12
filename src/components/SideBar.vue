<script setup>
// 左侧功能栏：头像（点击进设置）+ 底部设置齿轮
import { store, setSettingsOpen } from '../store'
import { t } from '../lib/i18n'
import Avatar from './Avatar.vue'

function openSettings() {
  setSettingsOpen(true)
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
      <!-- 齿轮：8 齿规则多边形（外圆 8.5 / 齿根 6.6，45° 旋转对称），
           与外圈内孔**同一描边 1.6** —— 之前外圈 1.3、内孔 1.6 且齿宽窄不一，
           在一栏线性图标里显得歪。改路径请保持齿形规整 + 粗细一致
           （scripts/icons.test.mjs 会校验这两点） -->
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
        <circle cx="12" cy="12" r="3.2" stroke="currentColor" stroke-width="1.6" />
        <path d="M11.14 5.46 L11.63 3.51 L15.25 4.15 L15.05 6.15 L16.02 6.76 L17.74 5.73 L19.85 8.75 L18.29 10.02 L18.54 11.14 L20.49 11.63 L19.85 15.25 L17.85 15.05 L17.24 16.02 L18.27 17.74 L15.25 19.85 L13.98 18.29 L12.86 18.54 L12.37 20.49 L8.75 19.85 L8.95 17.85 L7.98 17.24 L6.26 18.27 L4.15 15.25 L5.71 13.98 L5.46 12.86 L3.51 12.37 L4.15 8.75 L6.15 8.95 L6.76 7.98 L5.73 6.26 L8.75 4.15 L10.02 5.71 Z"
          stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
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
