// node --test scripts/ —— 原生弹窗禁用契约（界面提示/确认/输入一律走 lib/dialog）
//
// 背景：应用里曾有 40 处原生 alert/confirm/prompt，弹出的是系统对话框，和自绘
// 界面割裂（样式、深色主题、语言都对不上），密码输入还是明文。改完用一条测试
// 守住，免得后面又长回来。
//
// **保留项**：文件/文件夹选择与保存对话框（plugin-dialog 的 open / save）、
// 系统托盘、系统通知 —— 这些本来就是系统级能力。
import { readFileSync, readdirSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const srcDir = new URL('../src/', import.meta.url)
const files = [
  'App.vue',
  'main.js',
  'store.js',
  ...readdirSync(new URL('components/', srcDir), { withFileTypes: true })
    .filter((d) => d.isFile() && d.name.endsWith('.vue'))
    .map((d) => `components/${d.name}`),
  ...readdirSync(new URL('lib/', srcDir), { withFileTypes: true })
    .filter((d) => d.isFile() && d.name.endsWith('.js'))
    .map((d) => `lib/${d.name}`),
]

const RULES = [
  {
    re: /window\.(alert|confirm|prompt)\s*\(/,
    msg: 'window.alert/confirm/prompt 是系统原生弹窗',
  },
  {
    re: /(?<![.\w])alert\s*\(/,
    msg: '裸 alert() 是系统原生弹窗；应用内提示请用 lib/dialog 的 toast/alert',
  },
  {
    re: /import\s*\{[^}]*\b(confirm|ask|message)\b[^}]*\}\s*from\s*'@tauri-apps\/plugin-dialog'/,
    msg: 'plugin-dialog 的原生确认/消息框；文件与保存对话框（open/save）才是保留项',
  },
]

/** 去掉注释，避免注释里提到 alert( 造成误报 */
function stripComments(code) {
  return code.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '')
}

test('界面不得使用原生弹窗（文件/保存对话框除外）', () => {
  const hits = []
  for (const rel of files) {
    const code = stripComments(readFileSync(new URL(rel, srcDir), 'utf8'))
    for (const { re, msg } of RULES) {
      code.split('\n').forEach((line, i) => {
        if (re.test(line)) hits.push(`${rel}:${i + 1} ${msg}\n    ${line.trim().slice(0, 90)}`)
      })
    }
  }
  assert.deepEqual(hits, [], `发现原生弹窗调用：\n  ${hits.join('\n  ')}`)
})

test('对话框服务有唯一渲染宿主，且主窗口与图片窗口都挂上了', () => {
  const app = readFileSync(new URL('App.vue', srcDir), 'utf8')
  const viewer = readFileSync(new URL('components/ImageViewer.vue', srcDir), 'utf8')
  assert.match(app, /<DialogHost\b[^>]*\/>/, '主窗口必须挂 DialogHost')
  assert.match(viewer, /<DialogHost\b[^>]*\/>/, '图片查看器是独立窗口，也要挂自己的 DialogHost')
})

