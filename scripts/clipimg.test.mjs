// node --test scripts/ —— 纯函数单测（不依赖 Tauri 运行时）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { b64ToBlob, pendingImgFromB64 } from '../src/lib/clipimg.js'

test('b64ToBlob 还原字节且带正确 MIME', async () => {
  const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a])
  const b64 = Buffer.from(bytes).toString('base64')
  const blob = b64ToBlob(b64, 'image/png')
  assert.equal(blob.type, 'image/png')
  assert.deepEqual(new Uint8Array(await blob.arrayBuffer()), bytes)
})

test('b64ToBlob 缺省 MIME 用 image/png', () => {
  const blob = b64ToBlob(Buffer.from([1, 2, 3]).toString('base64'))
  assert.equal(blob.type, 'image/png')
})

test('pendingImgFromB64 组装待发送图：尺寸缺省时取解码长度', () => {
  const bytes = new Uint8Array(1024)
  const b64 = Buffer.from(bytes).toString('base64')
  const p = pendingImgFromB64(b64, 'image/png')
  assert.equal(p.b64, b64)
  assert.equal(p.mime, 'image/png')
  assert.equal(p.size, 1024, '无 size 参数时按实际字节数')
  assert.ok(p.blob, '提供 Blob 供前端生成预览 URL')
})

test('pendingImgFromB64 接受后端下发的尺寸', () => {
  const b64 = Buffer.from('abc').toString('base64')
  const p = pendingImgFromB64(b64, 'image/png', 999)
  assert.equal(p.size, 999)
})

test('非法 base64 直接抛出（交由调用方提示）', () => {
  assert.throws(() => b64ToBlob('@@not-base64@@'))
})

import { b64ToBytes } from '../src/lib/clipimg.js'

test('base64 → 字节数组（剪贴板图片与截图共用的转换）', () => {
  // "AQID" = [1,2,3]
  assert.deepEqual(Array.from(b64ToBytes('AQID')), [1, 2, 3])
  assert.deepEqual(Array.from(b64ToBytes('')), [])
})