/**
 * 截图落盘的纯函数（无 DOM、无 Tauri、node --test 直接跑）。
 *
 * 为什么放在前端这一处：后端只回答「系统的图片根目录在哪」—— 那个位置记在
 * `~/.config/user-dirs.dirs` 的 XDG_PICTURES_DIR 里，可能被用户改成任何绝对路径，
 * 前端拼 `~` 或 `~/Pictures` 都是猜。真正的目录解析、命名与重名避让全在本文件，
 * 只此一份（后端写盘只管「不许覆盖」这一件事）。
 *
 * 为什么不弹原生保存框：遮罩窗口是置顶、无边框、整屏的 Wayland 表面，从它上面
 * 挂原生 GTK 模态框，合成器会把对话框另开成一个顶层窗口 —— 真机表现是「对话框
 * 看不见 + 遮罩整层消失 + 只剩活桌面」（v0.1.17 的保存按钮就是这么坏的）。
 */

/** 本应用在图片目录下的专属子目录名 */
export const SHOT_DIR_NAME = 'OpenIPMsg 截图'

/** 目录分隔符：Windows 盘符/反斜杠路径用 `\`，其余一律 `/` */
function sep(dir) {
  return /^[A-Za-z]:/.test(dir) || (dir.includes('\\') && !dir.includes('/')) ? '\\' : '/'
}

/** 图片根目录 → 本应用截图目录（去掉多余尾部分隔符；空根目录时只留子目录名） */
export function shotsDir(picturesBase, name = SHOT_DIR_NAME) {
  const base = String(picturesBase ?? '').replace(/[\\/]+$/, '')
  return base ? `${base}${sep(base)}${name}` : name
}

/** 目录 + 文件名 → 完整路径（目录已有尾部分隔符就不重复加） */
export function joinPath(dir, name) {
  const d = String(dir ?? '')
  if (!d) return String(name ?? '')
  if (/[\\/]$/.test(d)) return d + name
  return `${d}${sep(d)}${name}`
}

/** 完整路径 → 文件名（弹提示时只说名字，别把整条路径糊到屏幕中间） */
export function baseNameOf(path) {
  const s = String(path ?? '')
  const at = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'))
  return at >= 0 ? s.slice(at + 1) : s
}

const pad = (n, width = 2) => String(n).padStart(width, '0')

/**
 * 本地时间 → `截图-YYYYMMDD-HHMMSS.png`。
 *
 * 用 `getFullYear/getMonth/...`（**本地时区**）而不是 toISOString：文件名是给人
 * 看的，得和用户墙上的钟对上（旧实现用 ISO 的 UTC 时间，晚上 8 点拍的图叫 12 点）。
 */
export function shotFileName(date = new Date()) {
  const d = date instanceof Date ? date : new Date(date)
  const stamp =
    `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}` +
    `-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`
  return `截图-${stamp}.png`
}

/** 重名时最多往后排多少个（防止 taken 里塞满同名前缀时死循环） */
export const MAX_DUPLICATES = 9999

/**
 * 重名避让：`截图-….png` 已存在 → `-2` → `-3` …（后缀插在扩展名之前）。
 *
 * 只做「不覆盖」的一半：真正的兜底在后端 —— 写盘用 create_new，目录列表与写入
 * 之间被别人占了同名文件时返回 E_SAVE_EXISTS，调用方据此再排下一个名字。
 */
export function freeName(name, taken = []) {
  const used = new Set((taken ?? []).map((n) => String(n)))
  const base = String(name ?? '')
  if (!used.has(base)) return base
  const dot = base.lastIndexOf('.')
  const stem = dot > 0 ? base.slice(0, dot) : base
  const ext = dot > 0 ? base.slice(dot) : ''
  for (let i = 2; i <= MAX_DUPLICATES + 1; i++) {
    const candidate = `${stem}-${i}${ext}`
    if (!used.has(candidate)) return candidate
  }
  throw new Error(`重名过多：${base}`)
}
