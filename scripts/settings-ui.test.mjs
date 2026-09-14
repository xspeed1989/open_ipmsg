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
