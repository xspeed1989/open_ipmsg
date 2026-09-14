// node --test scripts/ —— DialogHost 组件冒烟测试（真编译 + 真渲染）
//
// 为什么要有它：`vite build` 和静态契约测试只看语法和字符串，抓不到 setup/render
// 里的运行时错误。真踩过一次 —— defineProps 没接返回值就裸用 prop 名，
// 构建照过，浏览器里一渲染整个宿主组件空白。这里把 SFC 编译出来渲染一遍，
// 让这类错误在 `pnpm test` 里就暴露。
import { readFileSync, rmSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createSSRApp } from 'vue'
import { renderToString } from 'vue/server-renderer'
import { compileScript, parse } from 'vue/compiler-sfc'
import { toast, alert as showAlert, resetDialogs } from '../src/lib/dialog.js'
import { setLocale } from '../src/lib/i18n.js'

const SFC = new URL('../src/components/DialogHost.vue', import.meta.url)
// 编译产物落在组件目录里：这样它里面的相对导入（../lib/…）才解析得到
const OUT = new URL('../src/components/__dialoghost_smoke__.mjs', import.meta.url)

/** 把 SFC 编译成可直接 import 的 ESM 模块（inlineTemplate + ssr：模板编译成 SSR 渲染函数） */
async function loadDialogHost() {
  const { descriptor } = parse(readFileSync(SFC, 'utf8'), { filename: 'DialogHost.vue' })
  const compiled = compileScript(descriptor, {
    id: 'dialoghostsmoke',
    inlineTemplate: true,
    // 必须走 SSR 模板：客户端模板编译出的 Teleport 在 renderToString 的回退路径里
    // 整段不渲染（内容为空），那样冒烟测试就看不到 toast 了
    templateOptions: { ssr: true },
  })
  // 源码里的相对导入是 Vite 风格（不带扩展名），node 的 ESM 解析器需要补上 .js
  const content = compiled.content.replace(
    /(from\s+')(\.[^']*[^'.js]|[^']*\.(?:vue|json))(')/g,
    (full, head, spec, tail) => (spec.endsWith('.js') || spec.endsWith('.vue') ? full : `${head}${spec}.js${tail}`)
  )
  writeFileSync(OUT, content)
  return (await import(OUT.href)).default
}

test('DialogHost 真渲染：toast 与模态都能出来（setup/render 无运行时错误）', async () => {
  setLocale('zh-CN')
  resetDialogs()
  try {
    const DialogHost = await loadDialogHost()
    assert.ok(DialogHost, '应能编译出组件默认导出')

    // 轻提示
    resetDialogs()
    toast('已开封', { duration: 0 })
    const withToast = await renderToString(createSSRApp(DialogHost))
    assert.match(withToast, /已开封/, 'toast 文案应渲染出来')
    assert.match(withToast, /role="status"/, 'toast 容器要带 aria-live 语义')

    // 模态：确认框（带自定义按钮文案与危险样式）
    resetDialogs()
    const pending = showAlert('导入完成：共 3 条消息')
    const withModal = await renderToString(createSSRApp(DialogHost))
    assert.match(withModal, /role="dialog"/, '模态要带 dialog 语义')
    assert.match(withModal, /导入完成：共 3 条消息/)
    resetDialogs()
    await pending

    // 空态：没有 toast 也没有模态时不该崩
    resetDialogs()
    const empty = await renderToString(createSSRApp(DialogHost))
    assert.ok(typeof empty === 'string')
  } finally {
    resetDialogs()
    rmSync(OUT, { force: true })
  }
})
