<script setup>
// 自定义标题栏：可拖拽 + 最小化/最大化/关闭（仿微信PC窗口控制）
import { ref, onMounted, onUnmounted } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { store } from '../store'

const appWindow = getCurrentWindow()
const maximized = ref(false)

let unlisten = null
onMounted(async () => {
  try {
    maximized.value = await appWindow.isMaximized()
    unlisten = await appWindow.onResized(async () => {
      maximized.value = await appWindow.isMaximized()
    })
  } catch (e) {
    /* 权限缺失时静默降级 */
  }
})
onUnmounted(() => unlisten && unlisten())

function minimize() {
  appWindow.minimize().catch(() => {})
}
function toggleMax() {
  appWindow.toggleMaximize().catch(() => {})
}
function close() {
  appWindow.close().catch(() => {})
}
</script>

<template>
  <div class="titlebar" data-tauri-drag-region @dblclick="toggleMax">
    <div class="tb-left" data-tauri-drag-region>
      <span class="logo" data-tauri-drag-region></span>
      <span class="app-name" data-tauri-drag-region>
        Open IPMsg v{{ store.config?.version || '?' }}
      </span>
    </div>
    <div class="tb-controls">
      <button class="tb-btn" title="最小化" @click="minimize">
        <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 5h8" stroke="currentColor" stroke-width="1" /></svg>
      </button>
      <button class="tb-btn" :title="maximized ? '还原' : '最大化'" @click="toggleMax">
        <svg v-if="!maximized" width="10" height="10" viewBox="0 0 10 10">
          <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1" />
        </svg>
        <svg v-else width="10" height="10" viewBox="0 0 10 10">
          <rect x="1.5" y="3" width="5.5" height="5.5" fill="none" stroke="currentColor" stroke-width="1" />
          <path d="M3.5 1.5h5v5" fill="none" stroke="currentColor" stroke-width="1" />
        </svg>
      </button>
      <button class="tb-btn tb-close" title="关闭" @click="close">
        <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" stroke-width="1" /></svg>
      </button>
    </div>
  </div>
</template>

<style scoped>
.titlebar {
  height: 34px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  background: var(--c-titlebar);
  border-bottom: 1px solid var(--c-hairline);
}
.tb-left {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-left: 12px;
}
.logo {
  width: 14px;
  height: 14px;
  border-radius: 4px;
  background: linear-gradient(135deg, #2fd06e, var(--c-accent));
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.25);
}
.app-name {
  font-size: 12px;
  color: var(--c-sub);
}
.tb-controls {
  display: flex;
  height: 100%;
}
.tb-btn {
  width: 44px;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--c-text);
}
.tb-btn:hover {
  background: var(--c-hover);
  color: var(--c-text);
}
.tb-close:hover {
  background: var(--c-danger);
  color: var(--c-card);
}
</style>