test('toast 锚定在聊天记录区顶端居中，不靠窗口坐标硬算', () => {
  const host = readFileSync(new URL('components/DialogHost.vue', srcDir), 'utf8')
  const chat = readFileSync(new URL('components/ChatWindow.vue', srcDir), 'utf8')
  const app = readFileSync(new URL('App.vue', srcDir), 'utf8')

  // 聊天窗口提供锚点：紧跟在头部之后、零高度 —— toast 由它贴着消息列表顶端，
  // 而不是拿窗口坐标去减标题栏/头部高度（那种算法一改布局就飘到标题栏上，正是修过的 bug）
  assert.match(chat, /class="toast-anchor"/, '聊天窗口需要 toast 锚点')
  const chatCss = chat.match(/<style scoped>([\s\S]*?)<\/style>/)?.[1] || ''
  const anchor = chatCss.match(/(?:^|\n)\s*\.toast-anchor\s*\{([^}]*)\}/)?.[1] || ''
  assert.match(anchor, /position\s*:\s*relative/, '锚点要充当定位上下文')
  assert.match(anchor, /height\s*:\s*0/, '锚点不能占位，否则会把消息列表推下去')

  // 宿主：主窗口把 toast teleport 进锚点；图片查看器没有锚点，留在宿主体内。
  // 锚点用元素 ref + provide/inject 传递，不用 CSS 选择器 —— Teleport 在选择器
  // 解析时若元素还没入 DOM 就静默不渲染（只给一条 warning），挂载顺序一变就丢提示
  assert.match(host, /inject\(\s*'toastAnchor'/, 'DialogHost 需 inject 锚点 ref')
  assert.match(
    host,
    /<Teleport\s+:to="[^"]+"\s+:disabled="[^"]+"/,
    '用 Teleport 落到锚点；没有锚点时靠 disabled 原地渲染'
  )
  // provide 必须在 DialogHost 的**祖先**上：ChatWindow 与 DialogHost 是 App 的
  // 兄弟节点，在 ChatWindow 里 provide 传不到宿主机（真踩过：锚点为 null，
  // toast 悄悄退回整窗顶端）
  assert.match(
    app,
    /provide\(\s*'toastAnchor'/,
    'toastAnchor 必须由 App（DialogHost 的祖先）provide'
  )
  assert.match(chat, /inject\(\s*'toastAnchor'/, 'ChatWindow 需 inject 这个共享 ref')
  // 绑定必须走脚本侧 setter：ref 对象直接写进模板会被 setupState 的 proxyRefs
  // 解包成 null（A/B 实测对比确认），锚点绑不上 → toast 悄悄退回整窗顶端
  assert.match(chat, /:ref="setToastAnchor"/, 'ChatWindow 要用 setter 把注入的 ref 绑到锚点元素上')
  assert.match(
    chat,
    /const setToastAnchor = \(el\) => \{[\s\S]*?toastAnchor\.value = el/,
    'setter 要把元素写回注入的 ref'
  )
  // 只看模板段：脚本里的注释会提到这个错误写法
  const chatTemplate = chat.match(/<template>([\s\S]*?)<\/template>/)?.[1] || ''
  assert.doesNotMatch(
    chatTemplate,
    /:ref="toastAnchor"/,
    'ref 对象不能直接写进模板（会被 setupState 解包成 null）'
  )
  assert.doesNotMatch(
    chat,
    /provide\(\s*'toastAnchor'/,
    'ChatWindow 不是 DialogHost 的祖先，provide 在这里等于没provide'
  )

  const hostCss = host.match(/<style scoped>([\s\S]*?)<\/style>/)?.[1] || ''
  const toasts = hostCss.match(/(?:^|\n)\s*\.toasts\s*\{([^}]*)\}/)?.[1] || ''
  assert.match(toasts, /position\s*:\s*absolute/)
  assert.match(toasts, /top\s*:\s*12px/, '贴着锚点顶端（原来的 cw-toast 就是这个位置）')
  assert.match(toasts, /align-items\s*:\s*center/, 'toast 需水平居中')
  assert.match(
    toasts,
    /pointer-events\s*:\s*none/,
    'teleport 后不再继承宿主的 pointer-events，容器必须显式关掉，否则挡住聊天区顶部点击'
  )
  assert.doesNotMatch(hostCss, /--rail-w/, '不该再用侧边栏/列表宽度硬算聊天区位置')
  assert.match(app, /toast-area="chat"/, '主窗口要显式指定聊天区落点（图片窗口用默认的整窗）')
})

test('script setup 里不得裸用 prop 名（构建期不报错，运行时才炸）', () => {
  // 真踩过：defineProps 没接返回值就在 computed 里写 toastArea，vite build 照过，
  // 浏览器里一渲染就 ReferenceError，整个宿主组件空白
  for (const rel of ['components/DialogHost.vue', 'components/SettingsModal.vue', 'components/ChatWindow.vue']) {
    const code = readFileSync(new URL(rel, srcDir), 'utf8')
    const script = code.match(/<script setup>([\s\S]*?)<\/script>/)?.[1] || ''
    const decl = script.match(/defineProps\(\s*\{[\s\S]*?\n\s*\}\)/)
    if (!decl) continue
    const names = [...decl[0].matchAll(/^\s*([A-Za-z_$][\w$]*)\s*:/gm)].map((m) => m[1])
    if (!names.length) continue
    if (!/const props = defineProps\(/.test(script)) {
      assert.fail(`${rel}: script 里要用 prop 就必须 const props = defineProps(...)`)
    }
    // 声明块本身含 prop 名，先摘掉；再摘掉 props.xxx 的合法用法
    const rest = script.replace(decl[0], '').replace(/props\.[\w$]+/g, '')
    const bare = names.filter((n) => new RegExp(`(?<![.\\w$])${n}(?![\\w$])`).test(rest))
    assert.deepEqual(bare, [], `${rel}: script 里裸用了 prop ${bare.join(', ')}，会运行时 ReferenceError`)
  }
})
