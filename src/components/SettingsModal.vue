<script setup>
// 设置弹窗：昵称 / 群组 / 下载目录 / 语言 / 主题 / 发送编码 / 消息加密 + 本机信息
import { reactive, watch, computed, ref } from 'vue'
import { applyTheme, store, refreshConfig, refreshUsers, loadSessions } from '../store'
import * as ipc from '../lib/ipc'
import { t, SUPPORTED_LANGS, LANG_NAMES, setLocale, detectLocale } from '../lib/i18n'
import { comboFromEvent, isValidCombo, isWaylandUA } from '../lib/hotkey'
import { open as pickDialog } from '@tauri-apps/plugin-dialog'

const form = reactive({
  nickname: '',
  group: '',
  download_dir: '',
  encoding: 'utf8',
  theme: 'system',
  lang: 'zh-CN',
  // 加密默认开启：config 缺失该字段（旧版本后端）时也按开启处理
  encrypt: true,
  absence_enabled: false,
  absence_text: '',
  password_use: false,
  password: '',
  agent_addr: '',
  master_addr: '',
  allow_send_list: true,
  ipdict_enabled: true,
  dir_mode: 'off',
  v6_mcast: true,
  // 截图热键（规范形，空串 = 不注册全局热键）与「确认后复制到剪贴板」
  shot_hotkey: '',
  shot_copy_clipboard: true,
})

watch(
  () => store.settingsOpen,
  (open) => {
    if (open && store.config) {
      form.nickname = store.config.nickname || ''
      form.group = store.config.group || ''
      form.download_dir = store.config.download_dir || ''
      form.encoding = store.config.encoding || 'utf8'
      form.theme = store.config.theme || 'system'
      form.lang = store.config.lang || detectLocale()
      form.encrypt = store.config.encrypt !== false
      form.absence_enabled = !!store.config.absence_enabled
      form.absence_text = store.config.absence_text || ''
      form.password_use = !!store.config.password_use
      form.password = store.config.password || ''
      form.agent_addr = store.config.agent_addr || ''
      form.master_addr = store.config.master_addr || ''
      form.allow_send_list = store.config.allow_send_list !== false
      form.ipdict_enabled = store.config.ipdict_enabled !== false
      form.dir_mode = store.config.dir_mode || 'off'
      form.v6_mcast = store.config.v6_mcast !== false
      form.shot_hotkey = store.config.shot_hotkey || ''
      form.shot_copy_clipboard = store.config.shot_copy_clipboard !== false
    }
  },
  { immediate: true }
)

const canClose = computed(() => !store.firstRun)

const recording = ref(false)

/** 录制：按下的组合键直接写进表单；Esc 清空（= 不注册全局热键） */
function onHotkeyKeydown(e) {
  // 裸 Tab 放行：否则 preventDefault 会把焦点困在录制框里（键盘用户出不去）；
  // 带修饰键的 Ctrl/Alt+Tab 仍按普通组合键录制
  if (e.key === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey) return
  e.preventDefault()
  if (e.key === 'Escape') {
    form.shot_hotkey = ''
    return
  }
  const combo = comboFromEvent(e)
  if (combo) form.shot_hotkey = combo
}

/** 配置里可能存着一个不可用的值（手改配置 / 跨平台拷贝）—— 明确告诉用户它不会生效 */
const hotkeyHint = computed(() => {
  if (form.shot_hotkey && !isValidCombo(form.shot_hotkey)) return t('settings.shotHotkeyInvalid')
  return isWaylandUA(navigator.userAgent)
    ? t('settings.shotHotkeyWayland')
    : t('settings.shotHotkeyHint')
})

// 点击指纹行 → 复制本机密钥指纹（复用 ipc.copyText 的原生剪贴板通道）
const fpCopied = ref(false)
let fpTimer = null
async function copyFp() {
  const fp = store.config?.key_fp
  if (!fp) return
  try {
    await ipc.copyText(fp)
    fpCopied.value = true
    clearTimeout(fpTimer)
    fpTimer = setTimeout(() => {
      fpCopied.value = false
    }, 1500)
  } catch {
    // 剪贴板不可用时静默：指纹文本本身仍完整可见、可手动选择复制
  }
}

// 选中即预览：语言/主题不必按保存就能看到效果；取消关闭时再还原成已保存的值
watch(() => form.theme, (tv) => {
  if (tv) applyTheme(tv)
})
watch(() => form.lang, (lv) => {
  if (lv) setLocale(lv)
})
function restoreSavedTheme() {
  applyTheme(store.config?.theme)
}
function restoreSavedLang() {
  setLocale(store.config?.lang || detectLocale())
}

