<script setup>
// 右侧聊天窗口：头部 / 消息流（日期分隔+气泡）/ 工具栏 / 输入区
import { ref, computed, watch, nextTick } from 'vue'
import {
  store, sendText, sendFiles, downloadFile,
  displayName, dayLabel, fmtTime, fmtSize, refreshUsers,
} from '../store'
import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import Avatar from './Avatar.vue'
import EmojiPicker from './EmojiPicker.vue'

const activeUser = computed(
  () => store.userMap[store.activeKey] || store.peerMeta[store.activeKey] || null
)
const msgs = computed(() => store.chats[store.activeKey]?.msgs || [])

/* ---------- 渲染列表：插入日期分隔 + 连续消息聚合 ---------- */
const viewList = computed(() => {
  const out = []
  let prev = null
  for (const m of msgs.value) {
    if (!prev || dayChanged(prev, m)) {
      out.push({ kind: 'day', id: 'day-' + m.ts + '-' + out.length, label: dayLabel(m.ts) })
    }
    const dayInserted = out.length && out[out.length - 1].kind === 'day'
    const merged =
      prev && !dayInserted &&
      prev.dir === m.dir && prev.kind === m.kind && m.ts - prev.ts < 180
    out.push({ kind: 'msg', id: 'm-' + m.pkt + '-' + out.length, m, firstOfCluster: !merged })
    prev = m
  }
  return out
})
function dayChanged(a, b) {
  return new Date(a.ts * 1000).toDateString() !== new Date(b.ts * 1000).toDateString()
}

/* ---------- 滚动控制 ---------- */
const scroller = ref(null)
let autoBottom = true
function onScroll() {
  const el = scroller.value
  if (!el) return
  autoBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 90
}
function scrollBottom(smooth = false) {
  nextTick(() => {
    const el = scroller.value
    if (!el || !autoBottom) return
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' })
  })
}
watch(() => store.activeKey, () => {
  autoBottom = true
  scrollBottom()
})
watch(() => msgs.value.length, () => scrollBottom(true))

/* ---------- 发送 ---------- */
const draft = ref('')
const ta = ref(null)

watch(() => store.activeKey, () => nextTick(() => ta.value?.focus()))

async function doSend() {
  if (!store.activeKey) return
  const text = draft.value
  if (!text.trim()) return
  try {
    await sendText(text.replace(/\n{3,}/g, '\n\n').trimEnd())
    draft.value = ''
    autoBottom = true
  } catch (e) {
    alert('发送失败：' + e)
  }
}
function onKeydown(e) {
  // Enter 发送；Ctrl/Cmd+Enter 与 Shift+Enter 换行（微信PC习惯）
  if (e.key === 'Enter' && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.altKey) {
    e.preventDefault()
    doSend()
  }
}

async function pickFiles() {
  if (!store.activeKey) return
  try {
    const sel = await openFileDialog({ multiple: true, title: '选择要发送的文件' })
    if (!sel) return
    await sendFiles(Array.isArray(sel) ? sel : [sel])
    autoBottom = true
  } catch (e) {
    alert('发送文件失败：' + e)
  }
}

/* ---------- 表情 ---------- */
const emojiOpen = ref(false)
function insertEmoji(e) {
  const el = ta.value
  if (!el) return
  const s = el.selectionStart ?? draft.value.length
  draft.value = draft.value.slice(0, s) + e + draft.value.slice(el.selectionEnd ?? s)
  nextTick(() => {
    el.focus()
    el.selectionStart = el.selectionEnd = s + e.length
  })
}

/* ---------- 文件卡片动作 ---------- */
function pct(f) {
  if (!f.total) return 0
  return Math.min(100, Math.round(((f.transferred || 0) / f.total) * 100))
}
async function openFile(path) {
  try { await openPath(path) } catch (e) { alert('打开失败：' + e) }
}
async function revealFile(path) {
  try { await revealItemInDir(path) } catch (e) { alert('打开文件夹失败：' + e) }
}
</script>

