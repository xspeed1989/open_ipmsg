// node --test scripts/ —— 快捷键串纯函数单测
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  normalizeCombo, isValidCombo, comboFromEvent, matchesLocalCombo,
  findHotkeyConflict, resolveRuntimeCloseHotkey,
} from '../src/lib/hotkey.js'

test('归一化：大小写/别名/修饰键顺序', () => {
  assert.equal(normalizeCombo('alt+a'), 'Alt+A')
  assert.equal(normalizeCombo('shift+ctrl+s'), 'Ctrl+Shift+S')
  assert.equal(normalizeCombo('cmd+shift+a'), 'CmdOrCtrl+Shift+A')
  assert.equal(normalizeCombo('super+A'), 'CmdOrCtrl+A')
  assert.equal(normalizeCombo('  Alt + A '), 'Alt+A')
  assert.equal(normalizeCombo('enter'), 'Enter')
  assert.equal(normalizeCombo(''), null)
  assert.equal(normalizeCombo('a+b'), null) // 两个非修饰键非法
  assert.equal(normalizeCombo(null), null)
})

test('必须是「修饰键 + 主键」，纯修饰键或单键不算有效全局热键', () => {
  assert.equal(isValidCombo('Alt+A'), true)
  assert.equal(isValidCombo('A'), false)
  assert.equal(isValidCombo('Shift'), false)
  assert.equal(isValidCombo('Ctrl+Shift'), false)
  // 设置页用它标出「配置里存着一个不可用的快捷键」（手改配置 / 跨平台拷配置）
  assert.equal(isValidCombo(''), false)
  assert.equal(isValidCombo(undefined), false)
})

test('从键盘事件录制组合键', () => {
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: true, shiftKey: false, metaKey: false }), 'Alt+A')
  assert.equal(comboFromEvent({ key: 'S', ctrlKey: true, altKey: false, shiftKey: true, metaKey: false }), 'Ctrl+Shift+S')
  assert.equal(comboFromEvent({ key: 'Meta', ctrlKey: false, altKey: false, shiftKey: false, metaKey: true }), null)
  assert.equal(comboFromEvent({ key: 'a', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false }), null)
})

test('空格键能录成 Space，无法表达的键被拒绝', () => {
  assert.equal(comboFromEvent({ key: ' ', code: 'Space', ctrlKey: true }), 'Ctrl+Space')
  assert.equal(normalizeCombo('ctrl+space'), 'Ctrl+Space')
  // '+' 在 '+' 分隔的规范形里无法表达，直接拒绝而不是产出坏串
  assert.equal(comboFromEvent({ key: '+', ctrlKey: true }), null)
})

test('macOS 的 Option 合成字符不会产出不可解析的组合键', () => {
  // 按住 Option 再按 A：e.key 是 'å'，e.code 仍是 'KeyA'
  assert.equal(comboFromEvent({ key: 'å', code: 'KeyA', altKey: true }), 'Alt+A')
  // 拿不到 code 时拒绝合成字符，不猜
  assert.equal(comboFromEvent({ key: 'å', altKey: true }), null)
})

test('未知键名与越界 F 键不算合法热键', () => {
  assert.equal(isValidCombo('Ctrl+Foobar'), false)
  assert.equal(isValidCombo('Ctrl+F0'), false)
  assert.equal(isValidCombo('Ctrl+F99'), false)
  assert.equal(isValidCombo('Ctrl+F24'), true)
  assert.equal(normalizeCombo('Alt+Å'), null)
})

test('规范名可往返：normalizeCombo 幂等', () => {
  assert.equal(normalizeCombo('Alt+Up'), 'Alt+ArrowUp')
  assert.equal(normalizeCombo('Alt+ArrowUp'), 'Alt+ArrowUp')
  assert.equal(isValidCombo('Alt+ArrowUp'), true)
  assert.equal(comboFromEvent({ key: 'ArrowUp', code: 'ArrowUp', altKey: true }), 'Alt+ArrowUp')
})

import { isWaylandUA } from '../src/lib/hotkey.js'

