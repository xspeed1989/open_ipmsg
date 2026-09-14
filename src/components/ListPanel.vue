<script setup>
// 中栏：联系人列表（按群组分组，固定显示，未读角标提示新消息；右键可删除会话）
import { computed, ref, watch } from 'vue'
import { store, openChat, onSearchInput, openHit, displayName, fmtTime, deleteContact, getAbsenceInfoFor, isBroadcastKey } from '../store'
import { makeSnippet } from '../lib/text'
import { t } from '../lib/i18n'
import { confirm } from '../lib/dialog'
import Avatar from './Avatar.vue'

const q = computed(() => store.search.trim())
/** 命中条目的一行摘要：文本消息截命中处，文件消息显示文件名 */
function hitLine(h) {
  if (h.hit === 'file') return `${t('file.tag')} ${(h.files || []).join(t('sep.list'))}`
  return makeSnippet(h.text, q.value)
}
function hitTitle(h) {
  return `${h.nickname || displayName(h.key) || h.key} · ${fmtTime(h.ts)}`
}

function matchUser(u) {
  const q = store.search.trim().toLowerCase()
  if (!q) return true
  const hay = `${u.nickname} ${u.host} ${u.ip} ${u.group} ${u.user}`
    .toLowerCase()
  return hay.includes(q)
}

/** 按群组分组的会话；组内未读优先，其次在线，其余按昵称排序 */
const contactGroups = computed(() => {
  const groups = {}
  for (const u of store.sessionList) {
    if (!matchUser(u)) continue
    const g = u.group || t('ungrouped')
    ;(groups[g] ||= []).push(u)
  }
  const unreadOf = (u) => store.unread[u.key] || 0
  return Object.entries(groups)
    .sort(([a], [b]) => a.localeCompare(b, 'zh'))
    .map(([g, list]) => ({
      group: g,
      users: list.sort(
        (x, y) =>
          unreadOf(y) - unreadOf(x) ||
          Number(y.online) - Number(x.online) ||
          (x.nickname || '').localeCompare(y.nickname || '', 'zh')
      ),
    }))
})

function openContact(u) {
  openChat(u.key)
}

/* ---------- 右键删除会话（微信式） ---------- */

/** 右键菜单：{ x, y, key }；key 为待删除的联系人 */
const ctxMenu = ref(null)
const ctxMenuRef = ref(null)

function openContactCtx(u, e) {
  // 广播信箱是常驻条目：既不能删（删了立刻回来），也没有别的会话操作，
  // 索性不给菜单，避免"点了删除却没变化"的错觉
  if (isBroadcastKey(u.key)) return
  ctxMenu.value = {
    x: Math.min(e.clientX, window.innerWidth - 140),
    y: Math.min(e.clientY, window.innerHeight - 72),
    key: u.key,
  }
}
function closeCtx() {
  ctxMenu.value = null
}
function onCtxMouseDown(e) {
  const path = e.composedPath ? e.composedPath() : []
  if (path.includes(ctxMenuRef.value)) return
  closeCtx()
}
function onCtxKeyDown(e) {
  if (e.key === 'Escape') closeCtx()
}
watch(ctxMenu, (open) => {
  if (open) {
    document.addEventListener('mousedown', onCtxMouseDown, true)
    document.addEventListener('keydown', onCtxKeyDown)
  } else {
    document.removeEventListener('mousedown', onCtxMouseDown, true)
    document.removeEventListener('keydown', onCtxKeyDown)
  }
})

async function doDeleteContact() {
  const key = ctxMenu.value?.key
  if (!key) return
  closeCtx()
  const who = displayName(key)
  const ok = await confirm(t('list.deleteConfirm', { who }), {
    okLabel: t('list.deleteOk'),
    cancelLabel: t('cancel'),
    danger: true,
  })
  if (ok) await deleteContact(key)
}
</script>

