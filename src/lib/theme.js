/**
 * 主题应用：把配置里的 theme 写到 <html data-theme>，CSS 变量随之切换。
 *
 * - 'light' / 'dark'：显式指定，写 data-theme
 * - 其它（'system' 或缺省）：移除属性，交给 CSS 的
 *   `@media (prefers-color-scheme: dark)` 跟随系统，无需 JS 监听
 *
 * @param {string} theme
 * @param {HTMLElement} [root] 便于测试时传入替身
 * @returns {string} 实际生效的模式（'light' | 'dark' | 'system'）
 */
export function applyTheme(theme, root) {
  const el = root || (typeof document !== 'undefined' ? document.documentElement : null)
  const mode = theme === 'light' || theme === 'dark' ? theme : 'system'
  if (!el) return mode
  if (mode === 'system') {
    el.removeAttribute('data-theme')
  } else {
    el.setAttribute('data-theme', mode)
  }
  return mode
}
