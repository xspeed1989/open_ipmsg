<script setup>
// 设置弹窗：昵称 / 群组 / 下载目录 / 语言 / 主题 / 发送编码 / 消息加密 + 本机信息
import { reactive, watch, computed, ref } from 'vue'
import { applyTheme, store, refreshConfig, refreshUsers, loadSessions } from '../store'
import * as ipc from '../lib/ipc'
import { t, SUPPORTED_LANGS, LANG_NAMES, setLocale, detectLocale } from '../lib/i18n'
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
    }
  },
  { immediate: true }
)

const canClose = computed(() => !store.firstRun)

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
.field select {
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
.field select {
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
.field select option {
  background: var(--c-card);
  color: var(--c-text);
}
.field input:focus,
.field select:focus {
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
.import-hint {
  font-size: 11.5px;
  color: var(--c-weak);
  line-height: 1.6;
  margin-top: 8px;
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