<template>
  <aside class="list-panel">
    <div class="search-wrap">
      <div class="search">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none">
          <circle cx="10.5" cy="10.5" r="6" stroke="currentColor" stroke-width="2" />
          <path d="M15 15l5 5" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
        <input
          :value="store.search"
          :placeholder="t('list.searchPh')"
          spellcheck="false"
          @input="onSearchInput($event.target.value)"
        />
        <button v-if="q" class="clear" :title="t('list.clear')" @click="onSearchInput('')">✕</button>
      </div>
    </div>

    <div class="rows">
      <div v-if="q && contactGroups.length" class="group-head">
        {{ t('list.contactsCount', { n: contactGroups.reduce((n, g) => n + g.users.length, 0) }) }}
      </div>
      <template v-for="g in contactGroups" :key="g.group">
        <div v-if="!q" class="group-head">{{ g.group }}{{ t('list.groupCount', { n: g.users.length }) }}</div>
        <div
          v-for="u in g.users"
          :key="u.key"
          class="row contact"
          :data-user-key="u.key"
          :class="{ active: store.activeKey === u.key, off: !u.online && !isBroadcastKey(u.key), 'drag-hover': store.dragHoverKey === u.key }"
          @click="openContact(u)"
          @contextmenu.prevent="openContactCtx(u, $event)"
        >
          <div class="ava">
            <Avatar :name="isBroadcastKey(u.key) ? t('broadcast.name') : (u.nickname || u.user || '?')" :seed="u.key" :size="36" />
            <!-- 广播信箱不是对端，没有在线/离线可言：用固定的 📡 取代状态点 -->
            <i v-if="isBroadcastKey(u.key)" class="status-dot bc" :title="t('broadcast.sub')">📡</i>
            <i v-else class="status-dot" :class="u.online ? 'on' : 'off'" :title="u.online ? t('online') : t('offline')"></i>
          </div>
          <div class="mid">
            <div class="r1 ellipsis">
              {{ isBroadcastKey(u.key) ? t('broadcast.name') : (u.nickname || u.user || t('unknownUser')) }}
              <span v-if="u.online && u.absence" class="away-tag" :title="u.absence_text || t('chat.leave')"
  @click.stop="getAbsenceInfoFor(u.key).catch(() => {})">{{ t('chat.leave') }}</span>
            </div>
            <div v-if="isBroadcastKey(u.key)" class="r2 ellipsis">{{ t('broadcast.tip') }}</div>
            <div v-else class="r2 ellipsis">{{ u.host || '' }}{{ u.ip ? ' · ' + u.ip : '' }}{{ u.group ? ' · ' + u.group : '' }}</div>
          </div>
          <div class="right">
            <i v-if="store.unread[u.key]" class="badge">
              {{ store.unread[u.key] > 99 ? '99+' : store.unread[u.key] }}
            </i>
          </div>
        </div>
      </template>

      <!-- 聊天记录命中 -->
      <template v-if="q">
        <div class="group-head">
          {{ store.searching ? t('list.recordsSearching') : t('list.recordsCount', { n: store.searchHits.length }) }}
        </div>
        <div
          v-for="h in store.searchHits"
          :key="h.key + '-' + h.pkt + '-' + h.ts"
          class="row hit"
          @click="openHit(h)"
        >
          <div class="mid">
            <div class="r1 ellipsis">{{ hitTitle(h) }}</div>
            <div class="r2 ellipsis">{{ hitLine(h) }}</div>
          </div>
        </div>
        <div v-if="!store.searching && !store.searchHits.length" class="empty-tip">
          <p class="sub">{{ t('list.noMatch') }}</p>
        </div>
      </template>

      <div v-if="!contactGroups.length && !q" class="empty-tip">
        <p>{{ t('list.empty') }}</p>
        <p class="sub">{{ t('list.emptySub') }}</p>
      </div>
    </div>

    <!-- 右键菜单：删除会话 -->
    <div
      v-if="ctxMenu"
      ref="ctxMenuRef"
      class="ctx-menu"
      :style="{ position: 'fixed', left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }"
    >
      <!-- 广播信箱不对应任何对端，右键菜单在 openContactCtx 里直接不弹 -->
      <button class="ctx-item danger" @click="doDeleteContact">{{ t('list.deleteSession') }}</button>
    </div>
  </aside>
</template>

