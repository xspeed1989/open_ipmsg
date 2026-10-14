/**
 * 快捷键串纯函数：规范化 + 设置页录制。
 *
 * 规范形：修饰键（CmdOrCtrl/Ctrl/Alt/Shift，输出时按此顺序）+ 主键，用 '+' 连接，如 "Ctrl+Alt+A"。
 * 这个字符串直接存进配置，启动时交给 Tauri 的 global-shortcut 插件
 * （Windows/macOS/X11）或由 Rust 转成 portal 触发器（Wayland，见 shortcut.rs）；
 * 两侧都不需要 JS 再转换，所以这里只负责「录入即规范形」。
 */

/** 输出顺序：平台主修饰键在前（CmdOrCtrl → Ctrl → Alt → Shift） */
const MOD_ORDER = ['CmdOrCtrl', 'Ctrl', 'Alt', 'Shift']

const MOD_ALIASES = {
  ctrl: 'Ctrl', control: 'Ctrl',
  alt: 'Alt', option: 'Alt',
  shift: 'Shift',
  cmd: 'CmdOrCtrl', command: 'CmdOrCtrl', meta: 'CmdOrCtrl', super: 'CmdOrCtrl', win: 'CmdOrCtrl',
  cmdorctrl: 'CmdOrCtrl',
}

/** 主键别名 → 规范名 */
const KEY_ALIASES = {
  esc: 'Escape',
  escape: 'Escape',
  space: 'Space',
  spacebar: 'Space',
  enter: 'Enter',
  return: 'Enter',
  tab: 'Tab',
  backspace: 'Backspace',
  delete: 'Delete',
  del: 'Delete',
  insert: 'Insert',
  home: 'Home',
  end: 'End',
  pageup: 'PageUp',
  pagedown: 'PageDown',
  up: 'ArrowUp',
  down: 'ArrowDown',
  left: 'ArrowLeft',
  right: 'ArrowRight',
  // 规范名自身也要能解析：否则 normalizeCombo('Alt+ArrowUp') 返回 null，
  // 而 normalizeCombo('Alt+Up') 返回 'Alt+ArrowUp' —— 规范化不幂等，
  // 设置页会把一个 Rust 其实能注册的串标成「不可用」。
  arrowup: 'ArrowUp',
  arrowdown: 'ArrowDown',
  arrowleft: 'ArrowLeft',
  arrowright: 'ArrowRight',
  printscreen: 'PrintScreen',
}

/** 规范化组合键；非法返回 null */
export function normalizeCombo(input) {
  if (typeof input !== 'string') return null
  const parts = input.split('+').map((p) => p.trim()).filter(Boolean)
  if (!parts.length) return null
  const mods = new Set()
  let key = ''
  for (const p of parts) {
    const alias = MOD_ALIASES[p.toLowerCase()]
    if (alias) {
      mods.add(alias)
      continue
    }
    if (key) return null // 出现第二个非修饰键
    const named = KEY_ALIASES[p.toLowerCase()]
    if (named) key = named
    else if (/^f([1-9]|1\d|2[0-4])$/i.test(p)) key = p.toUpperCase()
    else if (/^[a-z0-9]$/i.test(p)) key = p.toUpperCase()
    // 未知键名不猜（"Foobar" / "F0" / "F99" / "Å" 一律判非法）：
    // isValidCombo 的职责就是「标出配置里存着的不可用快捷键」，
    // 放行未知键名会让设置页给一个 Rust 根本注册不了的串打绿灯。
    else return null
  }
  if (!key) return null
  return [...MOD_ORDER.filter((m) => mods.has(m)), key].join('+')
}

/** 有效全局热键 = 至少一个修饰键 + 主键（设置页用它标出配置里存着的非法值） */
export function isValidCombo(combo) {
  const n = normalizeCombo(combo)
  return !!n && n.split('+').length >= 2
}

/**
 * 从键盘事件取出规范主键名；取不到返回 null。
 *
 * 优先用 e.code（物理键位）：macOS 下按住 Option 再按字母，e.key 是合成字符
 * （Option+A → 'å'），只有 e.code（'KeyA'）才能还原出 Rust 侧可解析的组合键。
 * 没有 code 时退回 e.key，并拒绝一切非 ASCII 单字符（合成字符、'+' 等）。
 */
