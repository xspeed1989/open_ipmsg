/**
 * 真 store 驱动的「本次运行关闭快捷键冻结」行为测试。
 *
 * 与 hotkey.test.mjs 里的纯函数单测互补：这里跨的是真 seam（真 store.js + mockIPC），
 * 验证 refreshConfig() 首次成功加载配置后把值冻结、之后保存设置再刷新也不覆盖。
 * 硬前提：store.js 的相对 import 带显式 .js（见 helpers/store-harness.mjs 文件头）。
 */
import { test, before } from 'node:test'
import assert from 'node:assert/strict'
import { startStore } from './helpers/store-harness.mjs'

let h
before(async () => {
  h = await startStore({
    responses: {
      get_config: () => ({
        nickname: '我', group: '', hostname: 'me-host', lang: 'zh', theme: 'auto',
        encoding: 'utf8', download_dir: '', encrypt: false, password_use: false,
        absence_enabled: false, absence_text: '', shot_copy_clipboard: true,
        auto_open_secret: true, close_hotkey: 'Ctrl+W',
      }),
    },
  })
})

test('首次成功加载配置后采用加载值，之后 refreshConfig 不覆盖（改配置不重启不换绑）', async () => {
  assert.equal(h.store.runtimeCloseHotkey, 'Ctrl+W', '首次加载配置后应冻结为加载值')

  // 用户在设置页改了 close_hotkey 并保存：SettingsModal 保存后会再 refreshConfig 一次
  const base = h.backend.get_config
  h.backend.get_config = () => ({ ...base(), close_hotkey: 'Ctrl+Alt+X' })
  await h.mod.refreshConfig()

  assert.equal(h.store.config.close_hotkey, 'Ctrl+Alt+X', '配置本身应更新为新值')
  assert.equal(
    h.store.runtimeCloseHotkey,
    'Ctrl+W',
    '冻结值不能被后续 refreshConfig 覆盖（本次运行仍按旧组合匹配）'
  )
})

test('把配置清空（禁用）也不改变本次运行的冻结值', async () => {
  const base = h.backend.get_config
  h.backend.get_config = () => ({ ...base(), close_hotkey: '' })
  await h.mod.refreshConfig()
  assert.equal(h.store.config.close_hotkey, '', '配置应更新为空串（禁用）')
  assert.equal(
    h.store.runtimeCloseHotkey,
    'Ctrl+W',
    '空串是禁用，但只在下次启动生效；本次运行的冻结值保持不变'
  )
})
