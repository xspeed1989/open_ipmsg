/**
 * 自定义表情包（表情库）前端纯逻辑。
 *
 * 这里只放**不依赖 Tauri / DOM** 的纯函数：列表归一化、包预览文案与默认勾选、
 * 待发送附件与表情条目的匹配、导出文件名生成等，便于用 node --test 直接测。
 * 真正的读盘、校验、落盘都在后端 emoji.rs（见 src/lib/ipc.js 的命令封装）。
 */

/** 单张表情上限，与后端 emoji.rs 的 MAX_EMOJI_BYTES 保持一致（16MB） */
export const MAX_EMOJI_BYTES = 16 * 1024 * 1024

/** 一次「导入图片」最多选多少个文件（后端逐个校验，这里只做交互层兜底） */
export const MAX_IMPORT_BATCH = 500

/** 支持导入的图片扩展名（真正的判定在后端按文件头魔数做） */
export const IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp']

/** 包文件名后缀：仍是标准 zip，双击可用系统解压工具打开 */
export const PACK_EXT = 'ipmojis'

/**
 * 单个表情条目的 id 是否可用：只要求「非空 + 文件名安全」。
 *
 * 不锁死后端的 id 方案（形如 `<毫秒时间戳十六进制><两位序号>`）：一旦后端换了
 * 生成方式，过严的校验会把条目静默丢掉，界面直接显示「还没有自定义表情」。
 */
function validId(id) {
  return typeof id === 'string' && id.length > 0 && id.length <= 64 && /^[A-Za-z0-9_-]+$/.test(id)
}

/**
 * 归一化后端返回的表情列表：
 * - 过滤掉缺 id / 缺文件名的脏数据（索引被手工改坏时也不能让面板崩掉）
 * - 名字兜底、size 归一为非负整数
 * @param {any} raw 后端 list_emojis 的返回（`{ emojis: [...] }` 或直接数组）
 * @returns {Array<{id:string,name:string,file:string,abs:string,cacheFile:string,size:number,addedAt:number}>}
 */
export function normalizeEmojis(raw) {
  const list = Array.isArray(raw) ? raw : raw?.emojis
  if (!Array.isArray(list)) return []
  const seen = new Set()
  const out = []
  for (const it of list) {
    if (!it || typeof it !== 'object') continue
    const id = String(it.id ?? '')
    const file = String(it.file ?? '')
    if (!validId(id) || !file || seen.has(id)) continue
    seen.add(id)
    out.push({
      id,
      name: String(it.name ?? '').trim() || '表情',
      file,
      abs: String(it.abs ?? ''),
      cacheFile: String(it.cache_file ?? ''),
      size: Number.isFinite(Number(it.size)) ? Math.max(0, Number(it.size)) : 0,
      addedAt: Number.isFinite(Number(it.added_at)) ? Number(it.added_at) : 0,
    })
  }
  return out
}

/**
 * 导入结果的用户可读摘要（成功 n 个 + 跳过的原因合并去重）。
 * @param {{imported?:Array, skipped?:Array<{name?:string,reason?:string}>}} res
 * @param {(key:string, vars?:object)=>string} t i18n 取词函数
 */
export function importSummary(res, t) {
  const ok = Array.isArray(res?.imported) ? res.imported.length : Number(res?.imported) || 0
  const skipped = Array.isArray(res?.skipped) ? res.skipped : []
  if (!skipped.length) return { text: t('emoji.imported', { n: ok }), level: 'info' }
  const reasons = []
  for (const s of skipped) {
    const r = String(s?.reason ?? '').trim()
    if (r && !reasons.includes(r)) reasons.push(r)
  }
  return {
    text: t('emoji.importSkipped', { n: ok, m: skipped.length, why: reasons.join('；') }),
    level: 'warn',
  }
}

/** 包预览的一行状态文案（不可导入时给出原因） */
export function packItemState(item, t) {
  if (!item || typeof item !== 'object') return { kind: 'bad', text: '' }
  if (item.problem === '表情库里已有相同图片' || item.duplicate) {
    return { kind: 'dup', text: t('emoji.packDup') }
  }
  if (item.problem) return { kind: 'bad', text: String(item.problem) }
  return { kind: 'ok', text: '' }
}

/** 包预览汇总：`共 n 张，可导入 m 张` */
export function packSummary(inspect, t) {
  const items = Array.isArray(inspect?.items) ? inspect.items : []
  const importable = items.filter((i) => !i?.problem && i?.kind).length
  return {
    total: items.length,
    importable,
    text: t('emoji.packTotal', { n: items.length, m: importable }),
  }
}

/**
 * 包预览的默认勾选：只勾可导入项（重复项与非法项默认不勾，但用户仍可手动勾）。
 * @returns {Set<string>} 勾选的包内相对路径
 */
export function defaultPackSelection(inspect) {
  const items = Array.isArray(inspect?.items) ? inspect.items : []
  return new Set(items.filter((i) => !i?.problem && i?.kind).map((i) => i.file))
}

/** 导出文件名：表情包-20250824.ipmojis（不含路径，交给保存对话框） */
export function packFileName(date = new Date()) {
  const p = (n) => String(n).padStart(2, '0')
  return `表情包-${date.getFullYear()}${p(date.getMonth() + 1)}${p(date.getDate())}.${PACK_EXT}`
}

/** 文件名（或路径）是否像可导入的图片（真正的判定在后端） */
export function looksLikeImage(name) {
  const s = String(name ?? '')
  const i = s.lastIndexOf('.')
  if (i <= 0) return false
  return IMAGE_EXTS.includes(s.slice(i + 1).toLowerCase())
}

/**
 * 待发送附件里能直接入库的本地路径。
 * 剪贴板图片只有 base64（后端未落盘）时返回空串 —— 此时不能入库。
 * @param {{kind?:string,path?:string}} item
 */
export function attachPath(item) {
  if (!item || item.kind !== 'img') return ''
  return typeof item.path === 'string' ? item.path : ''
}

/**
 * 从消息附件的下载状态判断「添加到表情」此刻是否可用。
 * 已下载（本地有路径）→ 可直接入库；仅公告未下载 → 需要先下载再收纳。
 * @param {{name?:string,path?:string,state?:string,isDir?:boolean}} file
 */
export function addToEmojiPlan(file) {
  if (!file || file.isDir || !looksLikeImage(file.name)) return { ok: false, reason: 'notImage' }
  if (typeof file.path === 'string' && file.path) return { ok: true, needDownload: false }
  if (file.state === 'done') return { ok: false, reason: 'noPath' }
  return { ok: true, needDownload: true }
}

/** 表情缩略显示阈值（聊天气泡内最长边上限，CSS 里 .emoji-sticker 同值） */
export const STICKER_DISPLAY_MAX = 150

/**
 * 判断一条消息里的某个附件是否应按「自定义表情」缩略渲染：
 * 发送/接收时都按文件名与本地表情库比对（命中即缩略，避免 16MB 大图撑满气泡）。
 * @param {{name?:string}} file
 * @param {Set<string>|Array<string>} files 表情库文件名集合
 */
export function isStickerFile(file, files) {
  const name = String(file?.name ?? '')
  if (!name) return false
  const set = files instanceof Set ? files : new Set(files || [])
  return set.has(name)
}
