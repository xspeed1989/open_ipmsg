<script setup>
// 根组件：自定义标题栏 + 三栏布局（侧边栏 / 列表 / 聊天窗口）
import { store } from './store'
import TitleBar from './components/TitleBar.vue'
import SideBar from './components/SideBar.vue'
import ListPanel from './components/ListPanel.vue'
import ChatWindow from './components/ChatWindow.vue'
import SettingsModal from './components/SettingsModal.vue'
</script>

<template>
  <div class="app">
    <TitleBar />
    <div class="body">
      <SideBar />
      <ListPanel />
      <ChatWindow />
    </div>
    <SettingsModal v-if="store.settingsOpen" />
  </div>
</template>

<style scoped>
.app {
  height: 100%;
  display: flex;
  flex-direction: column;
}
.body {
  flex: 1;
  min-height: 0;
  display: flex;
}
</style>