<style scoped>
.list-panel {
  width: var(--list-w);
  flex: none;
  background: var(--c-list);
  border-right: 1px solid var(--c-hairline);
  display: flex;
  flex-direction: column;
}
.search-wrap {
  padding: 10px 10px 6px;
}
.search {
  color: var(--c-sub);
  height: 26px;
  background: var(--c-hairline);
  border-radius: 4px;
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 0 8px;
}
.search:focus-within {
  background: var(--c-card);
  outline: 1px solid var(--c-weak);
}
.clear {
  flex: none;
  color: var(--c-sub);
  font-size: 11px;
  padding: 0 2px;
}
.clear:hover {
  color: var(--c-text);
}
.row.hit .r1 {
  font-size: 12px;
  color: var(--c-sub);
}
.row.hit .r2 {
  font-size: 12.5px;
  color: var(--c-text);
}
.search input {
  flex: 1;
  font-size: 12px;
  min-width: 0;
}
.rows {
  flex: 1;
  overflow-y: auto;
}
.row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  cursor: pointer;
}
.row:hover {
  background: var(--c-list-hover);
}
.row.active {
  background: var(--c-list-active);
}
/* 拖放文件时悬停命中的联系人：高亮提示松开后的落点会话 */
.row.drag-hover {
  background: var(--c-list-active);
  box-shadow: inset 0 0 0 2px var(--c-accent);
}
.mid {
  flex: 1;
  min-width: 0;
}
.r1 {
  font-size: 13.5px;
  line-height: 19px;
}
.r2 {
  font-size: 12px;
  color: var(--c-sub);
  line-height: 17px;
}
.right {
  display: flex;
  align-items: center;
}
.badge {
  min-width: 16px;
  height: 16px;
  padding: 0 4px;
  border-radius: 8px;
  background: var(--c-danger);
  color: var(--c-card);
  font-size: 10.5px;
  font-style: normal;
  line-height: 16px;
  text-align: center;
}
.ava {
  position: relative;
}
.away-tag {
  display: inline-block;
  margin-left: 6px;
  padding: 0 5px;
  font-size: 10px;
  line-height: 15px;
  color: var(--c-text-3, #999);
  border: 1px solid var(--c-border, #d0d0d0);
  border-radius: 4px;
  vertical-align: 1px;
}
.status-dot {
  position: absolute;
  right: -1px;
  bottom: -1px;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  border: 2px solid var(--c-list);
}
.row.active .status-dot {
  border-color: var(--c-list-active);
}
.status-dot.on {
  background: var(--c-accent);
}
/* 离线会话：灰点，整行弱化 */
.status-dot.off {
  background: var(--c-weak);
}
/* 广播信箱：不是对端，用 📡 取代在线状态点（不参与在线/离线语义） */
.status-dot.bc {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--c-accent);
  font-size: 8px;
  line-height: 1;
  font-style: normal;
}
.row.contact.off .r1 {
  color: var(--c-sub);
}
.group-head {
  padding: 8px 12px 4px;
  font-size: 12px;
  color: var(--c-sub);
}
.empty-tip {
  margin-top: 60px;
  padding: 0 20px;
  text-align: center;
  color: var(--c-sub);
  font-size: 13px;
  line-height: 1.7;
}
.empty-tip .sub {
  font-size: 11.5px;
  color: var(--c-weak);
}

/* ---------- 右键菜单（删除会话） ---------- */
.ctx-menu {
  z-index: 40;
  min-width: 96px;
  padding: 4px;
  background: var(--c-card);
  border: 1px solid var(--c-hairline);
  border-radius: 8px;
  box-shadow: 0 6px 24px var(--c-shadow);
}
.ctx-item {
  display: block;
  width: 100%;
  padding: 6px 10px;
  border-radius: 4px;
  text-align: left;
  font-size: 13px;
  color: var(--c-text);
}
.ctx-item:hover {
  background: var(--c-list-hover);
}
.ctx-item.danger {
  color: var(--c-danger);
}
.ctx-item.danger:hover {
  background: var(--c-danger);
  color: var(--c-card);
}
</style>