test('应用内快捷键命中：规范化后相等才算命中', () => {
  const ev = (o = {}) => ({
    key: 'w', ctrlKey: true, altKey: false, shiftKey: false, metaKey: false, ...o,
  })
  // 命中：配置形与本次按键规范化后相等（大小写/别名无关）
  assert.equal(matchesLocalCombo('Ctrl+W', ev()), true)
  assert.equal(matchesLocalCombo('ctrl+w', ev()), true)
  // 不命中：多了/少了修饰键
  assert.equal(matchesLocalCombo('Ctrl+Shift+W', ev()), false)
  assert.equal(matchesLocalCombo('Ctrl+Alt+W', ev()), false)
  assert.equal(matchesLocalCombo('Alt+A', ev()), false)
  // 空串/不可用组合 = 禁用，永不命中
  assert.equal(matchesLocalCombo('', ev()), false)
  assert.equal(matchesLocalCombo(undefined, ev()), false)
  // 无修饰键的按键不构成组合键
  assert.equal(matchesLocalCombo('Ctrl+W', ev({ ctrlKey: false })), false)
})

test('Wayland 会话粗判只看显式 Wayland 标记', () => {
  assert.equal(isWaylandUA('Mozilla/5.0 (X11; Linux x86_64)'), false)
  assert.equal(isWaylandUA('Mozilla/5.0 (Wayland; Linux x86_64)'), true)
  assert.equal(isWaylandUA(''), false)
})

// findHotkeyConflict 是设置页「保存前冲突检测」的规则本体：用真实输入驱动，
// 不靠读源码文本断言。
test('冲突判定：三个默认值（close=Ctrl+W / open=Ctrl+Alt+Z / shot=Alt+A）互不冲突', () => {
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: 'Ctrl+W' },
      { field: 'open_hotkey', combo: 'Ctrl+Alt+Z' },
      { field: 'shot_hotkey', combo: 'Alt+A' },
    ]),
    null
  )
})

test('冲突判定：close 与 open 相同 → 返回 {a:close_hotkey, b:open_hotkey}', () => {
  assert.deepEqual(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: 'Ctrl+Alt+Z' },
      { field: 'open_hotkey', combo: 'Ctrl+Alt+Z' },
      { field: 'shot_hotkey', combo: 'Alt+A' },
    ]),
    { a: 'close_hotkey', b: 'open_hotkey' }
  )
  // 规范化后相等也算冲突（大小写/别名无关）
  assert.deepEqual(
    findHotkeyConflict([
      { field: 'shot_hotkey', combo: 'Alt+A' },
      { field: 'close_hotkey', combo: 'alt+a' },
    ]),
    { a: 'shot_hotkey', b: 'close_hotkey' }
  )
})

test('冲突判定：空串/不可用值不参与比较、不误报', () => {
  // 两个都清空（禁用）不算冲突
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: '' },
      { field: 'open_hotkey', combo: '' },
      { field: 'shot_hotkey', combo: '' },
    ]),
    null
  )
  // 一个空串 + 两个不同值：空串不占位、也不误报
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: '' },
      { field: 'open_hotkey', combo: 'Ctrl+Alt+Z' },
      { field: 'shot_hotkey', combo: '' },
    ]),
    null
  )
  // 手改配置存下的非法值（缺修饰键 / 未知键名）不参与比较，不能被判成冲突
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: 'W' },
      { field: 'open_hotkey', combo: 'W' },
    ]),
    null
  )
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: 'Ctrl+Foobar' },
      { field: 'open_hotkey', combo: 'Ctrl+Foobar' },
    ]),
    null
  )
})

// 本轮核心回归点：改成统一「重启后生效」后，把「旧运行中的全局组合」交给关闭动作，
// 只要三个新配置值互不冲突，就不再需要额外拦截（那层 activeGlobalHotkeys 快照已删）。
test('冲突判定：旧全局组合交给 close、三个新配置值不冲突 → 无冲突', () => {
  // 旧运行时 open=Ctrl+Alt+Z，用户把 open 改成 Ctrl+Alt+X、close 设成 Ctrl+Alt+Z：
  // 三个新值互不相同，findHotkeyConflict 只看新值，返回 null
  assert.equal(
    findHotkeyConflict([
      { field: 'close_hotkey', combo: 'Ctrl+Alt+Z' },
      { field: 'open_hotkey', combo: 'Ctrl+Alt+X' },
      { field: 'shot_hotkey', combo: 'Alt+A' },
    ]),
    null
  )
})