<template>
  <section class="chat-window">
    <!-- 头部 -->
    <header v-if="activeUser" class="cw-head">
      <div class="peer">
        <div class="name">{{ displayName(store.activeKey) }}</div>
        <div class="sub">
          {{ activeUser.host || '' }}<template v-if="activeUser.ip"> · {{ activeUser.ip }}</template>
          <template v-if="activeUser.group"> · {{ activeUser.group }}</template>
          <i v-if="!store.userMap[store.activeKey]" class="off-tag">离线</i>
        </div>
      </div>
      <button class="mini-btn" title="重新广播上线，刷新在线用户" @click="refreshUsers">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none">
          <path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
      </button>
    </header>

    <!-- 消息区 -->
    <div v-if="activeUser" ref="scroller" class="msgs" @scroll="onScroll">
      <div v-for="v in viewList" :key="v.id">
        <div v-if="v.kind === 'day'" class="day-sep"><span>{{ v.label }}</span></div>

        <div v-else class="msg-row" :class="[v.m.dir === 'out' ? 'self' : 'peer', { merge: !v.firstOfCluster }]">
          <Avatar class="m-ava" :name="v.m.dir === 'out' ? store.config?.nickname : displayName(store.activeKey)"
            :seed="v.m.dir === 'out' ? 'self' : store.activeKey" :size="34" />
          <div class="bubble-wrap">
            <div class="bubble" :class="{ file: v.m.kind === 'file' }">
              <div v-if="v.m.text" class="b-text">{{ v.m.text }}</div>
              <div v-for="f in v.m.files || []" :key="f.id" class="file-card">
                <div class="fc-icon">
                  <svg width="26" height="26" viewBox="0 0 24 24" fill="none">
                    <path d="M6 3h8l4 4v14H6V3z" stroke="#7a7a7a" stroke-width="1.6" stroke-linejoin="round" />
                    <path d="M14 3v4h4" stroke="#7a7a7a" stroke-width="1.6" stroke-linejoin="round" />
                  </svg>
                </div>
                <div class="fc-main">
                  <div class="fc-name ellipsis" :title="f.name">{{ f.name }}</div>
                  <div class="fc-sub">
                    <span>{{ fmtSize(f.size) }}</span>
                    <!-- 收到的文件 -->
                    <template v-if="v.m.dir === 'in'">
                      <template v-if="f.state === 'pending'">
                        <a @click.prevent="downloadFile(v.m, f)">下载</a>
                      </template>
                      <template v-else-if="f.state === 'downloading'">
                        <span>{{ pct(f) }}%</span>
                        <i class="bar"><i :style="{ width: pct(f) + '%' }"></i></i>
                      </template>
                      <template v-else-if="f.state === 'done'">
                        <span class="ok">已保存</span>
                        <a @click.prevent="openFile(f.path)">打开</a>
                        <a @click.prevent="revealFile(f.path)">所在文件夹</a>
                      </template>
                      <template v-else-if="f.state === 'failed'">
                        <span class="err">失败</span>
                        <a @click.prevent="downloadFile(v.m, f)">重试</a>
                      </template>
                    </template>
                    <!-- 发出的文件 -->
                    <template v-else>
                      <span class="ok">已发送</span>
                      <a @click.prevent="revealFile(f.path)">所在文件夹</a>
                    </template>
                  </div>
                </div>
              </div>
            </div>
            <div class="m-time" :class="{ self: v.m.dir === 'out' }">{{ fmtTime(v.m.ts) }}</div>
          </div>
        </div>
      </div>
      <div style="height: 10px"></div>
    </div>

    <!-- 空态 -->
    <div v-else class="placeholder">
      <svg width="72" height="72" viewBox="0 0 24 24" fill="none">
        <path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.6c-.5.42-1.3.07-1.3-.6V5.5z"
          stroke="#d9d9d9" stroke-width="1.4" stroke-linejoin="round" />
      </svg>
      <p>选择一个会话，开始聊天</p>
      <p class="sub">局域网内基于 IPMsg 协议（UDP/TCP 2425）</p>
    </div>

    <!-- 输入区 -->
    <footer v-if="activeUser" class="composer">
      <div class="toolbar">
        <button title="表情" @click="emojiOpen = !emojiOpen">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6" />
            <circle cx="9" cy="10" r="1.2" fill="currentColor" />
            <circle cx="15" cy="10" r="1.2" fill="currentColor" />
            <path d="M8.2 14c.9 1.4 2.2 2.1 3.8 2.1s2.9-.7 3.8-2.1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button title="发送文件" @click="pickFiles">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <path d="M11 5.5A3.5 3.5 0 0 1 17.9 7l.6 6.5a5.5 5.5 0 0 1-11 .5L7 8" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" transform="rotate(45 12 12)" />
            <path d="M13.5 8.5l-5 5a2.5 2.5 0 0 0 3.5 3.5l5.5-5.5a4 4 0 1 0-5.7-5.7L6 11" stroke="currentColor"
              stroke-width="1.6" stroke-linecap="round" />
          </svg>
        </button>
        <button class="disabled" title="截图功能开发中" disabled>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
            <rect x="3" y="6" width="18" height="14" rx="2" stroke="currentColor" stroke-width="1.6" />
            <path d="M8 6l1.5-2.5h5L16 6" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
            <circle cx="12" cy="13" r="3.4" stroke="currentColor" stroke-width="1.6" />
          </svg>
        </button>
      </div>

      <EmojiPicker v-if="emojiOpen" @pick="insertEmoji" />

      <textarea
        ref="ta"
        v-model="draft"
        class="input-area"
        placeholder="输入消息…"
        spellcheck="false"
        @keydown="onKeydown"
      ></textarea>

      <div class="composer-foot">
        <span class="hint">Enter 发送 / Ctrl+Enter 换行</span>
        <button class="send-btn" :disabled="!draft.trim()" @click="doSend">
          发送<span class="s-key">(S)</span>
        </button>
      </div>
    </footer>
  </section>
</template>

