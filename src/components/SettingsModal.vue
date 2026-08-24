<script setup>
// 设置弹窗：昵称 / 群组 / 下载目录 / 主题 / 发送编码 + 本机信息
import { reactive, watch, computed, ref } from 'vue'
import { applyTheme, store, refreshConfig, refreshUsers, loadSessions } from '../store'
import * as ipc from '../lib/ipc'
import { open as pickDialog } from '@tauri-apps/plugin-dialog'

const form = reactive({
  nickname: '',
  group: '',
  download_dir: '',
  encoding: 'utf8',
  theme: 'system',
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
    }
  },
  { immediate: true }
)

const canClose = computed(() => !store.firstRun)

// 选中即预览：不必按保存就能看到效果；取消关闭时再还原成已保存的主题
watch(() => form.theme, (t) => applyTheme(t))
function restoreSavedTheme() {
  applyTheme(store.config?.theme)
}

async function chooseDir() {
  const dir = await pickDialog({ directory: true, title: '选择接收文件的保存目录' })
  if (dir) form.download_dir = dir
}

const importing = ref(false)
// 从官方 IP Messenger 的日志库（v4.5+ 的 ipmsg.db）导入历史聊天记录。
// 后端按 msg_id 去重，重复选择同一个文件执行也不会产生重复记录。
async function importIpmsg() {
  const picked = await pickDialog({
    multiple: true,
    title: '选择官方 IP Messenger 的日志数据库',
    filters: [{ name: 'IPMsg 日志库（ipmsg.db）', extensions: ['db'] }],
  })
  const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter(Boolean)
  if (!paths.length || importing.value) return
  importing.value = true
  try {
    const r = await ipc.importIpmsgLogs(paths)
    await loadSessions()
    const lines = r.files.map((f) =>
      f.ok ? `✔ ${f.path.split(/[\\/]/).pop()}：导入 ${f.imported} 条` : `✘ ${f.path}\n  ${f.error}`
    )
    let msg =
      r.failed > 0
        ? `${lines.join('\n')}`
        : `导入完成：共 ${r.total} 条消息` +
          (r.skipped ? `（跳过 ${r.skipped} 条已存在/备忘录）` : '') +
          (r.sessionsNew ? `，新增 ${r.sessionsNew} 个会话` : '')
    if (!r.total && !r.failed) msg += '\n没有新消息可导入。'
    alert(msg)
  } catch (e) {
    alert('导入失败：' + e)
  } finally {
    importing.value = false
  }
}

function close() {
  if (!canClose.value) return
  restoreSavedTheme() // 放弃未保存的改动，主题跟着回退
  store.settingsOpen = false
}

async function save() {
  if (!form.nickname.trim()) {
    alert('请填写昵称')
    return
  }
  const patch = {
    nickname: form.nickname.trim(),
    group: form.group.trim(),
    download_dir: form.download_dir.trim(),
    encoding: form.encoding,
    theme: form.theme,
  }
  try {
    await ipc.saveConfig(patch)
    await refreshConfig()
    await refreshUsers()
    store.firstRun = false
    store.settingsOpen = false
  } catch (e) {
    alert('保存失败：' + e)
  }
}
</script>

<template>
  <div class="overlay" @click.self="close">
    <div class="modal">
      <header>
        <span>设置</span>
        <button v-if="canClose" class="x" @click="close">✕</button>
      </header>

      <div class="body">
        <label class="field">
          <span class="lab">昵称</span>
          <input v-model="form.nickname" placeholder="在局域网内显示的名字" maxlength="32" spellcheck="false" />
        </label>
        <label class="field">
          <span class="lab">群组</span>
          <input v-model="form.group" placeholder="如：研发部（可留空）" maxlength="32" spellcheck="false" />
        </label>
        <label class="field">
          <span class="lab">接收目录</span>
          <div class="dir-row">
            <input v-model="form.download_dir" readonly class="dir-input" :title="form.download_dir" />
            <button class="btn-plain" @click="chooseDir">选择…</button>
          </div>
        </label>
        <label class="field">
          <span class="lab">外观主题</span>
          <select v-model="form.theme">
            <option value="system">跟随系统</option>
            <option value="light">浅色</option>
            <option value="dark">深色</option>
          </select>
        </label>
        <label class="field">
          <span class="lab">发送编码</span>
          <select v-model="form.encoding">
            <option value="utf8">UTF-8（推荐，客户端间互通）</option>
            <option value="gbk">GBK（兼容老版中文飞鸽）</option>
          </select>
        </label>

        <div class="selfinfo">
          <div class="si-title">聊天记录</div>
          <div class="si-row" style="display:block">
            <button class="btn-plain" :disabled="importing" @click="importIpmsg">
              {{ importing ? '导入中…' : '导入官方 IP Messenger 聊天记录…' }}
            </button>
            <div class="import-hint">
              选择官方 IPMsg（v4.5+）的日志数据库 ipmsg.db，可多选；按对方 IP 归入对应会话，
              附件仅记录文件名。可重复执行，不会产生重复记录。
            </div>
          </div>
        </div>

        <div class="selfinfo" style="margin-top:12px">
          <div class="si-title">本机信息</div>
          <div class="si-row"><span>主机名</span><b>{{ store.config?.hostname || '-' }}</b></div>
          <div class="si-row"><span>IP 地址</span><b>{{ (store.config?.ips || []).join('，') || '-' }}</b></div>
          <div class="si-row"><span>协议端口</span><b>UDP/TCP 2425</b></div>
        </div>

        <p class="note">修改昵称/群组后会自动重新向局域网广播上线。</p>
      </div>

      <footer>
        <button v-if="canClose" class="btn-plain" @click="close">取消</button>
        <button class="btn-primary" @click="save">保存</button>
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
}
.modal {
  width: 440px;
  background: var(--c-card);
  border-radius: 10px;
  box-shadow: 0 12px 40px var(--c-shadow);
  overflow: hidden;
}
header {
  height: 44px;
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
  padding: 16px 20px 6px;
}
.field {
  display: flex;
  align-items: center;
  margin-bottom: 12px;
}
.lab {
  width: 64px;
  flex: none;
  font-size: 13px;
  color: var(--c-text);
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
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 12px 20px 16px;
}
</style>
