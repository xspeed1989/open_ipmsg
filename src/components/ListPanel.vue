<script setup>
// 中栏：联系人列表（按群组分组，固定显示，未读角标提示新消息）
import { computed } from 'vue'
import { store, openChat } from '../store'
import Avatar from './Avatar.vue'

function matchUser(u) {
  const q = store.search.trim().toLowerCase()
  if (!q) return true
  const hay = `${u.nickname} ${u.host} ${u.ip} ${u.group} ${u.user}`
    .toLowerCase()
  return hay.includes(q)
}

/** 按群组分组的联系人；组内未读优先，其余按昵称排序 */
const contactGroups = computed(() => {
  const groups = {}
  for (const u of store.users) {
    if (!matchUser(u)) continue
    const g = u.group || '未分组'
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
          (x.nickname || '').localeCompare(y.nickname || '', 'zh')
      ),
    }))
})

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

    <div class="rows">
      <template v-for="g in contactGroups" :key="g.group">
        <div class="group-head">{{ g.group }}（{{ g.users.length }}）</div>
        <div
          v-for="u in g.users"
          :key="u.key"
          class="row contact"
          :class="{ active: store.activeKey === u.key }"
          @click="openContact(u)"
        >
          <div class="ava">
            <Avatar :name="u.nickname || u.user || '?'" :seed="u.key" :size="36" />
            <i class="status-dot on" title="在线"></i>
          </div>
          <div class="mid">
            <div class="r1 ellipsis">{{ u.nickname || u.user || '未知用户' }}</div>
            <div class="r2 ellipsis">{{ u.host }} · {{ u.ip }}{{ u.group ? ' · ' + u.group : '' }}</div>
          </div>
          <div class="right">
            <i v-if="store.unread[u.key]" class="badge">
              {{ store.unread[u.key] > 99 ? '99+' : store.unread[u.key] }}
            </i>
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
  align-items: center;
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
