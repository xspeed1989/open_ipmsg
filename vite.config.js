import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri 前端构建配置：固定端口 1420，构建目标与 Tauri webview 匹配
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: process.env.TAURI_DEV_HOST || false,
  },
  build: {
    target: 'es2021',
    minify: 'esbuild',
    sourcemap: false,
  },
})
