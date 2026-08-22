import { createApp } from 'vue'
import App from './App.vue'
import { boot } from './store'
import './styles/global.css'

const app = createApp(App)
app.mount('#app')

// 挂载后初始化后端连接（加载配置、用户列表、注册事件监听）
boot().catch((e) => {
  console.error('boot failed', e)
})
