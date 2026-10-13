// node --test scripts/ —— 聊天里图片/视频显示尺寸（媒体框）的纯函数契约
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { fitMedia, MEDIA_BOX_W, MEDIA_BOX_H } from '../src/lib/media.js'

test('比例不变，缩进媒体框内（真机那张表情图 1506×1216）', () => {
  const fit = fitMedia(1506, 1216)
  assert.equal(fit.h, MEDIA_BOX_H, '高度顶到上限')
  assert.equal(fit.w, 248, '宽度按比例（1506/1216 × 200）')
  assert.ok(fit.w <= MEDIA_BOX_W && fit.h <= MEDIA_BOX_H)
})

test('宽图被宽度限住、高图被高度限住，比例都不变', () => {
  const wide = fitMedia(4000, 1000)
  assert.deepEqual(wide, { w: MEDIA_BOX_W, h: 65 })
  const tall = fitMedia(1000, 4000)
  assert.deepEqual(tall, { w: 50, h: MEDIA_BOX_H })
})

test('小图不放大（原尺寸显示，别糊）', () => {
  assert.deepEqual(fitMedia(64, 48), { w: 64, h: 48 })
  assert.deepEqual(fitMedia(260, 200), { w: 260, h: 200 })
})

test('尺寸未知（还没加载出来）返回 null，交给 CSS 兜底', () => {
  for (const bad of [[0, 0], [undefined, 100], [NaN, 100], [-1, 5], [null, null]]) {
    assert.equal(fitMedia(...bad), null, `${bad} 应当给 null`)
  }
})

// 接线（源码契约）：尺寸算出来必须真的写进元素属性，否则气泡又被原始像素撑开
import { readFileSync } from 'node:fs'
const chat = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')

test('图片/视频都挂了加载回调，并把尺寸写回元素属性', () => {
  assert.match(chat, /:width="f\._dw" :height="f\._dh"/, 'img 要带显式宽高')
  assert.match(chat, /@load="onImgLoad\(f, \$event\)"/)
  assert.match(chat, /@loadeddata="onVideoLoad\(f, \$event\)"/)
  assert.equal(
    (chat.match(/f\._dw = fit\.w/g) || []).length,
    2,
    '图片与视频两个回调都要写回尺寸'
  )
})

test('媒体框上限与 CSS 一致（改一处要同步另一处）', () => {
  assert.match(chat, /max-width:\s*min\(260px, 100%\)/)
  assert.match(chat, /max-height:\s*200px/)
  assert.equal(MEDIA_BOX_W, 260)
  assert.equal(MEDIA_BOX_H, 200)
})
