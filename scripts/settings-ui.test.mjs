// node --test scripts/ —— 设置页样式契约（不依赖 Tauri / 浏览器运行时）
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { t, setLocale } from '../src/lib/i18n.js'

const source = readFileSync(new URL('../src/components/SettingsModal.vue', import.meta.url), 'utf8')
const css = source.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''

function declarations(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const matches = css.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}\\s*\\{([^}]*)\\}`, 'g'))
  return Array.from(matches, (match) => match[1]).join('\n')
}

test('协议扩展设置按纵向表单布局，不退化为 si-row 的横向挤压', () => {
  assert.match(declarations('.adv-col'), /flex-direction\s*:\s*column\b/)
  assert.match(declarations('.adv-line'), /display\s*:\s*flex\b/)
  assert.match(declarations('.adv-line'), /align-items\s*:\s*center\b/)
  assert.match(declarations('.adv-input'), /width\s*:\s*100%/)
})

test('截图设置分区包含热键录制与自动复制开关', () => {
  assert.match(source, /shot_hotkey/, '设置页必须能改截图热键')
  assert.match(source, /shot_copy_clipboard/, '设置页必须有「确认后复制到剪贴板」开关')
})

test('截图热键输入框是只读录制控件，不做自由文本输入', () => {
  const css = source.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''
  assert.match(css, /\.hotkey-input/, '热键录制控件需要独立样式（避免与普通输入框混淆）')
  assert.match(source, /readonly/, '热键框必须 readonly，值只能由按键录制写入')
})

test('加密行的标签与开关同一行，且各表单行控件左边缘对齐', () => {
  // 加密行是「标签 + 三行内容」的复合行：沿用 .field 默认的 align-items:center
  // 会把标签压到内容块的垂直中点（正好是说明那一行），看着像标签配错了行 ——
  // 中文同样错位，只是标签都是 4 字、不那么刺眼；英文长标签还会同时暴露下一个问题。
  assert.match(source, /class="field enc-field"/, '加密行需要独立类名，才能与其它单行表单项区分')
  assert.match(
    declarations('.field.enc-field'),
    /align-items\s*:\s*flex-start\b/,
    '加密行的标签必须与内容列顶边对齐（否则落到说明那一行）'
  )
  assert.match(
    declarations('.field.enc-field .lab'),
    /line-height\s*:\s*21px\b/,
    '标签行高需等于开关高度，才能与开关落在同一视觉行'
  )
  // 标签列定宽：宽度跟着文字长度走时，英文长标签会把该行控件整体推右
  const minWidth = declarations('.field .lab').match(/min-width\s*:\s*(\d+)px/)
  assert.ok(minWidth, '表单行标签需要显式 min-width，否则列宽随文案长度浮动')
  assert.ok(
    Number(minWidth[1]) >= 88,
    `标签列至少 88px 才能容下最长的英文标签，实际 ${minWidth[1]}px`
  )
  setLocale('en')
  const enLabel = t('settings.encrypt')
  assert.ok(
    enLabel.length <= 12,
    `英文标签「${enLabel}」过长，会把加密行控件推出对齐列（88px 标签列约容得下 12 字符）`
  )
  setLocale('zh-CN')
})

test('协议扩展区有「自动打开封书」开关：默认选中、可回填、可保存', () => {
  // 默认选中：表单初值必须是 true（与后端 Config::default 的 serde default 一致）
  assert.match(
    source,
    /auto_open_secret:\s*true/,
    '「自动打开封书」默认必须选中'
  )
  // 回填：配置里没有这个字段（旧版本写下的 config.json）时也按「开」处理
  assert.match(
    source,
    /form\.auto_open_secret\s*=\s*store\.config\.auto_open_secret\s*!==\s*false/,
    '设置页回填漏了该字段：打开一次再保存就会把用户的开关写回默认值'
  )
  // 保存：必须进 save_config 补丁（后端 apply_config_patch 按 Option 合并）
  assert.match(
    source,
    /auto_open_secret:\s*!!form\.auto_open_secret/,
    '保存补丁漏了该字段：改了开关不生效'
  )
  // 开关控件：与其它协议开关同款的可访问 switch
  assert.match(source, /:class="\{ on: form\.auto_open_secret \}"/, '缺少绑定该字段的开关')
  assert.match(
    source,
    /@click="form\.auto_open_secret = !form\.auto_open_secret"/,
    '开关必须能切换该字段'
  )
  assert.match(
    source,
    /t\('settings\.autoOpenSecretHint'\)/,
    '开关需要一行说明文案（否则用户不知道它管的是「收到的封书」）'
  )
})

test('常规区有「开机启动」开关：默认不勾、可回填、保存时先改系统再落盘', () => {
  // 默认不勾：往系统里写自启条目必须是用户的显式选择
  assert.match(source, /autostart:\s*false/, '「开机启动」默认必须不勾选')
  // 回填：config.autostart 是唯一数据源（读不到就按关闭处理）
  assert.match(
    source,
    /form\.autostart\s*=\s*!!store\.config\.autostart/,
    '设置页回填漏了该字段：打开一次再保存就会把用户的开关写回默认值'
  )
  // 真正的系统登记走 set_autostart，而不是只改 config.json
  assert.match(
    source,
    /await ipc\.setAutostart\(/,
    '保存时必须调用 ipc.setAutostart 去写系统自启条目'
  )
  // 顺序：系统写入失败就整体不落盘 —— 否则会留下「界面显示已开、系统里没有」的假象
  const saveFn = source.match(/async function save\(\)[\s\S]*?\n\}/)?.[0] || ''
  const callAt = saveFn.indexOf('ipc.setAutostart')
  const saveCfgAt = saveFn.indexOf('await ipc.saveConfig(')
  const patchAt = saveFn.indexOf('const patch = {')
  assert.ok(callAt > -1 && patchAt > -1 && callAt < patchAt, 'setAutostart 必须在 saveConfig 之前调用')
  // 配置落盘失败要回滚系统条目：否则系统里留着「开机自己起来」而设置里显示关着
  assert.ok(
    saveFn.indexOf('ipc.setAutostart(prevAutostart)') > saveCfgAt,
    'saveConfig 失败时必须回滚系统自启条目'
  )
  // 失败要提示用户，且用带错误码的译文（E_AUTOSTART_FAILED）
  assert.match(
    source,
    /describeError\(e, 'settings\.alertAutostartFailed'\)/,
    '开机启动写入失败必须给出可读提示'
  )
  // 开关控件与说明
  assert.match(source, /:class="\{ on: form\.autostart \}"/, '缺少绑定该字段的开关')
  assert.match(
    source,
    /@click="form\.autostart = !form\.autostart"/,
    '开关必须能切换该字段'
  )
  assert.match(source, /t\('settings\.autostartHint'\)/, '开关需要说明文案（生效时机/写到哪里）')
  // 保存补丁里也要记录这次选择（后端 apply_config_patch 按 Option 合并）
  assert.match(source, /autostart:\s*!!form\.autostart/, '保存补丁漏了该字段')
})

test('窗口快捷键：两个只读录制框 + ?? 回填 + 保存前冲突检测', () => {
  // 两个录制框都在，且是独立的 readonly 控件（不做自由文本输入）
  assert.match(source, /id="close-hotkey"/, '缺少关闭窗口快捷键录制框')
  assert.match(source, /id="open-hotkey"/, '缺少打开窗口快捷键录制框')
  assert.match(source, /close_hotkey:\s*''/, '表单初值漏了 close_hotkey')
  assert.match(source, /open_hotkey:\s*''/, '表单初值漏了 open_hotkey')
  // 回填用 ?? 而不是 ||：空串是「禁用」的有效值，不能被默认值顶回
  assert.match(
    source,
    /form\.close_hotkey\s*=\s*store\.config\.close_hotkey\s*\?\?/,
    '关闭快捷键回填必须用 ??（空串=禁用，不能被默认值覆盖）'
  )
  assert.match(
    source,
    /form\.open_hotkey\s*=\s*store\.config\.open_hotkey\s*\?\?/,
    '打开快捷键回填必须用 ??（空串=禁用，不能被默认值覆盖）'
  )
  // 保存补丁必须带上这两个字段（后端 apply_config_patch 按 Option 合并）
  assert.match(source, /close_hotkey:\s*form\.close_hotkey/, '保存补丁漏了 close_hotkey')
  assert.match(source, /open_hotkey:\s*form\.open_hotkey/, '保存补丁漏了 open_hotkey')
  // 录制必须 stopPropagation：否则录 Ctrl+W 会被别处的 window 监听当成真的命令
  assert.match(source, /stopPropagation\(\)/, '录制框必须 stopPropagation')
  // 保存前两两冲突检测：规则抽到 lib/hotkey.js 的纯函数里（真实输入的行为测试见
  // hotkey.test.mjs），这里只验证设置页确实复用它并报明确译文
  assert.match(source, /findHotkeyConflict\(/, '冲突检测必须复用 findHotkeyConflict 纯函数')
  assert.match(source, /t\('settings\.hotkeyConflict'/, '冲突时必须报明确错误译文')
})

test('三个录制框都带 data-hotkey-recorder 标记（App 捕获阶段靠它跳过）', () => {
  // App.vue 的关闭快捷键挂在 window 捕获阶段，早于录制框自己的 target 阶段监听；
  // 没有这个显式标记，在设置页录 Ctrl+W 就会把主窗口关掉。
  const marks = source.match(/^\s*data-hotkey-recorder\s*$/gm) || []
  assert.equal(marks.length, 3, '截图/关闭/打开三个录制框都必须带 data-hotkey-recorder')
  for (const id of ['shot-hotkey', 'close-hotkey', 'open-hotkey']) {
    assert.match(
      source,
      new RegExp(`id="${id}"[\\s\\S]{0,200}data-hotkey-recorder`),
      `${id} 录制框缺 data-hotkey-recorder（录 Ctrl+W 会关窗）`
    )
  }
})

test('保存前冲突检测复用纯函数，且在写配置之前拦下', () => {
  const saveFn = source.match(/async function save\(\)[\s\S]*?\n\}/)?.[0] || ''
  assert.ok(saveFn, '应当能从源码里抽出 save()')
  // 规则本体在 lib/hotkey.js 的 findHotkeyConflict（真实输入的行为测试见 hotkey.test.mjs）；
  // SettingsModal 只负责复用 + 翻译提示 + 中止保存
  assert.match(saveFn, /findHotkeyConflict\(/, '保存前必须调用 findHotkeyConflict')
  assert.match(saveFn, /t\('settings\.hotkeyConflict'/, '冲突时要报明确冲突译文')
  // 必须在写配置之前拦下：放到 saveConfig 之后就已经落盘了
  assert.ok(
    saveFn.indexOf('findHotkeyConflict') < saveFn.indexOf('await ipc.saveConfig('),
    '冲突检测必须在 saveConfig 之前'
  )
  // 上一轮为「重启后才换绑」写的 activeGlobalHotkeys 配置快照已删：它记的是配置值
  // 而不是注册结果，会误报；现在统一「重启后生效」，不再需要这类拦截。
  assert.doesNotMatch(source, /activeGlobalHotkeys/, '不应再依赖 activeGlobalHotkeys 快照')
  assert.doesNotMatch(source, /hotkeyConflictActive/, '不再需要「旧全局绑定仍生效」的专属冲突译文')
})
