import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const storeSource = readFileSync(new URL('../src/store.js', import.meta.url), 'utf8')
const chatSource = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')

test('重复打开当前会话仍发出新的滚动请求', async () => {
  const body = storeSource.match(/export async function openChat\(key\) \{([\s\S]*?)\n\}/)[1]
  const store = { activeKey: 'peer', chatOpenVersion: 0 }
  const calls = []
  const open = new Function('store', 'ensureChat', 'markReadFor', `return async (key) => {${body}}`)(
    store, async (key) => calls.push(['load', key]), async (key) => calls.push(['read', key]),
  )
  await open('peer')
  assert.equal(store.chatOpenVersion, 1)
  await open('peer')
  assert.equal(store.chatOpenVersion, 2)
  assert.equal(store.activeKey, 'peer')
  assert.deepEqual(calls, [['load', 'peer'], ['read', 'peer'], ['load', 'peer'], ['read', 'peer']])
})

test('打开会话重置历史浏览状态，窗口恢复布局后补做滚动', () => {
  assert.match(chatSource, /watch\(\(\) => \[store\.activeKey, store\.chatOpenVersion\], \(\) => \{\s*autoBottom = true\s*scrollBottom\(\)/)
  assert.match(chatSource, /new ResizeObserver\(\(\) => scrollBottom\(\)\)/)
  assert.match(chatSource, /onCleanup\(\(\) => observer\.disconnect\(\)\)/)
})

test('托盘和通知入口均通过 openChat 发出定位请求', () => {
  for (const event of ['openUnread', 'openChat']) {
    const handler = storeSource.split(`await ipc.listenEvent(ipc.EVT.${event},`)[1].split("console.error")[0]
    assert.match(handler, /await openChat\(key\)/)
  }
})
