// node --test scripts/ —— SFC 模板绑定校验
//
// 为什么需要它：Vue 的模板编译**不校验标识符是否存在**，`vite build` 同样不校验。
// 于是「模板里调用了一个已不存在的函数」这种错误能一路通过构建，只在运行时由
// Vue 打一条 console warning、界面上表现为「点了没反应」。
//
// 真实事故（2026-09）：清理废弃的右键菜单动作时，正则把相邻的 closeMenu 一并删掉，
// 导致 ❤️ 页的「移到最前 / 删除」静默失效，而 build 与全部既有测试都是绿的。
// 这个校验器把「模板引用的顶层标识符必须在 <script setup> 里有定义」变成硬约束。
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

/** 模板里允许出现、但不需要在 script 里定义的名字 */
const ALLOWED = new Set([
  // JS/Vue 内置
  'true', 'false', 'null', 'undefined', 'NaN', 'Infinity',
  'Math', 'Date', 'JSON', 'Number', 'String', 'Boolean', 'Array', 'Object', 'Set', 'Map',
  'window', 'document', 'console', 'localStorage', '$event', '$props', '$attrs', '$slots', '$refs',
  // SVG/HTML 常见 camelCase 属性
  'viewBox', 'preserveAspectRatio', 'strokeWidth', 'strokeLinecap', 'strokeLinejoin',
  'fillRule', 'clipRule', 'textAnchor', 'stopColor', 'strokeDasharray', 'strokeDashoffset',
])

/** 取 <script setup> 里可被模板引用的顶层名字 */
export function scriptBindings(script) {
  const names = new Set()
  const add = (n) => n && names.add(n)
  for (const m of script.matchAll(/^\s*(?:const|let|var)\s+([A-Za-z_$][\w$]*)/gm)) add(m[1])
  for (const m of script.matchAll(/^\s*(?:async\s+)?function\s+([A-Za-z_$][\w$]*)/gm)) add(m[1])
  // import { a, b as c } from '...'
  for (const m of script.matchAll(/import\s*\{([^}]*)\}\s*from/g)) {
    for (const part of m[1].split(',')) {
      const seg = part.trim()
      if (!seg) continue
      const as = seg.split(/\s+as\s+/)
      add((as[1] || as[0]).trim())
    }
  }
  // import Foo from '...'
  for (const m of script.matchAll(/import\s+([A-Za-z_$][\w$]*)\s+from/g)) add(m[1])
  // defineEmits(['pick', 'pickSticker']) → 模板里 $emit 事件名也算已定义
  const emits = script.match(/defineEmits\(\s*\[([^\]]*)\]/)?.[1] || ''
  for (const m of emits.matchAll(/'([^']+)'/g)) {
    add(m[1])
    add(m[1].replace(/[A-Z]/g, (c) => '-' + c.toLowerCase())) // pickSticker → pick-sticker
  }
  return names
}

/**
 * 模板里出现过的标识符。
 *
 * 关键点：**动态属性值（v-if / :class / @click / {{ }}）里的标识符必须保留**，
 * 只把「文本节点里的字符串字面量」和「静态属性值」剥掉，否则 @click="foo" 会被
 * 整段当字符串丢掉、校验器就永远发现不了被删掉的函数（自检用例专门盯这一点）。
 */
export function templateIdentifiers(template) {
  const src = template.replace(/<!--[\s\S]*?-->/g, ' ')
  const parts = []
  const re = /([A-Za-z_:@#.\[\]-][\w:.\-\[\]@#]*)\s*=\s*("[^"]*"|'[^']*')/g
  let last = 0
  let m
  while ((m = re.exec(src))) {
    const attr = m[1]
    const isDynamic = attr.startsWith('v-') || attr.startsWith(':') || attr.startsWith('@') || attr.startsWith('#')
    // 属性名本身（如 v-if / :class）保留，供下面的白名单过滤
    parts.push(attr)
    // 动态绑定的值要参与识别；静态属性值（class="row"）不参与
    if (isDynamic) parts.push(m[2])
    last = re.lastIndex
  }
  parts.push(src.slice(last))

  const ids = new Set()
  for (const chunk of parts) {
    // 插值里的表达式：单独再取一次（{{ }} 在上面的属性扫描里不会被拆出来）
    for (const m2 of chunk.matchAll(/\{\{([\s\S]*?)\}\}/g)) parts.push(m2[1])
  }
  const text = parts.join(' ')
  for (const m of text.matchAll(/[A-Za-z_$][\w$]*/g)) {
    const name = m[0]
    const before = text[m.index - 1]
    const after = text[m.index + name.length]
    if (before === '.') continue
    if (before === '?' && after === '.') continue
    if (after === ':') continue // 对象字面量的键
    if (before === '-' || before === ':') continue // 指令名片段
    ids.add(name)
  }
  return ids
}

/** 校验一个 SFC 文件，返回「模板引用了但 script 里没有」的名字 */
export function undefinedTemplateBindings(source) {
  const script = source.match(/<script setup[^>]*>([\s\S]*?)<\/script>/)?.[1] || ''
  const template = source.match(/<template>([\s\S]*)<\/template>/)?.[1] || ''
  const bindings = scriptBindings(script)
  const missing = []
  for (const name of templateIdentifiers(template)) {
    if (ALLOWED.has(name) || bindings.has(name)) continue
    // HTML 标签与 Vue 指令关键字
    if (/^[a-z][a-z0-9-]*$/.test(name)) continue
    if (name.length === 1) continue // 文本里的 (S)、(R) 这类单字母提示
    if (/^[A-Z][A-Z0-9]*$/.test(name)) continue // 全大写：标签/缩写噪声
    if (['v', 'key', 'ref', 'is', 'slot', 'template'].includes(name)) continue
    missing.push(name)
  }
  return [...new Set(missing)]
}

const FILES = [
  '../src/components/EmojiPicker.vue',
  '../src/components/ChatWindow.vue',
  '../src/components/SettingsModal.vue',
  '../src/components/ListPanel.vue',
  '../src/components/SideBar.vue',
  '../src/components/TitleBar.vue',
  '../src/components/RecipientPicker.vue',
  '../src/components/Avatar.vue',
  '../src/components/ImageViewer.vue',
  '../src/components/ScreenshotOverlay.vue',
]

for (const f of FILES) {
  test(`模板里引用的名字都在 <script setup> 里定义了：${f.split('/').pop()}`, () => {
    const src = readFileSync(new URL(f, import.meta.url), 'utf8')
    const missing = undefinedTemplateBindings(src)
    assert.deepEqual(missing, [], `模板引用了未定义的名字：${missing.join(', ')}`)
  })
}

test('校验器本身能发现被删掉的函数（防回归自检）', () => {
  const broken = `<script setup>
import { ref } from 'vue'
const n = ref(0)
function openThing() { n.value++ }
</script>
<template><button @click="openThing">x</button><button @click="closeThing">y</button></template>`
  assert.deepEqual(undefinedTemplateBindings(broken), ['closeThing'])
})
