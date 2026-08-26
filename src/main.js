import { createApp } from 'vue'
import App from './App.vue'
import ImageViewer from './components/ImageViewer.vue'
import { boot, refreshConfig } from './store'
import './styles/global.css'

// 图片查看器是同一份前端的另一个入口（由 open_image_viewer 带查询串打开的独立窗口），
// 它不需要网络栈事件，也不该重复初始化主界面状态
const isViewer =
  new URLSearchParams(location.search).get('viewer') === 'image' || !!window.__OIM_VIEWER__

if (isViewer) {
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
