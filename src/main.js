import { createApp } from 'vue'
import App from './App.vue'
import ImageViewer from './components/ImageViewer.vue'
import ScreenshotOverlay from './components/ScreenshotOverlay.vue'
import { boot, refreshConfig } from './store'
import './styles/global.css'

// 屏蔽浏览器原生右键菜单（主窗口与图片查看器窗口都生效）：
// - 消息气泡 / 联系人行的自绘菜单已 preventDefault，这里跳过不干扰；
// - 可编辑元素（输入框 / 多行输入 / contenteditable）放行原生菜单，
//   保证复制 / 剪切 / 粘贴等常用功能可用；
// - 其余区域（空白、图片、面板…）一律不弹原生菜单。
window.addEventListener('contextmenu', (e) => {
  if (e.defaultPrevented) return
  const el = e.target
  if (
    el &&
    typeof el.closest === 'function' &&
    el.closest('input, textarea, [contenteditable="true"], [contenteditable=""]')
  ) {
    return
  }
  e.preventDefault()
})

// 图片查看器是同一份前端的另一个入口（由 open_image_viewer 带查询串打开的独立窗口），
// 它不需要网络栈事件，也不该重复初始化主界面状态
const isViewer =
  new URLSearchParams(location.search).get('viewer') === 'image' || !!window.__OIM_VIEWER__

// 截图遮罩是同一份前端的第三个入口（由 start_screenshot 打开的独立窗口）
const isShot =
  new URLSearchParams(location.search).get('viewer') === 'shot' || !!window.__OIM_SHOT__

if (isShot) {
  createApp(ScreenshotOverlay).mount('#app')
  // 遮罩窗口也要有正确的主题变量，但不启动网络栈
  refreshConfig().catch((e) => console.error('shot config failed', e))
} else if (isViewer) {
  createApp(ImageViewer).mount('#app')
  // 独立图片窗口也要应用语言/主题：只拉配置，不启动网络栈
  refreshConfig().catch((e) => {
    console.error('viewer config failed', e)
  })
} else {
  createApp(App).mount('#app')
  // 挂载后初始化后端连接（加载配置、用户列表、注册事件监听）
  boot().catch((e) => {
    console.error('boot failed', e)
  })
}
