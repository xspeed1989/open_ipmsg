/**
 * 快捷键串纯函数：规范化 + 设置页录制。
 *
 * 规范形：修饰键（CmdOrCtrl/Ctrl/Alt/Shift）+ 主键，用 '+' 连接，如 "CmdOrCtrl+Shift+A"。
 * 这个字符串直接存进配置，启动时交给 Tauri 的 global-shortcut 插件
 * （Windows/macOS/X11）或由 Rust 转成 portal 触发器（Wayland，见 shortcut.rs）；
 * 两侧都不需要 JS 再转换，所以这里只负责「录入即规范形」。
 */

// 修饰键固定顺序：CmdOrCtrl（平台主修饰键）在前，其余按 Ctrl/Alt/Shift。
// 下游只按名字解析（Tauri 的 Shortcut::from_str、Rust 侧 portal 触发器），
// 顺序只影响可读性与幂等比较，与 task-11 的 "CmdOrCtrl+Alt+A" 示例保持一致。
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
    else if (/^f\d{1,2}$/i.test(p)) key = p.toUpperCase()
    else if (p.length === 1) key = p.toUpperCase()
    else key = p[0].toUpperCase() + p.slice(1)
  }
  if (!key) return null
  return [...MOD_ORDER.filter((m) => mods.has(m)), key].join('+')
}

/** 有效全局热键 = 至少一个修饰键 + 主键（设置页用它标出配置里存着的非法值） */
export function isValidCombo(combo) {
  const n = normalizeCombo(combo)
  return !!n && n.split('+').length >= 2
}

/** 设置页录制：从键盘事件得到规范形；纯修饰键或无修饰键返回 null */
export function comboFromEvent(e) {
  const key = e?.key
  if (!key) return null
  if (['Control', 'Alt', 'Shift', 'Meta', 'CapsLock', 'Dead', 'Unidentified'].includes(key)) return null
  const mods = []
  if (e.ctrlKey) mods.push('Ctrl')
  if (e.altKey) mods.push('Alt')
  if (e.shiftKey) mods.push('Shift')
  if (e.metaKey) mods.push('CmdOrCtrl')
  if (!mods.length) return null
  const combo = [...mods, key].join('+')
  return isValidCombo(combo) ? normalizeCombo(combo) : null
}
