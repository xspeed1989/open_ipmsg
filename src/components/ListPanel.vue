<script setup>
// 中栏：会话列表（消息页）/ 按群组分组的联系人（通讯录页）
import { computed } from 'vue'
import { store, displayName, fmtListTime, openChat, dayLabel } from '../store'
import Avatar from './Avatar.vue'

function matchUser(key, name) {
  const q = store.search.trim().toLowerCase()
  if (!q) return true
  const u = store.userMap[key]
  const hay = [
    name,
    key,
    u?.host,
    u?.group,
    u?.user,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
  return hay.includes(q)
}

function previewOf(last) {
  if (!last) return ''
  if (last.kind === 'file') {
    const n = (last.files || []).length
    const firstName = last.files?.[0]?.name || '文件'
    return `[文件] ${firstName}${n > 1 ? ` 等${n}个` : ''}`
  }
  return (last.text || '').replace(/\s+/g, ' ')
}

const sessions = computed(() => {
  const keys = new Set()
  for (const k of Object.keys(store.chats)) {
    if (store.chats[k].msgs.length) keys.add(k)
  }
  for (const k of Object.keys(store.unread)) keys.add(k)

  const arr = []
  for (const k of keys) {
    const msgs = store.chats[k]?.msgs || []
    const last = msgs[msgs.length - 1]
    const name = displayName(k)
    if (!matchUser(k, name)) continue
    arr.push({
      key: k,
      name,
      preview: previewOf(last),
      time: store.lastTs[k] || last?.ts || 0,
      unread: store.unread[k] || 0,
      online: !!store.userMap[k],
      group: store.userMap[k]?.group || store.peerMeta[k]?.group || '',
    })
  }
  arr.sort((a, b) => b.time - a.time)
  return arr
})

/** 通讯录：按群组分组 */
const contactGroups = computed(() => {
  const q = store.search.trim().toLowerCase()
  const groups = {}
  for (const u of store.users) {
    const hay = `${u.nickname} ${u.host} ${u.ip} ${u.group} ${u.user}`.toLowerCase()
    if (q && !hay.includes(q)) continue
    const g = u.group || '未分组'
    ;(groups[g] ||= []).push(u)
  }
  return Object.entries(groups)
    .sort(([a], [b]) => a.localeCompare(b, 'zh'))
    .map(([g, list]) => ({
      group: g,
      users: list.sort((x, y) => (x.nickname || '').localeCompare(y.nickname || '', 'zh')),
    }))
})

function openSession(s) {
  openChat(s.key)
}
function openContact(u) {
  openChat(u.key)
}
</script>

<template>
  <aside class="list-panel">
    <div class="search-wrap">
      <div class="search">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none">
          <circle cx="10.5" cy="10.5" r="6" stroke="#9a9a9a" stroke-width="2" />
          <path d="M15 15l5 5" stroke="#9a9a9a" stroke-width="2" stroke-linecap="round" />
        </svg>
        <input v-model="store.search" placeholder="搜索" spellcheck="false" />
      </div>
    </div>

    <!-- 会话列表 -->
    <div v-if="store.page === 'chat'" class="rows">
      <div
        v-for="s in sessions"
        :key="s.key"
        class="row"
        :class="{ active: store.activeKey === s.key }"
        @click="openSession(s)"
      >
        <div class="ava">
          <Avatar :name="s.name" :seed="s.key" :size="40" />
          <i v-if="!s.online" class="off-dot" title="离线"></i>
        </div>
        <div class="mid">
          <div class="r1 ellipsis">{{ s.name }}</div>
          <div class="r2 ellipsis">{{ s.preview }}</div>
        </div>
        <div class="right">
          <div class="time">{{ fmtListTime(s.time) }}</div>
          <i v-if="s.unread" class="badge">{{ s.unread > 99 ? '99+' : s.unread }}</i>
        </div>
      </div>

      <div v-if="!sessions.length" class="empty-tip">
        <p>暂无会话</p>
        <p class="sub">同一局域网内打开对方也会出现在通讯录，发消息后即建立会话</p>
      </div>
    </div>

    <!-- 通讯录 -->
    <div v-else class="rows">
      <template v-for="g in contactGroups" :key="g.group">
        <div class="group-head">{{ g.group }}（{{ g.users.length }}）</div>
        <div v-for="u in g.users" :key="u.key" class="row contact" @click="openContact(u)">
          <Avatar :name="u.nickname || u.user" :seed="u.key" :size="36" />
          <div class="mid">
            <div class="r1 ellipsis">{{ u.nickname || u.user }}</div>
            <div class="r2 ellipsis">{{ u.host }} · {{ u.ip }}</div>
          </div>
        </div>
      </template>
      <div v-if="!contactGroups.length" class="empty-tip">
        <p>局域网内暂无其他用户</p>
        <p class="sub">请确认对方已运行 IPMsg 客户端（UDP 端口 2425），或点击聊天窗口右上角「刷新」重新广播</p>
      </div>
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
  height: 26px;
  background: #dfdfdf;
  border-radius: 4px;
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 0 8px;
}
.search:focus-within {
  background: #fff;
  outline: 1px solid #c3c3c3;
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
  flex-direction: column;
  align-items: flex-end;
  gap: 5px;
}
.time {
  font-size: 11px;
  color: var(--c-sub);
}
.badge {
  min-width: 16px;
  height: 16px;
  padding: 0 4px;
  border-radius: 8px;
  background: var(--c-danger);
  color: #fff;
  font-size: 10.5px;
  font-style: normal;
  line-height: 16px;
  text-align: center;
}
.ava {
  position: relative;
}
.off-dot {
  position: absolute;
  right: -1px;
  bottom: -1px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: #c0c0c0;
  border: 1.5px solid var(--c-list);
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
  color: #a5a5a5;
  font-size: 13px;
  line-height: 1.7;
}
.empty-tip .sub {
  font-size: 11.5px;
  color: #bdbdbd;
}
</style>
