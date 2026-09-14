<script setup>
// 根组件：自定义标题栏 + 三栏布局（侧边栏 / 列表 / 聊天窗口）
import { provide, ref } from 'vue'
import { store } from './store'
import TitleBar from './components/TitleBar.vue'
import SideBar from './components/SideBar.vue'
import ListPanel from './components/ListPanel.vue'
import ChatWindow from './components/ChatWindow.vue'
import SettingsModal from './components/SettingsModal.vue'
import DialogHost from './components/DialogHost.vue'

// toast 锚点：DialogHost 与 ChatWindow 是兄弟节点，provide/inject 只向下传，
// 所以必须由共同祖先 App 提供，ChatWindow 注入后绑到自己的锚点元素上
const toastAnchor = ref(null)
provide('toastAnchor', toastAnchor)
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
    <!-- 应用内弹窗宿主：toast 落在聊天记录区顶部居中（原生弹窗已全部替换） -->
    <DialogHost toast-area="chat" />
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
