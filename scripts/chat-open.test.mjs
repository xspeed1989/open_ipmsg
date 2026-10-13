// node --test scripts/ —— 会话打开（openChat）与消息流滚动的回归用例
import { readFileSync } from 'node:fs'
import { test, before, beforeEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore, PEER } from './helpers/store-harness.mjs'

const chatSource = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')

// openChat 是 store 的用户动作，直接跨真 store 测（harness 后端是 mockIPC）。
// 原来这条用例靠正则抠 openChat 的函数体 + new Function 注入 ensureChat/markReadFor，
// 现在不需要了：真 store 本身就是可 import 的模块。
let h
before(async () => { h = await startStore() })
beforeEach(async () => { await h.reset() })

test('重复打开当前会话仍发出新的滚动请求', async () => {
  await h.mod.openChat(PEER)
  assert.equal(h.store.activeKey, PEER)
  assert.equal(h.store.chatOpenVersion, 1, '第一次打开递增')

  await h.mod.openChat(PEER)
  assert.equal(h.store.chatOpenVersion, 2, '同一会话再打开也要递增：界面据此重新定位最新消息')
})

test('打开会话重置历史浏览状态，窗口恢复布局后补做滚动', () => {
  assert.match(chatSource, /watch\(\(\) => \[store\.activeKey, store\.chatOpenVersion\], \(\) => \{\s*autoBottom = true\s*scrollBottom\(\)/)
  assert.match(chatSource, /new ResizeObserver\(\(\) => scrollBottom\(\)\)/)
  assert.match(chatSource, /onCleanup\(\(\) => observer\.disconnect\(\)\)/)
})

test('窗口回前台补定位；滚动有界重试；stale 布局不关掉自动到底', () => {
  // 唤起路径里 openChat()（触发滚动）先跑、w.show() 后跑，滚动落在窗口还没真正可见、
  // scrollHeight 还是 stale 的时刻 —— 靠「窗口聚焦」这一路补回来
  assert.match(
    chatSource,
    /watch\(\(\) => store\.windowFocused, \(focused\) => \{\s*if \(!focused\) return\s*autoBottom = true\s*scrollBottom\(\)/
  )
  // 布局没恢复时滚了也白滚：滚完校验是否真到底，没到底下一帧再补（有界，最多 3 帧）
  assert.match(chatSource, /if \(off > 2 && \+\+tries < 3\) requestAnimationFrame\(pin\)/)
  // 内容不溢出 = stale 布局，此时「离底部距离」是假的，不能拿它把 autoBottom 打成 false，
  // 否则之后每一次补定位都会被静默跳过
  assert.match(chatSource, /if \(el\.scrollHeight <= el\.clientHeight \+ 1\) return/)
})

// 「托盘和通知入口均通过 openChat 发出定位请求」那条源码正则断言已退役：
// 两个入口现在都走 dispatchEvent → open_chat effect，行为改由
// scripts/store-events.characterization.test.mjs 的 open-unread / open-chat 用例守着
// （断言切到了正确的会话，并且窗口 show/unminimize/set_focus 都发过）。