<style scoped>
.chat-window {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--c-chat);
  position: relative;
}
.cw-head {
  flex: none;
  height: 52px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 14px;
  border-bottom: 1px solid #e4e4e4;
}
.peer .name {
  font-size: 15px;
  font-weight: 600;
}
.peer .sub {
  font-size: 11.5px;
  color: var(--c-sub);
}
.off-tag {
  margin-left: 6px;
  font-style: normal;
  background: #c9c9c9;
  color: #fff;
  border-radius: 3px;
  font-size: 10px;
  padding: 0 4px;
}
.mini-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  color: #777;
  display: flex;
  align-items: center;
  justify-content: center;
}
.mini-btn:hover {
  background: rgba(0, 0, 0, 0.07);
  color: #333;
}

.msgs {
  flex: 1;
  overflow-y: auto;
  padding: 16px 18px 4px;
}
.day-sep {
  text-align: center;
  margin: 8px 0 14px;
}
.day-sep span {
  font-size: 11px;
  color: #a8a8a8;
  background: rgba(0, 0, 0, 0.04);
  border-radius: 3px;
  padding: 2px 8px;
}
.msg-row {
  display: flex;
  gap: 10px;
  margin-bottom: 4px;
}
.msg-row.merge {
  margin-top: -2px;
}
.msg-row.self {
  flex-direction: row-reverse;
}
.m-ava {
  margin-top: 2px;
}
.bubble-wrap {
  max-width: 62%;
  min-width: 0;
  display: inline-flex;
  flex-direction: column;
}
.msg-row.peer .bubble-wrap {
  align-items: flex-start;
}
.msg-row.self .bubble-wrap {
  align-items: flex-end;
}
.bubble {
  position: relative;
  background: var(--c-bubble-peer);
  border-radius: 6px;
  padding: 8px 11px;
  line-height: 1.55;
  font-size: 14px;
  word-break: break-word;
  white-space: pre-wrap;
  box-shadow: 0 1px 1px rgba(0, 0, 0, 0.03);
  max-width: 100%;
}
.bubble.file {
  white-space: normal;
}
.msg-row.self .bubble {
  background: var(--c-bubble-self);
}
.bubble::before {
  content: '';
  position: absolute;
  top: 11px;
  border: 6px solid transparent;
}
.msg-row.peer .bubble::before {
  left: -11px;
  border-right-color: var(--c-bubble-peer);
}
.msg-row.self .bubble::before {
  right: -11px;
  border-left-color: var(--c-bubble-self);
}
.m-time {
  font-size: 10.5px;
  color: #ababab;
  margin-top: 3px;
}

/* 文件卡片 */
.file-card {
  display: flex;
  gap: 8px;
  align-items: center;
  background: rgba(255, 255, 255, 0.85);
  border: 1px solid #ececec;
  border-radius: 6px;
  padding: 7px 10px;
  min-width: 210px;
  margin-top: 6px;
}
.b-text + .file-card {
  margin-top: 8px;
}
.fc-icon {
  flex: none;
}
.fc-main {
  flex: 1;
  min-width: 0;
}
.fc-name {
  font-size: 13px;
  max-width: 220px;
}
.fc-sub {
  font-size: 11.5px;
  color: var(--c-sub);
  display: flex;
  gap: 8px;
  align-items: center;
  margin-top: 2px;
}
.fc-sub a {
  color: #576b95;
  cursor: pointer;
}
.fc-sub a:hover {
  text-decoration: underline;
}
.ok {
  color: var(--c-accent);
}
.err {
  color: var(--c-danger);
}
.bar {
  width: 90px;
  height: 4px;
  border-radius: 2px;
  background: #e5e5e5;
  overflow: hidden;
  display: inline-block;
}
.bar > i {
  display: block;
  height: 100%;
  background: var(--c-accent);
  transition: width 0.2s;
}

.placeholder {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: #c2c2c2;
  font-size: 14px;
}
.placeholder .sub {
  font-size: 11.5px;
  color: #d4d4d4;
}

/* 输入区 */
.composer {
  flex: none;
  border-top: 1px solid #e4e4e4;
  background: #fbfbfb;
  position: relative;
}
.toolbar {
  display: flex;
  gap: 4px;
  padding: 7px 12px 0;
}
.toolbar button {
  width: 30px;
  height: 30px;
  border-radius: 6px;
  color: #6f6f6f;
  display: flex;
  align-items: center;
  justify-content: center;
}
.toolbar button:hover:not(.disabled) {
  background: rgba(0, 0, 0, 0.06);
  color: #333;
}
.toolbar .disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.input-area {
  display: block;
  width: calc(100% - 24px);
  height: 88px;
  margin: 4px 12px 0;
  resize: none;
  font-size: 14px;
  line-height: 1.6;
  user-select: text;
}
.composer-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 4px 12px 10px;
}
.hint {
  font-size: 11px;
  color: #b5b5b5;
}
.send-btn {
  border: 1px solid #d4d4d4;
  background: #f5f5f5;
  border-radius: 4px;
  padding: 5px 22px;
  font-size: 13px;
  color: #333;
}
.send-btn:hover:not(:disabled) {
  background: #efefef;
}
.send-btn:disabled {
  opacity: 0.55;
  cursor: default;
}
.s-key {
  font-size: 11px;
  color: #999;
}
</style>
