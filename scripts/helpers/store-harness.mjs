/**
 * 在 Node 里驱动**真 store** —— 事件迁移 characterization 网的地基。
 *
 * 为什么要这层：`src/store.js` 的事件→状态迁移只能靠 `scripts/*.test.mjs` 里
 * 正则抠函数体 + `new Function` 注入自由变量来测，那种测试跨的 seam 是**源码文本**
 * —— 缩进、换行、局部变量名一变就红，而真正的缺陷（事件到达顺序、谁先 await 谁）
 * 恰好长在它够不到的编排层。
 *
 * 这里跨的是真 seam：真 `store.js` + 真 `src/lib/ipc.js`，后端换成 Tauri 官方
 * `@tauri-apps/api/mocks` 的 `mockIPC`（生产里是真 Tauri 运行时）。事件也能真投进
 * 已注册的监听器：`plugin:event|listen` 会被拦下来记下 handler id，测试再从
 * `window.__TAURI_INTERNALS__.callbacks` 取回那个回调调用它。
 *
 * 硬前提：`store.js` 的相对 import 带显式 `.js`（Node ESM 不解析省略扩展名的相对
 * 路径，Vite 解析）。这条由 513fd0c 备好。
 *
 * `store.js` 是模块单例（模块级 reactive 对象），所以一个测试文件只 `boot()` 一次，
 * 用例之间用 `await h.reset()` 回到干净基线。
 */
import { nextTick } from 'vue'

let shimInstalled = false

/** Tauri 运行时在网页里提供的东西，Node 里得自己摆好（只摆最小集：一个 window 名）；
 *  窗口元信息交给官方 mockWindows 摆（getCurrentWindow() 要读 metadata.currentWindow.label） */
function installTauriShim() {
  if (shimInstalled) return
  shimInstalled = true
  globalThis.window = globalThis
  // 通知插件走的是 Web Notification API（不是 IPC）：不摆一个就抛 TypeError，
  // 被 store 的 try/catch 吞掉。这里摆成「用户拒绝过」，通知路径变成可预期的空转。
  globalThis.Notification = { permission: 'denied' }
}

/**
 * 基线后端应答：只保留 boot() 与事件路径会读到的命令，未声明的命令一律回 null。
 *
 * **数组与对象一律用函数发新副本**：`ensureChat` 会把 `get_history` 返回的数组
 * 直接存成 `chats[key].msgs` 并往里 push，`deleteContact` 会 splice `get_users` 的
 * 返回值……共用同一个常量实例会让上一条用例的写入脏下一条。真机上每次都是新反序列
 * 化出来的对象，所以这坑只在测试里现形。（已踩过一次：msg-out 用例里凭空多出一条
 * 上一条用例的入站消息。）
 */
const DEFAULT_BACKEND = {
  get_config: () => ({
    nickname: '我', group: '', hostname: 'me-host', lang: 'zh', theme: 'auto',
    encoding: 'utf8', download_dir: '', encrypt: false, password_use: false,
    absence_enabled: false, absence_text: '', shot_copy_clipboard: true,
    auto_open_secret: true,
  }),
  get_users: () => [],
  list_sessions: () => [],
  list_emojis: () => [],
  get_history: () => [],
  mark_read: 0,
  mark_out_read: 0,
  set_unread: null,
  'plugin:event|listen': 1,
  'plugin:event|unlisten': null,
  'plugin:notification|is_permission_granted': false,
  'plugin:notification|request_permission': 'denied',
}

/** 用例之间的干净基线（只列会被事件改动的那些字段） */
function baseline(store) {
  store.users = []
  store.userMap = {}
  store.sessions = []
  store.sessionList = []
  store.chats = {}
  store.peerMeta = {}
  store.unread = {}
  store.unreadTs = {}
  store.lastTs = {}
  store.activeKey = null
  store.locate = null
  store.dragHoverKey = ''
  store.search = ''
  store.searchHits = []
  store.searching = false
  store.emojiFiles = []
  store.emojiCacheFiles = []
  store.windowFocused = true
  store.firstRun = false
  store.settingsOpen = false
}

/**
 * 装好 shim 与 mockIPC，import 真 store 并 boot()。
 * @param {{responses?: object, boot?: boolean}} [opts] responses 覆盖基线应答；
 *   值可以是常量，也可以是 `(args) => value` / 返回 Promise 的函数（做异步时序用）
 */