async function chooseDir() {
  const dir = await pickDialog({ directory: true, title: t('settings.pickDirTitle') })
  if (dir) form.download_dir = dir
}

const importing = ref(false)
// 从官方 IP Messenger 的日志库（v4.5+ 的 ipmsg.db）导入历史聊天记录。
// 后端按 msg_id 去重，重复选择同一个文件执行也不会产生重复记录。
async function importIpmsg() {
  const picked = await pickDialog({
    multiple: true,
    title: t('settings.pickDbTitle'),
    filters: [{ name: t('settings.dbFilter'), extensions: ['db'] }],
  })
  const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter(Boolean)
  if (!paths.length || importing.value) return
  importing.value = true
  try {
    const r = await ipc.importIpmsgLogs(paths)
    await loadSessions()
    const lines = r.files.map((f) =>
      f.ok
        ? `${t('settings.importLineOk', { name: f.path.split(/[\\/]/).pop(), n: f.imported })}`
        : `✘ ${f.path}\n  ${f.error}`
    )
    let msg =
      r.failed > 0
        ? `${lines.join('\n')}`
        : t('settings.importDone', { total: r.total }) +
          (r.skipped ? t('settings.importSkipped', { n: r.skipped }) : '') +
          (r.sessionsNew ? t('settings.importNewSessions', { n: r.sessionsNew }) : '') +
          (r.mergedSessions ? t('settings.importMerged', { n: r.mergedSessions }) : '')
    if (!r.total && !r.failed) msg += t('settings.importNoNew')
    alert(msg)
  } catch (e) {
    alert(t('settings.alertImportFailed', { e }))
  } finally {
    importing.value = false
  }
}

function close() {
  if (!canClose.value) return
  restoreSavedTheme() // 放弃未保存的改动，主题跟着回退
  restoreSavedLang() // 语言同样回退到已保存的值
  store.settingsOpen = false
}

async function save() {
  if (!form.nickname.trim()) {
    alert(t('settings.alertNickname'))
    return
  }
  const patch = {
    nickname: form.nickname.trim(),
    group: form.group.trim(),
    download_dir: form.download_dir.trim(),
    encoding: form.encoding,
    theme: form.theme,
    lang: form.lang,
    encrypt: !!form.encrypt,
    absence_enabled: !!form.absence_enabled,
    absence_text: form.absence_text.trim(),
    password_use: !!form.password_use,
    password: form.password,
    agent_addr: form.agent_addr.trim(),
    master_addr: form.master_addr.trim(),
    allow_send_list: !!form.allow_send_list,
    ipdict_enabled: !!form.ipdict_enabled,
    dir_mode: form.dir_mode,
    v6_mcast: !!form.v6_mcast,
    shot_hotkey: form.shot_hotkey,
    shot_copy_clipboard: !!form.shot_copy_clipboard,
  }
  try {
    await ipc.saveConfig(patch)
    await refreshConfig()
    await refreshUsers()
    store.firstRun = false
    store.settingsOpen = false
  } catch (e) {
    alert(t('settings.alertSaveFailed', { e }))
  }
}
</script>

