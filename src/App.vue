<script setup>
// 根组件：自定义标题栏 + 三栏布局（侧边栏 / 列表 / 聊天窗口）
import { onMounted, onUnmounted, provide, ref } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { store } from './store'
import { matchesLocalCombo } from './lib/hotkey'
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

/**
 * 应用内「关闭窗口」快捷键（close_hotkey，默认 Ctrl+W）：等价于点自绘标题栏的 X。
 *
 * 读的是 `store.runtimeCloseHotkey`（本进程首次加载配置后冻结的值），而不是实时读
 * `store.config.close_hotkey`：关闭快捷键与全局热键一样「改配置不重启不换绑」，保存后
 * 仍按本次运行实际生效的那个组合匹配。
 *
 * 挂在 window **捕获**阶段：应用内其它快捷键（ChatWindow 的 Ctrl+F 搜索等）都在冒泡
 * 阶段监听且挂载更早，冒泡阶段会被它们抢先 preventDefault，用户配置的关闭键就形同虚设
 * （把关闭键设成 Ctrl+F 时按下去只会开搜索）。捕获阶段先于所有冒泡监听，命中时再
 * stopPropagation 断掉后续分发，从而让「用户显式配置的关闭动作」优先级最高。
 * 用 close 而不是 hide，以复用后端 CloseRequested 的「隐藏到托盘 + 首次提示」。
 */
function onCloseHotkey(e) {
  // 设置页的录制框（三个热键 input 都带 data-hotkey-recorder）：捕获阶段早于录制框自己的
  // target 阶段监听，这里不主动跳过的话，录 Ctrl+W 会直接把主窗口关掉。
  // 用显式数据标记而不是 CSS 类名：类名会随样式调整，标记是这段逻辑的合约。
  if (e.target?.dataset?.hotkeyRecorder !== undefined) return
  if (e.defaultPrevented || e.repeat || e.isComposing) return
  const combo = store.runtimeCloseHotkey
  if (!combo) return
  if (!matchesLocalCombo(combo, e)) return
  e.preventDefault()
  // 阻断事件继续下发：否则同一次按键还会被 ChatWindow 的冒泡监听当成搜索等命令
  e.stopPropagation()
  getCurrentWindow().close().catch(() => {})
}
onMounted(() => window.addEventListener('keydown', onCloseHotkey, true))
onUnmounted(() => window.removeEventListener('keydown', onCloseHotkey, true))
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
