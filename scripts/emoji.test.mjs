// node --test scripts/ —— 自定义表情包纯逻辑单测（不依赖 Tauri / DOM）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  MAX_EMOJI_BYTES, MAX_IMPORT_BATCH, normalizeEmojis, importSummary, packItemState,
  packSummary, defaultPackSelection, packFileName, looksLikeImage, attachPath,
  addToEmojiPlan, isStickerFile, STICKER_DISPLAY_MAX, PACK_EXT,
} from '../src/lib/emoji.js'

/** 极简取词：直接回显 key，便于断言参数确实被传进去了 */
const t = (key, vars) => (vars ? `${key}:${JSON.stringify(vars)}` : key)

/* ---------------- 列表归一化 ---------------- */

test('normalizeEmojis 兼容 {emojis:[...]} 与裸数组，并保留后端下发的字段', () => {
  const raw = {
    emojis: [
      { id: 'a1b2', name: '开心', file: 'a1b2.png', abs: '/d/emojis/a1b2.png', size: 12, added_at: 1700000000, cache_file: 'ipmsgclip_s_1_0.png' },
    ],
  }
  const [e] = normalizeEmojis(raw)
  assert.equal(e.id, 'a1b2')
  assert.equal(e.name, '开心')
  assert.equal(e.abs, '/d/emojis/a1b2.png', '缩略图要用绝对路径调 read_image_data')
  assert.equal(e.cacheFile, 'ipmsgclip_s_1_0.png')
  assert.equal(e.addedAt, 1700000000)
  assert.equal(normalizeEmojis([{ id: 'ff00', file: 'x.png' }]).length, 1)
})

test('normalizeEmojis 过滤脏数据而不是让面板崩掉', () => {
  const list = normalizeEmojis({
    emojis: [
      null,
      'nope',
      { id: '', file: 'x.png' }, // 缺 id
      { id: 'zz', file: '' }, // 缺文件名
      { id: 'bad id!', file: 'x.png' }, // id 含不安全字符
      { id: 'a'.repeat(65), file: 'x.png' }, // id 过长
      { id: 'ab12', file: 'a.png' },
      { id: 'ab12', file: 'dup.png' }, // 重复 id 只留第一条
    ],
  })
  assert.equal(list.length, 1)
  assert.equal(list[0].file, 'a.png')
  assert.equal(normalizeEmojis(null).length, 0)
  assert.equal(normalizeEmojis({}).length, 0)
})

test('normalizeEmojis 不锁死后端 id 方案（短 id / 非十六进制都要能过）', () => {
  // 后端 id 目前是「毫秒时间戳十六进制 + 两位序号」，但界面不该依赖这个格式：
  // 过严的校验会把条目静默丢掉，面板直接变空态
  const list = normalizeEmojis({
    emojis: [
      { id: '01', file: 'a.png' },
      { id: 'zz-9_x', file: 'b.png' },
      { id: 'f'.repeat(40), file: 'c.png' },
    ],
  })
  assert.equal(list.length, 3)
})

test('normalizeEmojis 给名字与大小兜底', () => {
  const [a] = normalizeEmojis([{ id: 'aa11', file: 'a.png', name: '   ' }])
  assert.equal(a.name, '表情')
  const [b] = normalizeEmojis([{ id: 'aa12', file: 'b.png', size: -5 }])
  assert.equal(b.size, 0)
})

/* ---------------- 导入结果提示 ---------------- */

test('importSummary 全部成功时只说导入数量', () => {
  const r = importSummary({ imported: [{}, {}], skipped: [] }, t)
  assert.equal(r.level, 'info')
  assert.match(r.text, /"n":2/)
})

test('importSummary 有跳过项时合并去重原因', () => {
  const r = importSummary({
    imported: [{}],
    skipped: [
      { name: 'a', reason: '不是支持的图片格式' },
      { name: 'b', reason: '不是支持的图片格式' },
      { name: 'c', reason: '超过 16MB 上限' },
    ],
  }, t)
  assert.equal(r.level, 'warn')
  assert.match(r.text, /"n":1/)
  assert.match(r.text, /"m":3/)
  assert.match(r.text, /不是支持的图片格式；超过 16MB 上限/, '同一原因只出现一次')
})

test('importSummary 兼容后端只回数字的 imported', () => {
  const r = importSummary({ imported: 3, skipped: [] }, t)
  assert.match(r.text, /"n":3/)
})

/* ---------------- 表情包预览 ---------------- */

