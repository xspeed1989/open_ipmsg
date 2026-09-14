# 应用内对话框（替换原生弹窗）+ 后端错误本地化

日期：2026-09-14
状态：已与用户确认设计，待实现

## 背景

界面里散落 40 处原生弹窗，会弹出系统级对话框，与自绘界面割裂：

| 类型 | 数量 | 位置 |
| --- | --- | --- |
| `alert()` | 36 | ChatWindow 31 / SettingsModal 4 / ImageViewer 1 |
| 原生 `confirm`（`@tauri-apps/plugin-dialog`） | 2 | ListPanel 删除会话、ChatWindow |
| `window.confirm` | 1 | EmojiPicker 删除表情 |
| `window.prompt` | 1 | ChatWindow 密码锁输入 |

另外**后端错误串是中文硬编码**：`invoke` 失败时前端把 `e` 直接塞进提示
（`alert(e)` / `alert(t('chat.alertClear', { e }))`），英文界面于是显示中文。

**保持原生**（用户明确排除）：`open()` 文件/文件夹选择、`save()` 保存文件对话框、
系统托盘、系统通知。

## 设计

### 1. `src/lib/dialog.js` —— 对话框服务

响应式状态 + 命令式 API，任何模块都能调用（不依赖组件实例）：

```js
toast(text, { kind = 'info', duration = 3200 })  // 右下角堆叠，自动消失，可点掉
alert(text)        // 模态信息框，返回 Promise（多行摘要这类需要阅读时间的场景）
confirm(text, opts)// 模态确认，Promise<boolean>；opts: { okLabel, cancelLabel, danger }
prompt(text, opts) // 模态输入，Promise<string|null>；opts: { password, placeholder, value }
```

- 同时最多 4 条 toast，超出丢最旧；`MAX_TOASTS`、`TOAST_MS` 具名导出便于测试。
- 模态同一时刻只有一个：新请求先解决旧的（confirm → false，prompt → null），
  避免 Promise 悬挂。
- 纯逻辑，可在 node 下测试（vue 的 `reactive` 不依赖 DOM）。

### 2. `src/components/DialogHost.vue` —— 唯一渲染宿主

- 挂在 `App.vue`（主窗口）与 `main.js` 的 ImageViewer 分支（独立窗口，各有 1 个调用点）；
  截图遮罩窗口没有调用点，不挂。
- 视觉沿用现有弹窗语言（`.overlay` + 卡片 + `btn-primary`/`btn-plain` + 既有 CSS 变量）。
- 无障碍：模态 `role="dialog"` + `aria-modal`、打开时焦点移入、Esc 关闭、确认/取消可 Tab 到。
- toast 容器 `aria-live="polite"`。

### 3. `src/lib/errors.js` —— 后端错误本地化

后端错误加**稳定错误码前缀**：`E_XXX|<原文>`。`|` 之后一律视为「细节/原文」，
日志与既有测试断言（`err.contains("密码错误")`）不受影响。

```js
describeError(e, fallbackKey = 'err.fallback') → string
```

- 有码且 i18n 表里有该码 → 用译文；模板可含 `{e}` 占位符接收细节。
- 无码 → 本地化兜底句；**非中文界面下不再把中文原文透出去**（原文进 `console.warn`）。
- 中文界面保留原文，便于排查。

错误码清单（首批，覆盖所有能追到的用户可见路径）：unlock 3 个、recall 3 个、
send 空消息、昵称必填、图片类型/过大/读取失败、文件不存在、保存失败、
剪贴板读取失败/超时、图片窗口打开失败、导入失败、聊天记录落盘失败。

### 4. 调用点替换

- 36 处 `alert` → `toast`（错误用 `kind: 'error'`，结果用 `kind: 'info'`）；
  导入历史记录的多行汇总保留模态 `alert`（需要阅读时间）。
- 2 处原生 `confirm` + 1 处 `window.confirm` → `await dialog.confirm(...)`，
  按钮文案沿用既有 i18n key（`cancel` / `list.deleteOk` 等）。
- 1 处 `window.prompt` → `dialog.prompt(..., { password: true })`（顺带修掉明文输入）。
- 文件/保存对话框保持不变。

### 5. 测试

- `scripts/dialog.test.mjs`：toast 堆叠上限与自动消失、confirm 的 true/false、
  prompt 返回值与取消、模态互斥时旧 Promise 的落定。
- `scripts/errors.test.mjs`：有码→译文、无码→兜底、英文下不含 CJK、
  错误码表两种语言齐全（复用 i18n 的字典解析）。
- `scripts/settings-ui.test.mjs` 同级新增：**静态禁用测试** —— `src/` 下不得再出现
  `window.alert/confirm/prompt`（`src/lib/dialog.js` 白名单除外），防回潮。
- Rust 侧：带码错误串仍保留中文原文，既有断言不变；新增一条断言错误码前缀存在。

## 不做

- 不改系统托盘、系统通知（本就是系统级能力）。
- 不改文件/保存对话框。
- 不做 toast 的「撤销」动作、不做富文本弹窗（YAGNI）。