<template>
  <div class="overlay" @click.self="close">
    <div class="modal">
      <header>
        <span>{{ t('settings.title') }}</span>
        <button v-if="canClose" class="x" @click="close">✕</button>
      </header>

      <div class="body">
        <label class="field">
          <span class="lab">{{ t('settings.nickname') }}</span>
          <input v-model="form.nickname" :placeholder="t('settings.nicknamePh')" maxlength="32" spellcheck="false" />
        </label>
        <label class="field">
          <span class="lab">{{ t('settings.group') }}</span>
          <input v-model="form.group" :placeholder="t('settings.groupPh')" maxlength="32" spellcheck="false" />
        </label>
        <label class="field">
          <span class="lab">{{ t('settings.downloadDir') }}</span>
          <div class="dir-row">
            <input v-model="form.download_dir" readonly class="dir-input" :title="form.download_dir" />
            <button class="btn-plain" @click="chooseDir">{{ t('settings.choose') }}</button>
          </div>
        </label>
        <label class="field">
          <span class="lab">{{ t('settings.language') }}</span>
          <select v-model="form.lang">
            <option v-for="l in SUPPORTED_LANGS" :key="l" :value="l">{{ LANG_NAMES[l] }}</option>
          </select>
        </label>
        <label class="field">
          <span class="lab">{{ t('settings.theme') }}</span>
          <select v-model="form.theme">
            <option value="system">{{ t('settings.themeSystem') }}</option>
            <option value="light">{{ t('settings.themeLight') }}</option>
            <option value="dark">{{ t('settings.themeDark') }}</option>
          </select>
        </label>
        <label class="field">
          <span class="lab">{{ t('settings.encoding') }}</span>
          <select v-model="form.encoding">
            <option value="utf8">{{ t('settings.encodingUtf8') }}</option>
            <option value="gbk">{{ t('settings.encodingGbk') }}</option>
          </select>
        </label>

        <div class="field">
          <span class="lab">{{ t('settings.encrypt') }}</span>
          <div class="enc-col">
            <div class="enc-line">
              <button type="button" class="switch" :class="{ on: form.encrypt }" role="switch"
                :aria-checked="form.encrypt ? 'true' : 'false'" :title="t('settings.encrypt')"
                @click="form.encrypt = !form.encrypt">
                <i class="knob"></i>
              </button>
              <span class="enc-state">{{ form.encrypt ? t('settings.encryptOn') : t('settings.encryptOff') }}</span>
            </div>
            <div class="enc-hint">{{ t('settings.encryptHint') }}</div>
            <!-- 本机公钥指纹：与对端核对密钥用；点击整行复制 -->
            <div v-if="store.config?.key_fp" class="fp-row" :title="t('settings.fpTitle')" @click="copyFp">
              <span class="fp-lab">{{ t('settings.fpLabel') }}</span>
              <code class="fp-val">{{ store.config.key_fp }}</code>
              <span v-if="fpCopied" class="fp-copied">{{ t('settings.fpCopied') }}</span>
            </div>
          </div>
        </div>

        <div class="selfinfo">
          <div class="si-title">IPMsg 协议扩展</div>
          <div class="si-row adv-col">
            <label class="adv-line">
              <button type="button" class="switch" :class="{ on: form.absence_enabled }" role="switch"
                :aria-checked="form.absence_enabled ? 'true' : 'false'" @click="form.absence_enabled = !form.absence_enabled">
                <i class="knob"></i>
              </button>
              <span class="lab">{{ t('settings.absence') }}</span>
            </label>
            <input v-if="form.absence_enabled" v-model="form.absence_text" class="adv-input"
              :placeholder="t('settings.absenceText')" maxlength="120" spellcheck="false" />
            <div class="import-hint">{{ t('settings.absenceHint') }}</div>
          </div>
          <div class="si-row adv-col">
            <label class="adv-line">
              <button type="button" class="switch" :class="{ on: form.password_use }" role="switch"
                :aria-checked="form.password_use ? 'true' : 'false'" @click="form.password_use = !form.password_use">
                <i class="knob"></i>
              </button>
              <span class="lab">{{ t('settings.passwordUse') }}</span>
            </label>
            <input v-if="form.password_use" v-model="form.password" class="adv-input" type="password"
              :placeholder="t('settings.password')" maxlength="64" spellcheck="false" />
            <div class="import-hint">{{ t('settings.passwordHint') }}</div>
          </div>
          <div class="si-row adv-col">
            <span class="lab">{{ t('settings.agentAddr') }}</span>
            <input v-model="form.agent_addr" class="adv-input" placeholder="ip:2425（留空关闭）" spellcheck="false" />
            <div class="import-hint">{{ t('settings.agentHint') }}</div>
          </div>
          <div class="si-row adv-col">
            <span class="lab">{{ t('settings.dirMode') }}</span>
            <select v-model="form.dir_mode" class="adv-input">
              <option value="off">{{ t('settings.dirModeOff') }}</option>
              <option value="user">{{ t('settings.dirModeUser') }}</option>
              <option value="master">{{ t('settings.dirModeMaster') }}</option>
            </select>
            <input v-if="form.dir_mode === 'user'" v-model="form.master_addr" class="adv-input"
              :placeholder="t('settings.masterAddr')" spellcheck="false" />
            <div class="import-hint">{{ t('settings.dirHint') }}</div>
          </div>
          <div class="si-row adv-col">
            <label class="adv-line">
              <button type="button" class="switch" :class="{ on: form.allow_send_list }" role="switch"
                :aria-checked="form.allow_send_list ? 'true' : 'false'" @click="form.allow_send_list = !form.allow_send_list">
                <i class="knob"></i>
              </button>
              <span class="lab">{{ t('settings.allowSendList') }}</span>
            </label>
          </div>
          <div class="si-row adv-col">
            <label class="adv-line">
              <button type="button" class="switch" :class="{ on: form.ipdict_enabled }" role="switch"
                :aria-checked="form.ipdict_enabled ? 'true' : 'false'" @click="form.ipdict_enabled = !form.ipdict_enabled">
                <i class="knob"></i>
              </button>
              <span class="lab">{{ t('settings.ipdict') }}</span>
            </label>
          </div>
          <div class="si-row adv-col">
            <label class="adv-line">
              <button type="button" class="switch" :class="{ on: form.v6_mcast }" role="switch"
                :aria-checked="form.v6_mcast ? 'true' : 'false'" @click="form.v6_mcast = !form.v6_mcast">
                <i class="knob"></i>
              </button>
              <span class="lab">{{ t('settings.v6mcast') }}</span>
            </label>
            <div class="import-hint">{{ t('settings.v6mcastHint') }}</div>
          </div>
        </div>

        <div class="selfinfo">
          <div class="si-title">{{ t('settings.shot') }}</div>
          <div class="si-row">
            <label class="lab" for="shot-hotkey">{{ t('settings.shotHotkey') }}</label>
            <input
              id="shot-hotkey"
              class="hotkey-input"
              readonly
              :value="form.shot_hotkey || ''"
              :placeholder="t('settings.shotHotkeyPh')"
              @keydown="onHotkeyKeydown"
              @focus="recording = true"
              @blur="recording = false"
            />
          </div>
          <div class="si-row">
            <label class="lab" for="shot-copy">{{ t('settings.shotCopy') }}</label>
            <input id="shot-copy" type="checkbox" v-model="form.shot_copy_clipboard" />
          </div>
          <p class="import-hint">{{ hotkeyHint }}</p>
        </div>

        <div class="selfinfo">
          <div class="si-title">{{ t('settings.chatRecords') }}</div>
          <div class="si-row" style="display:block">
            <button class="btn-plain" :disabled="importing" @click="importIpmsg">
              {{ importing ? t('settings.importing') : t('settings.importBtn') }}
            </button>
            <div class="import-hint">{{ t('settings.importHint') }}</div>
          </div>
        </div>

        <div class="selfinfo" style="margin-top:12px">
          <div class="si-title">{{ t('settings.selfInfo') }}</div>
          <div class="si-row"><span>{{ t('settings.hostname') }}</span><b>{{ store.config?.hostname || '-' }}</b></div>
          <div class="si-row"><span>{{ t('settings.ip') }}</span><b>{{ (store.config?.ips || []).join(t('sep.list')) || '-' }}</b></div>
          <div class="si-row"><span>{{ t('settings.port') }}</span><b>UDP/TCP 2425</b></div>
        </div>

        <p class="note">{{ t('settings.note') }}</p>
      </div>

      <footer>
        <button v-if="canClose" class="btn-plain" @click="close">{{ t('cancel') }}</button>
        <button class="btn-primary" @click="save">{{ t('settings.save') }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  background: var(--c-mask);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  /* 弹窗贴边留白：小窗口时 max-height 以此为基准收缩（见 .modal） */
  padding: 24px;
}
.modal {
  width: 440px;
  background: var(--c-card);
  border-radius: 10px;
  box-shadow: 0 12px 40px var(--c-shadow);
  overflow: hidden;
  /* 内容超高时收缩到视口内，body 区内部滚动，头部/底部按钮始终可见 */
  display: flex;
  flex-direction: column;
  max-height: calc(100vh - 48px);
  max-height: calc(100dvh - 48px);
}
header {
  height: 44px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 16px;
  font-size: 14px;
  font-weight: 600;
  border-bottom: 1px solid var(--c-hairline);
}
.x {
  font-size: 13px;
  color: var(--c-sub);
  width: 24px;
  height: 24px;
  border-radius: 4px;
}
.x:hover {
  background: var(--c-list);
  color: var(--c-text);
}
.body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 16px 20px 6px;
}
.field {
  display: flex;
  align-items: center;
  margin-bottom: 12px;
}
.lab {
  min-width: 64px;
  flex: none;
  font-size: 13px;
  color: var(--c-text);
  padding-right: 10px;
}
.field input,
.field select,
.adv-input {
  flex: 1;
  height: 30px;
  border: 1px solid var(--c-border);
  border-radius: 4px;
  background: var(--c-card);
  color: var(--c-text);
  padding: 0 8px;
  font-size: 13px;
  user-select: text;
  min-width: 0;
}
/* WebKitGTK 会用原生控件画 select（底色不受 CSS 控制，深色下就成了浅底浅字），
   这里关掉原生外观并自绘箭头，保证两种主题下都受控 */
