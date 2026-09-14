/**
 * 后端错误 → 界面文案。
 *
 * 后端的错误串是给人看的**中文**（日志、`err.contains("密码错误")` 这类测试都
 * 依赖它），但直接甩进 UI 会让英文界面冒中文。约定：会呈现给用户的错误在原文
 * 前面加一个稳定错误码，形如 `E_UNLOCK_BAD_PASSWORD|密码错误`，其中：
 *
 * - `|` 之前是错误码，前端按码查 i18n；译文里可用 `{e}` 引用 `|` 之后的细节；
 * - `|` 之后是原文/细节，保留给日志与排障，i18n 表决定要不要展示。
 *
 * 没有码的错误走兜底文案：中文界面上带出原文便于排查，**非中文界面下不透出
 * 中文原文**（英文界面出现中文正是这一层要解决的问题）。
 */
import { locale, t } from './i18n.js'

/** 界面会遇到的错误码清单（与 Rust 侧写入的码一一对应） */
export const ERROR_CODES = [
  'E_UNLOCK_NOT_FOUND',
  'E_UNLOCK_DISABLED',
  'E_UNLOCK_BAD_PASSWORD',
  'E_RECALL_NOT_FOUND',
  'E_RECALL_NOT_MINE',
  'E_RECALL_HAS_FILES',
  'E_SEND_EMPTY',
  'E_NICKNAME_REQUIRED',
  'E_IMAGE_UNSUPPORTED',
  'E_IMAGE_TOO_BIG',
  'E_READ_FAILED',
  'E_FILE_MISSING',
  'E_SAVE_FAILED',
  'E_CLIPBOARD_FAILED',
  'E_CLIPBOARD_TIMEOUT',
  'E_VIEWER_OPEN_FAILED',
  'E_IMPORT_FAILED',
  'E_IMPORT_OPEN_FAILED',
  'E_IMPORT_NOT_IPMSG',
  'E_IMPORT_READ_FAILED',
  'E_PERSIST_FAILED',
  // 表情包导入预检（emoji.rs 逐条给出不可导入的原因）
  'E_PACK_BAD_PATH',
  'E_PACK_FILE_MISSING',
  'E_PACK_BAD_FORMAT',
  'E_PACK_TOO_BIG',
  'E_PACK_TOTAL_TOO_BIG',
  'E_PACK_DUPLICATE',
  // 截图（screenshot.rs 的 ShotErr::code()，历史遗留的无前缀风格）
  'CAPTURE_FAILED',
  'CAPTURE_BUSY',
  'PORTAL_MISSING',
  'PORTAL_DENIED',
  'PORTAL_TIMEOUT',
  'DECODE_FAILED',
  'MAC_PERMISSION',
]

// 码格式：大写字母开头的大写/数字/下划线串 + `|` + 细节。
// 不强制 E_ 前缀是为了兼容截图模块既有的 CAPTURE_FAILED 等码（见 screenshot.rs）
const CODE_RE = /^([A-Z][A-Z0-9_]{2,})\|([\s\S]*)$/
const CJK = /[\u4e00-\u9fff]/

/** 把任意抛出物转成文本（Error / 字符串 / 其它） */
function toText(e) {
  if (e == null) return ''
  if (typeof e === 'string') return e
  if (typeof e.message === 'string') return e.message
  return String(e)
}

/** 取出 `|` 之后的细节；顺带把认不出的错误码剥掉，别让用户看到 E_XXX */
function detailOf(raw) {
  const matched = CODE_RE.exec(raw.trim())
  return (matched ? matched[2] : raw).trim()
}

/**
 * 取出错误码（没有则返回空串）。
 * 调用方需要「按码分流」时用它，别去比对中文文案 —— 文案一改就静默失效。
 */
export function errorCode(e) {
  const matched = CODE_RE.exec(toText(e).trim())
  return matched ? matched[1] : ''
}

/**
 * 渲染错误文案。
 * @param {unknown} e invoke 抛出的错误（通常是字符串或 Error）
 * @param {string} [fallbackKey] 无码（或码没有译文）时的兜底文案 key
 */
export function describeError(e, fallbackKey = 'err.fallback') {
  const raw = toText(e)
  const fallback = t(fallbackKey)
  if (!raw.trim()) return fallback

  const matched = CODE_RE.exec(raw.trim())
  if (matched) {
    const key = `err.${matched[1]}`
    const text = t(key, { e: matched[2].trim() })
    if (text !== key) return text
  }

  const detail = detailOf(raw)
  if (!detail) return fallback
  // 中文界面保留原文（后端错误本来就是中文，排障靠它）；
  // 其它语言下宁可只说「操作失败」，也不让界面冒出中文
  if (locale.value !== 'zh-CN' && CJK.test(detail)) return fallback
  return t('err.withDetail', { e: detail })
}