test('关闭快捷键冻结语义：null 是「未加载」哨兵，加载后不再被覆盖', () => {
  // 模块初始值 null（尚未加载）→ 采用本次加载值
  assert.equal(resolveRuntimeCloseHotkey(null, 'Ctrl+W'), 'Ctrl+W')
  // 已冻结值 + 新加载值（保存设置后再 refreshConfig）→ 保持冻结值，不换绑
  assert.equal(resolveRuntimeCloseHotkey('Ctrl+W', 'Ctrl+Alt+X'), 'Ctrl+W')
  // 冻结值为空串（= 禁用）→ 保持空串，绝不能回退成默认 Ctrl+W
  assert.equal(resolveRuntimeCloseHotkey('', 'Ctrl+W'), '')
  // 用空串作为加载值（用户清空禁用）时同样原样采用
  assert.equal(resolveRuntimeCloseHotkey(null, ''), '')
})

// App.vue 里的实际挂载方式（读源码断言）：模块纯函数测得再对，挂错阶段照样会被
// 应用内已有快捷键抢先吞掉 —— 这就是「把关闭键设成 Ctrl+F 按下去只开搜索」的成因。
const appSource = readFileSync(new URL('../src/App.vue', import.meta.url), 'utf8')
const closeHandler = appSource.match(/function onCloseHotkey\(e\)[\s\S]*?\n\}/)?.[0] || ''

test('关闭窗口快捷键挂在 window 捕获阶段：不被应用内已有快捷键抢先吞掉', () => {
  assert.ok(closeHandler, 'App.vue 里必须有 onCloseHotkey 处理器')
  // capture=true：ChatWindow 的 Ctrl+F 搜索等冒泡监听挂载更早，冒泡阶段会被它们先
  // preventDefault，用户配置的关闭键就永远按不动（设置页也无任何提示）
  assert.match(
    appSource,
    /window\.addEventListener\('keydown',\s*onCloseHotkey,\s*true\)/,
    '关闭快捷键必须用捕获阶段监听'
  )
  // 卸载必须用同一 capture 值（布尔值参与监听器匹配），否则监听泄漏、随重挂载叠加
  assert.match(
    appSource,
    /window\.removeEventListener\('keydown',\s*onCloseHotkey,\s*true\)/,
    '卸载时要按同一 capture 值摘掉监听'
  )
  // 命中后必须停掉后续分发：同一次按键不能再被冒泡监听当成搜索等命令
  assert.match(closeHandler, /e\.preventDefault\(\)/)
  assert.match(closeHandler, /e\.stopPropagation\(\)/)
})

test('关闭窗口快捷键跳过录制框，且保留防护与空串禁用', () => {
  // 捕获阶段早于录制框自身的 target 阶段监听：只能靠显式数据标记跳过，否则在设置页
  // 录 Ctrl+W 会直接把主窗口关掉（隐藏到托盘）。
  assert.match(
    closeHandler,
    /e\.target\?\.dataset\?\.hotkeyRecorder !== undefined/,
    '录制框（data-hotkey-recorder）必须在捕获阶段直接 return'
  )
  assert.match(closeHandler, /e\.defaultPrevented/)
  assert.match(closeHandler, /e\.repeat/)
  assert.match(closeHandler, /e\.isComposing/)
  assert.match(closeHandler, /if \(!combo\) return/, '空串（= 禁用）必须不关窗')
})

test('关闭窗口快捷键读本次运行冻结值，不实时读配置', () => {
  // 改配置不重启不换绑：App 必须读 store.runtimeCloseHotkey，而不是实时读
  // store.config.close_hotkey（后者会让「重启后生效」的说明与行为不一致，
  // 也是上一轮「冲突检测」要拦截的根源）。
  assert.match(closeHandler, /store\.runtimeCloseHotkey/, '必须读本次运行冻结的关闭快捷键')
  assert.doesNotMatch(
    closeHandler,
    /store\.config\??\.close_hotkey/,
    '不能实时读配置（否则改配置不重启就换绑了）'
  )
})
