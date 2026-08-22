<script setup>
// 设置弹窗：昵称 / 群组 / 下载目录 / 发送编码 + 本机信息
import { reactive, watch, computed } from 'vue'
import { store, refreshConfig, refreshUsers } from '../store'
import * as ipc from '../lib/ipc'
import { open as pickDialog } from '@tauri-apps/plugin-dialog'

const form = reactive({
  nickname: '',
  group: '',
  download_dir: '',
  encoding: 'utf8',
})

watch(
  () => store.settingsOpen,
  (open) => {
    if (open && store.config) {
      form.nickname = store.config.nickname || ''
      form.group = store.config.group || ''
      form.download_dir = store.config.download_dir || ''
      form.encoding = store.config.encoding || 'utf8'
    }
  },
  { immediate: true }
)

const canClose = computed(() => !store.firstRun)

async function chooseDir() {
  const dir = await pickDialog({ directory: true, title: '选择接收文件的保存目录' })
  if (dir) form.download_dir = dir
}

function close() {
  if (canClose.value) store.settingsOpen = false
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
          <span class="lab">发送编码</span>
          <select v-model="form.encoding">
            <option value="utf8">UTF-8（推荐，客户端间互通）</option>
            <option value="gbk">GBK（兼容老版中文飞鸽）</option>
          </select>
        </label>

        <div class="selfinfo">
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
  background: rgba(0, 0, 0, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
}
.modal {
  width: 440px;
  background: #fff;
  border-radius: 10px;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
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
  border-bottom: 1px solid #eee;
}
.x {
  font-size: 13px;
  color: #999;
  width: 24px;
  height: 24px;
  border-radius: 4px;
}
.x:hover {
  background: #f2f2f2;
  color: #333;
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
  color: #555;
}
.field input,
.field select {
  flex: 1;
  height: 30px;
  border: 1px solid #ddd;
  border-radius: 4px;
  padding: 0 8px;
  font-size: 13px;
  user-select: text;
  min-width: 0;
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
  background: #fafafa;
  color: #666;
}
.selfinfo {
  background: #f7f9f8;
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
.note {
  font-size: 11.5px;
  color: #b5b5b5;
  margin: 10px 0 8px;
}
footer {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 12px 20px 16px;
}
</style>