.field select,
select.adv-input {
  appearance: none;
  -webkit-appearance: none;
  padding-right: 26px;
  background-image: linear-gradient(45deg, transparent 50%, var(--c-sub) 50%),
    linear-gradient(135deg, var(--c-sub) 50%, transparent 50%);
  background-position: right 13px center, right 8px center;
  background-size: 5px 5px, 5px 5px;
  background-repeat: no-repeat;
  cursor: pointer;
}
.field select option,
select.adv-input option {
  background: var(--c-card);
  color: var(--c-text);
}
.field input:focus,
.field select:focus,
.adv-input:focus {
  border-color: var(--c-accent);
}
.dir-row {
  flex: 1;
  display: flex;
  gap: 8px;
}
.dir-input {
  background: var(--c-card-alt);
  color: var(--c-sub);
}
/* 消息加密开关 + 本机密钥指纹 */
.enc-col {
  flex: 1;
  min-width: 0;
}
.enc-line {
  display: flex;
  align-items: center;
  gap: 8px;
}
.switch {
  position: relative;
  width: 38px;
  height: 21px;
  border-radius: 11px;
  background: var(--c-border);
  transition: background 0.15s;
  flex: none;
}
.switch.on {
  background: var(--c-accent);
}
.switch .knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 17px;
  height: 17px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 2px var(--c-shadow);
  transition: left 0.15s;
}
.switch.on .knob {
  left: 19px;
}
.enc-state {
  font-size: 12.5px;
  color: var(--c-sub);
}
.enc-hint {
  font-size: 11.5px;
  color: var(--c-weak);
  margin-top: 4px;
}
.fp-row {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
  cursor: pointer;
  min-width: 0;
}
.fp-lab {
  font-size: 11.5px;
  color: var(--c-sub);
  flex: none;
}
.fp-val {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 11.5px;
  color: var(--c-text);
  background: var(--c-card-alt);
  border: 1px solid var(--c-hairline);
  border-radius: 4px;
  padding: 2px 6px;
  user-select: text;
  overflow: hidden;
  white-space: nowrap;
}
.fp-row:hover .fp-val {
  border-color: var(--c-accent);
}
.fp-copied {
  font-size: 11.5px;
  color: var(--c-accent);
  flex: none;
}
.selfinfo {
  background: var(--c-card-alt);
  border-radius: 6px;
  padding: 10px 12px;
  margin-top: 4px;
}
.si-title {
  font-size: 12px;
  color: var(--c-sub);
  margin-bottom: 6px;
}
.si-row {
  display: flex;
  font-size: 12.5px;
  line-height: 22px;
}
.si-row span {
  width: 64px;
  flex: none;
  color: var(--c-sub);
}
/* 协议扩展项包含标题、控件和说明，不能沿用 .si-row 的横向双列表格。 */
.adv-col {
  flex-direction: column;
  align-items: stretch;
  line-height: normal;
  padding: 8px 0;
}
.adv-col + .adv-col {
  border-top: 1px solid var(--c-hairline);
}
.adv-line {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  min-height: 30px;
}
.adv-col .lab {
  width: auto;
  min-width: 0;
  padding-right: 0;
  color: var(--c-text);
  line-height: 1.4;
}
.adv-input {
  width: 100%;
  flex: none;
  margin-top: 6px;
}
.adv-col .import-hint {
  margin-top: 5px;
}
.import-hint {
  font-size: 11.5px;
  color: var(--c-weak);
  line-height: 1.6;
  margin-top: 8px;
}
/* 截图热键录制框：只读，值只能由按键录制写入（区别于普通可输入框）。
   变量名必须用本仓库 global.css 里真实存在的 --c-*（与 .dir-input 同款），
   想当然写 --line/--bg-soft/--fg 会被整条丢弃 → 变成无边框透明框 */
.hotkey-input {
  width: 160px;
  padding: 4px 8px;
  border: 1px solid var(--c-border);
  border-radius: 4px;
  background: var(--c-card-alt);
  color: var(--c-text);
  text-align: center;
  cursor: pointer;
}
.note {
  font-size: 11.5px;
  color: var(--c-weak);
  margin: 10px 0 8px;
}
footer {
  flex: none;
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 12px 20px 16px;
}
</style>