export async function startStore({ responses = {}, boot = true } = {}) {
  installTauriShim()
  const { mockIPC, mockWindows } = await import('@tauri-apps/api/mocks')
  mockWindows('main')
  const backend = { ...DEFAULT_BACKEND, ...responses }
  /** 基线副本：reset() 用它抹掉用例改过的命令应答（比如「永不给结果的 get_history」） */
  const pristineBackend = { ...backend }
  const log = []
  const listeners = new Map()

  mockIPC((cmd, args) => {
    log.push({ cmd, args })
    if (cmd === 'plugin:event|listen') {
      listeners.set(args.event, args.handler)
      return 1
    }
    const answer = backend[cmd]
    if (typeof answer === 'function') return answer(args)
    return answer === undefined ? null : answer
  })

  const mod = await import('../../src/store.js')
  if (boot) await mod.boot()
  // boot() 的流水要单独留一份：reset() 会清 log，但「订阅早于加载」这类接线
  // 事实属于启动一次的事实
  const bootLog = [...log]

  return {
    mod,
    store: mod.store,
    /** 运行时可改的后端应答表（做「慢 get_history」这类时序用例）。
     *  值是常量时会被调用方直接持有 —— 数组/对象请用函数发新副本，见文件头的注解 */
    backend,
    /** 后端收到的命令流水：[{cmd, args}] */
    log,
    /** boot() 那次启动的流水快照（reset() 不会动它） */
    bootLog,
    /** 已注册的事件名 → handler id */
    listeners,
    calls: (cmd) => log.filter((e) => e.cmd === cmd),
    commands: () => log.map((e) => e.cmd),
    countOf: (cmd) => log.filter((e) => e.cmd === cmd).length,
    /** 某条命令在流水里的首次/末次下标（断言相对顺序用） */
    firstAt: (cmd) => log.findIndex((e) => e.cmd === cmd),
    lastAt: (cmd) => log.map((e) => e.cmd).lastIndexOf(cmd),
    /** 等一个条件成真（给 fire-and-forget 的事件回调用，避免拿 setImmediate 碰运气） */
    async waitUntil(pred, { ticks = 200 } = {}) {
      for (let i = 0; i < ticks; i += 1) {
        if (pred()) return true
        await new Promise((r) => setImmediate(r))
      }
      throw new Error('waitUntil 超时：条件在 200 个 tick 内没成真')
    },
    /** 把一条事件真的投进 store 注册的监听器 */
    emit(event, payload) {
      const id = listeners.get(event)
      if (id === undefined) throw new Error(`store 没有注册「${event}」监听器`)
      const cb = globalThis.window.__TAURI_INTERNALS__.callbacks.get(id)
      if (typeof cb !== 'function') throw new Error(`「${event}」的 callback 不在注册表里`)
      return cb({ event, id, payload })
    },
    /** 清空流水、把后端应答与所有被事件改动的字段都还原到基线 */
    async reset() {
      log.length = 0
      for (const cmd of Object.keys(backend)) delete backend[cmd]
      Object.assign(backend, pristineBackend)
      baseline(mod.store)
      await nextTick() // 让 watch(totalUnread) 等调度器里的活跑完，免得污染下一条用例
      log.length = 0
    },
  }
}

/** 后端会发给前端的入站消息记录（形状照 net.rs 的 msg-in payload） */
export const inRecord = (over = {}) => ({
  dir: 'in', kind: 'text', text: '你好', files: [], ts: 1_700_000_000, pkt: 101,
  peer: { key: '10.0.0.9', ip: '10.0.0.9', nickname: '对端', host: 'peer-host', group: '' },
  need_read: true, read: false, enc: false, sig_ok: true,
  secret: false, locked: false, unlocked: false, broadcast: false,
  ...over,
})

/** 后端会发给前端的出站消息记录（本地已上屏过的那条） */
export const outRecord = (over = {}) => ({
  dir: 'out', kind: 'text', text: '我发的', files: [], ts: 1_700_000_000, pkt: 201,
  peer: { key: '10.0.0.9', nickname: '对端' },
  rcpt: true, read: false, delivered: false, ...over,
})

/** 会话 key 与对端记录里的一致 */
export const PEER = '10.0.0.9'