function keyFromEvent(e) {
  const code = typeof e?.code === 'string' ? e.code : ''
  if (/^Key[A-Z]$/.test(code)) return code.slice(3)
  if (/^Digit[0-9]$/.test(code)) return code.slice(5)
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code
  if (code === 'Space') return 'Space'

  const raw = typeof e?.key === 'string' ? e.key : ''
  if (raw === ' ') return 'Space' // 空格键的 e.key 就是 ' '
  const named = KEY_ALIASES[raw.toLowerCase()]
  if (named) return named
  if (/^[a-z0-9]$/i.test(raw)) return raw.toUpperCase()
  return null
}

/** 设置页录制：从键盘事件得到规范形；纯修饰键、无修饰键或取不到主键都返回 null */
export function comboFromEvent(e) {
  const raw = e?.key
  if (!raw) return null
  if (['Control', 'Alt', 'Shift', 'Meta', 'CapsLock', 'Dead', 'Unidentified'].includes(raw)) return null
  const key = keyFromEvent(e)
  if (!key) return null
  const mods = []
  if (e.ctrlKey) mods.push('Ctrl')
  if (e.altKey) mods.push('Alt')
  if (e.shiftKey) mods.push('Shift')
  if (e.metaKey) mods.push('CmdOrCtrl')
  if (!mods.length) return null
  return normalizeCombo([...mods, key].join('+'))
}

/**
 * 粗判 Wayland 会话：WebKitGTK 在 XWayland 下 UA 也可能含 X11，故只认显式 Wayland。
 * 设置页据此换用「由桌面环境绑定」的提示文案（Wayland 下全局热键走 portal）。
 */
export function isWaylandUA(ua = '') {
  return /wayland/i.test(ua)
}

/**
 * 应用内快捷键命中判定：配置里的规范形与本次按键的规范形相等才算命中。
 *
 * 与全局热键不同，应用内快捷键只在窗口收到 keydown 时有意义，所以直接比较
 * 规范化后的字符串即可；空串/不可用组合、取不到主键的按键一律返回 false。
 * 复用 comboFromEvent（它已处理 macOS 的 Option 合成字符与物理键位）。
 */
export function matchesLocalCombo(combo, e) {
  const target = normalizeCombo(combo)
  if (!target) return false
  const pressed = comboFromEvent(e)
  return !!pressed && pressed === target
}

/**
 * 保存前的快捷键冲突判定：找出 `fields`（形如 `[{ field, combo }, ...]`）里
 * **规范化后相等**的第一对，返回 `{ a, b }`（a 是先出现的字段，b 是撞上它的字段）；
 * 无冲突返回 null。空串/不可用组合不参与比较（空串 = 该项禁用，两个禁用不算冲突）。
 *
 * 只做纯判定：翻译提示、中止保存由调用方（设置页）负责 —— 这样这段「哪些值不能共存」
 * 的规则可以用真实输入直接测，而不是靠读源码文本。
 */
export function findHotkeyConflict(fields) {
  const seen = new Map() // 规范化组合 -> 先出现它的字段
  for (const { field, combo } of fields) {
    // 只比较**可用**的全局热键：空串（禁用）、缺修饰键、未知键名都跳过。
    // 不可用值永远不会生效，两个不可用值不能算「冲突」，否则会误报。
    if (!isValidCombo(combo)) continue
    const norm = normalizeCombo(combo)
    const other = seen.get(norm)
    if (other !== undefined) return { a: other, b: field }
    seen.set(norm, field)
  }
  return null
}

/**
 * 「本次运行实际生效的关闭窗口快捷键」的冻结规则（纯函数）。
 *
 * 关闭快捷键与三个全局热键一样，改配置不重启不换绑；App 的 keydown 处理器读的就是
 * 这个冻结值。`prev === null` 是「本进程还没加载过配置」的哨兵，此时采用本次加载值；
 * 一旦冻结（`prev` 是字符串）就不再被后续 `refreshConfig()` 覆盖。
 *
 * 用 `null` 而不是空串做哨兵：空串是「禁用」的**有效值**，必须原样保留，
 * 绝不能回退成默认 Ctrl+W。
 */
export function resolveRuntimeCloseHotkey(prev, loaded) {
  return prev !== null ? prev : loaded
}
