/**
 * 界面状态的写侧契约（架构评审候选 6）。
 *
 * 病灶：`store` 的 6 个字段有第二个写者 —— 组件直接 `store.x = …`。字段的**形状**于是成了
 * 跨组件 interface：改字段名要全仓搜索，写侧没有单一归属。现在写一律走 `store.js` 导出的
 * action，组件只读。
 *
 * 两条一起守：
 *   ① 源码扫描：组件里不许出现 `store.x =`（白名单为空 —— 「组件只读」是硬不变量）；
 *   ② 行为：每个 action 写对字段、形状对（真 store + mockIPC 驱动）。
 * 扫描是**契约**测试：改 action 的实现不会让它红，往组件里塞回一句写入才会。
 */
import { readFileSync, readdirSync } from 'node:fs'
import { test, before, beforeEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore } from './helpers/store-harness.mjs'

const componentsDir = new URL('../src/components/', import.meta.url)
const componentFiles = readdirSync(componentsDir).filter((f) => f.endsWith('.vue'))

test('组件只读 store：写入必须走 store.js 的 action', () => {
  // 先确认扫描面没扫空（否则这条断言会永远绿）
  assert.ok(componentFiles.length >= 10, `只扫到 ${componentFiles.length} 个组件，路径写错了？`)
  const sources = [
    ...componentFiles.map((f) => [
      `src/components/${f}`,
      readFileSync(new URL(f, componentsDir), 'utf8'),
    ]),
    ['src/App.vue', readFileSync(new URL('../src/App.vue', import.meta.url), 'utf8')],
  ]
  for (const [name, text] of sources) {
    text.split('\n').forEach((line, i) => {
      assert.doesNotMatch(
        line,
        /store\.[A-Za-z_$][\w$]*\s*=[^=]/,
        `${name}:${i + 1} 直接写了 store 字段，改成调 store.js 的 action：${line.trim()}`,
      )
    })
  }
})

let h
before(async () => { h = await startStore() })
beforeEach(async () => { await h.reset() })

test('setSettingsOpen / setFirstRun：写对字段', () => {
  h.mod.setSettingsOpen(true)
  assert.equal(h.store.settingsOpen, true)
  h.mod.setSettingsOpen(false)
  assert.equal(h.store.settingsOpen, false)

  h.mod.setFirstRun(true)
  assert.equal(h.store.firstRun, true)
  h.mod.setFirstRun(false)
  assert.equal(h.store.firstRun, false)
})

test('setLocate / clearLocate：定位请求的设置与消费', () => {
  const payload = { key: '10.0.0.9', pkt: 42, query: '报表' }
  h.mod.setLocate(payload)
  assert.deepEqual(h.store.locate, payload)

  h.mod.clearLocate()
  assert.equal(h.store.locate, null, '定位消费完要清空，否则会重复滚动')
})

test("setDragHover：key 与空值（两个空引号 = 不在任何联系人上）", () => {
  h.mod.setDragHover('10.0.0.9')
  assert.equal(h.store.dragHoverKey, '10.0.0.9')

  h.mod.setDragHover('')
  assert.equal(h.store.dragHoverKey, '')
  // 事件里 position 可能缺失 → 调用方直接传 undefined，不该写出 undefined 进字段
  h.mod.setDragHover(undefined)
  assert.equal(h.store.dragHoverKey, '')
})

test('setStickerIndex：list → file / cacheFile 的映射只做一份', () => {
  h.mod.setStickerIndex([
    { file: 'a.png', cacheFile: null },
    { file: 'b.webm', cacheFile: 'cache-b.webm' },
  ])
  assert.deepEqual(h.store.emojiFiles, ['a.png', 'b.webm'])
  assert.deepEqual(h.store.emojiCacheFiles, ['cache-b.webm'], 'cacheFile 为空的不进缓存名集合')

  h.mod.setStickerIndex([])
  assert.deepEqual(h.store.emojiFiles, [])
  assert.deepEqual(h.store.emojiCacheFiles, [])
})