const INSPECT = {
  name: '朋友的包',
  items: [
    { file: 'images/001_a.png', name: 'a', kind: 'png', duplicate: false, problem: null },
    { file: 'images/002_b.gif', name: 'b', kind: 'gif', duplicate: true, problem: '表情库里已有相同图片' },
    { file: '../evil.png', name: 'evil', kind: null, duplicate: false, problem: '包内路径不合法' },
  ],
}

test('packSummary 只把无问题的条目算作可导入', () => {
  const s = packSummary(INSPECT, t)
  assert.equal(s.total, 3)
  assert.equal(s.importable, 1)
  assert.match(s.text, /"n":3/)
  assert.match(s.text, /"m":1/)
})

test('defaultPackSelection 默认勾选可导入项，跳过重复与非法项', () => {
  const sel = defaultPackSelection(INSPECT)
  assert.deepEqual([...sel], ['images/001_a.png'])
  assert.equal(defaultPackSelection(null).size, 0)
})

test('packItemState 区分重复 / 非法 / 正常', () => {
  assert.equal(packItemState(INSPECT.items[0], t).kind, 'ok')
  assert.equal(packItemState(INSPECT.items[1], t).kind, 'dup')
  assert.equal(packItemState(INSPECT.items[2], t).kind, 'bad')
  assert.equal(packItemState(INSPECT.items[2], t).text, '包内路径不合法')
})

test('packFileName 生成带日期的包名', () => {
  const n = packFileName(new Date(2025, 7, 24))
  assert.equal(n, `表情包-20250824.${PACK_EXT}`)
  assert.ok(n.endsWith('.ipmojis'), '后缀是 .ipmojis（内容仍是标准 zip）')
})

/* ---------------- 图片判定与收纳 ---------------- */

test('looksLikeImage 只看扩展名，大小写不敏感', () => {
  assert.ok(looksLikeImage('a.PNG'))
  assert.ok(looksLikeImage('/tmp/x.webp'))
  assert.ok(!looksLikeImage('a.txt'))
  assert.ok(!looksLikeImage('noext'))
  assert.ok(!looksLikeImage('.hidden'))
  assert.ok(!looksLikeImage(''))
})

test('attachPath 只认已落盘的图片', () => {
  assert.equal(attachPath({ kind: 'img', path: '/cache/a.png' }), '/cache/a.png')
  assert.equal(attachPath({ kind: 'img' }), '', '只有 base64 的剪贴板图不能入库')
  assert.equal(attachPath({ kind: 'file', path: '/tmp/a.png' }), '')
  assert.equal(attachPath(null), '')
})

test('addToEmojiPlan 区分「可直接入库」与「需先下载」', () => {
  const local = { name: 'a.png', path: '/cache/a.png', state: 'done' }
  assert.deepEqual(addToEmojiPlan(local), { ok: true, needDownload: false })

  const remote = { name: 'b.gif', state: 'idle' }
  assert.deepEqual(addToEmojiPlan(remote), { ok: true, needDownload: true })

  assert.equal(addToEmojiPlan({ name: 'notes.txt' }).ok, false, '非图片不给收纳入口')
  assert.equal(addToEmojiPlan({ name: 'folder.png', isDir: true }).ok, false)
  assert.equal(addToEmojiPlan({ name: 'c.png', state: 'done' }).ok, false, '已下载却没有路径：无从入库')
  assert.equal(addToEmojiPlan(null).ok, false)
})

/* ---------------- 缩略渲染判定 ---------------- */

test('isStickerFile 命中表情库文件名（含已发出的缓存副本名）', () => {
  const files = new Set(['0a1.png', 'ipmsgclip_s_7_0.png'])
  assert.ok(isStickerFile({ name: '0a1.png' }, files))
  assert.ok(isStickerFile({ name: 'ipmsgclip_s_7_0.png' }, files), '自己发出的表情用缓存副本名')
  assert.ok(!isStickerFile({ name: 'photo.png' }, files))
  assert.ok(!isStickerFile(null, files))
  assert.ok(isStickerFile({ name: '0a1.png' }, ['0a1.png']), '也接受数组')
})

test('常量与后端/样式保持一致', () => {
  assert.equal(MAX_EMOJI_BYTES, 16 * 1024 * 1024)
  assert.ok(MAX_IMPORT_BATCH >= 100)
  assert.equal(STICKER_DISPLAY_MAX, 150, '改这里要同步 ChatWindow 的 .emoji-sticker max-width')
})
