// node --test scripts/ —— 「本地那份删了，就再也下不回来」的回归用例
//
// 真机问题：对方发来的文件下载过、卡片显示「已保存 / 打开 / 所在文件夹」，用户在
// 文件管理器里把本地那份删掉之后：
//   ① 卡片仍然说「已保存」，点「打开」是一条已经不存在的路径；
//   ② 收件卡片上没有再次下载的入口（右键菜单也没有），于是那个文件再也下不回来。
//
// 后端本来就不拦重复下载（download_file_task 没有「下过一次就拒绝」的门；落盘按
// 内容哈希判重，命中复用同一份、不造副本，见 docs/adr/0004），缺的只是前端的入口
// 与「状态说真话」：
//   ① 入口：done 状态的收件卡片**永远**有「重新下载」（同时是「下载坏的那份」的
//      逃生口），点了真的再发一次 download_file，且 pkt / rid / size 原样带回
//      （rid 的进制各家不一，必须原样回传）；
//   ② 状态：本地缺失时不再显示「打开 / 所在文件夹」，改说「本地文件已删除」；
//   ③ 有界：存在性核对一次 invoke 问整批，不按文件逐个问、不挂轮询定时器；
//   ④ 反面也一样：failed 说的是这一次下载没成，不等于本地没有 —— 重新下载一个早就
//      落好盘的文件时碰上对端离线，历史落到 failed 但 path 与字节都还在，这时仍然
//      要给「打开」，否则手里有完好的文件反而打不开（同一个病：界面说的不是本地真相）。
//
// 跨的是真 seam：真 store.js + 真 src/lib/ipc.js（后端换成官方 mockIPC），
// 模板侧沿用 sfc-bindings.test.mjs 的源码断言手法。
import { readFileSync } from 'node:fs'
import { test, before, beforeEach } from 'node:test'
import assert from 'node:assert/strict'
import { startStore, PEER } from './helpers/store-harness.mjs'
import { applyEvent } from '../src/lib/transitions.js'
import { t, setLocale } from '../src/lib/i18n.js'

const chatSource = readFileSync(
  new URL('../src/components/ChatWindow.vue', import.meta.url),
  'utf8'
)
const ipcSource = readFileSync(new URL('../src/lib/ipc.js', import.meta.url), 'utf8')
const storeSource = readFileSync(new URL('../src/store.js', import.meta.url), 'utf8')
const libRs = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8')
const netRs = readFileSync(new URL('../src-tauri/src/net.rs', import.meta.url), 'utf8')

/** 收件卡片里 done 那一段（渲染分支的结构断言都从这里取，避免误判别的状态） */
function doneBlock() {
  const start = chatSource.indexOf("<template v-else-if=\"f.state === 'done'\">")
  assert.ok(start > 0, 'ChatWindow.vue 里找不到 done 状态的收件卡片分支')
  const end = chatSource.indexOf("<template v-else-if=\"f.state === 'failed'\">", start)
  assert.ok(end > start, 'done 分支后面应当是 failed 分支')
  return chatSource.slice(start, end)
}

/** 收件卡片里 failed 那一段（同一张卡片，失败也要说真话）。
 *  切到「发出的文件」那条注释为止：再往下就是发件侧了。 */
function failedBlock() {
  const start = chatSource.indexOf("<template v-else-if=\"f.state === 'failed'\">")
  assert.ok(start > 0, 'ChatWindow.vue 里找不到 failed 状态的收件卡片分支')
  const stop = chatSource.indexOf('发出的文件', start)
  assert.ok(stop > start, 'failed 分支后面应当跟着「发出的文件」分支')
  return chatSource.slice(start, stop)
}

/* ---------------- ① 入口：done 状态永远能重新下载 ---------------- */

test('done 状态的收件卡片永远有「重新下载」入口（不只文件丢了才显示）', () => {
  const block = doneBlock()
  assert.match(
    block,
    /<a[^>]*@click\.prevent="downloadFile\(v\.m, f\)"[^>]*>\{\{ t\('chat\.redownload'\) \}\}<\/a>/,
    'done 卡片上缺少重新下载入口：文件删了/坏了就再也下不回来'
  )
  // 位置：它在 missing 分支**之外**（否则「文件还在」时就没有入口了）
  const missingAt = block.indexOf('<template v-if="f.missing">')
  const elseAt = block.indexOf('<template v-else>', missingAt)
  const linkAt = block.indexOf("t('chat.redownload')")
  assert.ok(missingAt > 0 && elseAt > missingAt, 'done 分支里应当先判 missing 再走 v-else')
  assert.ok(linkAt > elseAt, '重新下载链接必须在 missing / v-else 这对分支之外，恒常显示')
})

test('点「重新下载」确实再发一次 download_file，pkt/rid/size 原样带回', async () => {
  const h = await startStore()
  try {
    const f = {
      id: 9, name: '季度报表.pdf', size: 2048, rid: '1f', state: 'done', path: '/dl/季度报表.pdf',
    }
    const m = { dir: 'in', pkt: 4242, peer_key: PEER, files: [f] }
    await h.mod.downloadFile(m, f)

    const calls = h.calls('download_file')
    assert.equal(calls.length, 1, '重新下载必须真的落到后端命令上')
    assert.deepEqual(calls[0].args, {
      key: PEER,
      pktNo: 4242,
      fileId: 9,
      name: '季度报表.pdf',
      rid: '1f', // 进制不明，原样回传
      size: 2048,
      isDir: false,
    })
    assert.equal(f.state, 'downloading', '点了之后卡片要立刻进入下载中')
  } finally {
    await h.reset()
  }
})

test('文件夹附件的重新下载走 isDir=true（目录流那条路）', async () => {
  const h = await startStore()
  try {
    const f = { id: 3, name: '资料', size: 0, rid: '', state: 'done', path: '/dl/资料', dir_entry: true }
    const m = { dir: 'in', pkt: 77, peer: { key: PEER }, files: [f] }
    await h.mod.downloadFile(m, f)
    assert.equal(h.calls('download_file')[0].args.isDir, true)
    assert.equal(h.calls('download_file')[0].args.size, 0)
  } finally {
    await h.reset()
  }
})

/* ---------------- ② 状态说真话：本地缺失的标记与渲染 ---------------- */

test('本地缺失时不说「已保存」、不给打开/所在文件夹，只留重新下载', () => {
  const block = doneBlock()
  const missingAt = block.indexOf('<template v-if="f.missing">')
  const elseAt = block.indexOf('<template v-else>', missingAt)
  const missingBranch = block.slice(missingAt, elseAt)

  assert.match(missingBranch, /t\('chat\.localDeleted'\)/, '缺失时要明说「本地文件已删除」')
  assert.doesNotMatch(
    missingBranch,
    /openFile\(|revealFile\(/,
    '文件已经被删掉了，不能再给「打开 / 所在文件夹」（点开是一条死路径）'
  )
  // 文件还在的那一侧照旧：已保存 + 打开 + 所在文件夹
  const presentBranch = block.slice(elseAt)
  assert.match(presentBranch, /openFile\(f\.path\)/)
  assert.match(presentBranch, /revealFile\(f\.path\)/)
  assert.match(presentBranch, /chat\.saved/)
})

test('一次 invoke 问整批：批量核对 + 只标缺失的那些', async () => {
  const h = await startStore({
    responses: {
      // 后端逐项对齐地回：只有第二个路径还在
      local_files_exist: (args) => args.paths.map((p) => p === '/dl/b.txt'),
    },
  })
  try {
    const gone = { id: 1, name: 'a.txt', size: 1, state: 'done', path: '/dl/a.txt' }
    const kept = { id: 2, name: 'b.txt', size: 1, state: 'done', path: '/dl/b.txt' }
    const pending = { id: 3, name: 'c.txt', size: 1, state: 'failed', path: '' }
    h.store.chats[PEER] = {
      msgs: [{ dir: 'in', pkt: 1, files: [gone, kept, pending] }],
    }

    const missing = await h.mod.refreshLocalFiles(PEER)

    const calls = h.calls('local_files_exist')
    assert.equal(calls.length, 1, '一个会话只问一次（批量），不是每个文件一次')
    assert.deepEqual(calls[0].args.paths, ['/dl/a.txt', '/dl/b.txt'], '只问「已保存」的那些')
    assert.equal(missing, 1)
    assert.equal(gone.missing, true)
    assert.equal(kept.missing, false)
    assert.equal(pending.missing, undefined, '没下载完成的附件不该被贴「已删除」标签')
  } finally {
    await h.reset()
  }
})

test('IPC 问不到时不乱标已删除（宁可显示旧状态，也不藏起「打开」）', async () => {
  const h = await startStore({
    responses: {
      local_files_exist: () => {
        throw new Error('boom')
      },
    },
  })
  try {
    const f = { id: 1, name: 'a.txt', size: 1, state: 'done', path: '/dl/a.txt' }
    h.store.chats[PEER] = { msgs: [{ dir: 'in', pkt: 1, files: [f] }] }
    const missing = await h.mod.refreshLocalFiles(PEER)
    assert.equal(missing, 0)
    assert.ok(!f.missing, '核对失败时不能把用户的文件判成已删除')
  } finally {
    await h.reset()
  }
})

test('会话激活时核对一次：openChat 会问磁盘', async () => {
  const h = await startStore({
    responses: {
      get_history: () => [{ dir: 'in', pkt: 5, ts: 1, files: [
        { id: 1, name: 'a.txt', size: 1, state: 'done', path: '/dl/a.txt' },
      ] }],
      local_files_exist: () => [false], // 磁盘上那份已经不在了
    },
  })
  try {
    await h.mod.openChat(PEER)
    assert.equal(h.countOf('local_files_exist'), 1, '打开会话时批量核对一次')
    assert.equal(h.store.chats[PEER].msgs[0].files[0].missing, true, '历史里那条已保存的文件不在了')
  } finally {
    await h.reset()
  }
})

test('下载完成后重新核对一次（重新下载成功要立刻回到「已保存」）', () => {
  // 迁移层：done 事件 → 一条 refresh_local_files 副作用（ADR-0001 的形状）
  const state = {
    chats: {
      [PEER]: { msgs: [{ dir: 'in', pkt: 42, files: [{ id: 7, name: 'a.png', size: 10, state: 'downloading' }] }] },
    },
    pendingFileProgress: new Map(),
  }
  const done = { key: PEER, pkt: 42, file_id: 7, transferred: 10, total: 10, done: true, path: '/dl/a.png' }
  assert.deepEqual(applyEvent(state, 'file-progress', done), {
    effects: [{ type: 'refresh_local_files', key: PEER }],
  })
  assert.equal(state.chats[PEER].msgs[0].files[0].state, 'done')

  // 中途进度不触发核对（每个 150ms 一条，核对一次都嫌多）
  const mid = applyEvent(state, 'file-progress', { ...done, done: false, path: undefined })
  assert.deepEqual(mid.effects, [])

  // 消息还没入列（事件暂存）时也不核对：那时根本没有卡片可核对
  const empty = { chats: {}, pendingFileProgress: new Map() }
  const buffered = applyEvent(empty, 'file-progress', done)
  assert.deepEqual(buffered.effects, [])
  assert.equal(empty.pendingFileProgress.size, 1, '照旧暂存，等消息入列后回放')
})

test('存在性核对没有轮询定时器（用户删文件不产生任何事件）', () => {
  const body = storeSource.slice(
    storeSource.indexOf('export async function refreshLocalFiles'),
    storeSource.indexOf('/* ---------------- 聊天记录搜索'),
  )
  assert.ok(body.length > 200, '没取到 refreshLocalFiles 的函数体')
  assert.doesNotMatch(body, /setInterval|setTimeout|requestAnimationFrame/, '不许挂定时器轮询')
})

/* ---------------- ④ 反面：failed 不代表本地没有 ---------------- */

test('失败记录但本地文件还在：照样给「打开 / 所在文件夹」，重试也一直在', () => {
  const block = failedBlock()
  const guardAt = block.indexOf('<template v-if="f.path && !f.missing">')
  assert.ok(guardAt > 0, '失败分支必须按「有 path 且磁盘上还在」给打开链接')
  const guardEnd = block.indexOf('</template>', guardAt)
  const guardBranch = block.slice(guardAt, guardEnd)
  assert.match(guardBranch, /openFile\(f\.path\)/, '文件还在就要能打开')
  assert.match(guardBranch, /revealFile\(f\.path\)/, '文件还在就要能定位')
  // 重试不能因为这层判断被吞掉
  assert.match(block, /downloadFile\(v\.m, f\)/, '失败卡片必须留着重试')
  assert.match(block, /t\('chat\.failed'\)/)
})

test('失败记录且磁盘上没有：不给打开，只留失败与重试', async () => {
  // 渲染侧：打开链接被 !f.missing 挡着，标记为缺失时整段不出现
  const block = failedBlock()
  const guardAt = block.indexOf('<template v-if="f.path && !f.missing">')
  assert.ok(guardAt > 0)
  // 状态侧：核对磁盘确实会给 failed 条目贴上 missing（下面一条用例证明它进了批量）
  const h = await startStore({
    responses: { local_files_exist: (args) => args.paths.map(() => false) },
  })
  try {
    const f = { id: 1, name: 'a.txt', size: 3, state: 'failed', path: '/dl/a.txt' }
    h.store.chats[PEER] = { msgs: [{ dir: 'in', pkt: 9, files: [f] }] }
    await h.mod.refreshLocalFiles(PEER)
    assert.equal(f.missing, true, '磁盘上没有的失败条目也要标缺失，打开链接才会跟着消失')
    assert.equal(f.state, 'failed', '核对磁盘不改下载状态，只补一个事实')
  } finally {
    await h.reset()
  }
})

test('历史 failed+path 的条目也进批量核对（重开会话不说「说不清」的状态）', async () => {
  const h = await startStore({
    responses: {
      // 后端逐项对齐：失败的那条还在磁盘上，已保存那条反倒被删了
      local_files_exist: (args) => args.paths.map((p) => p === '/dl/keep.bin'),
    },
  })
  try {
    const failedKept = { id: 1, name: 'keep.bin', size: 3, state: 'failed', path: '/dl/keep.bin' }
    const doneGone = { id: 2, name: 'gone.bin', size: 3, state: 'done', path: '/dl/gone.bin' }
    const failedNoPath = { id: 3, name: 'never.bin', size: 3, state: 'failed', path: '' }
    h.store.chats[PEER] = { msgs: [{ dir: 'in', pkt: 9, files: [failedKept, doneGone, failedNoPath] }] }

    await h.mod.refreshLocalFiles(PEER)

    const calls = h.calls('local_files_exist')
    assert.equal(calls.length, 1, '还是一次 invoke 问整批')
    assert.deepEqual(
      calls[0].args.paths,
      ['/dl/keep.bin', '/dl/gone.bin'],
      '有 path 的 failed 条目要进核对；从没下载成功过（无 path）的不问'
    )
    assert.equal(failedKept.missing, false, '失败但文件还在 → 打开链接照给')
    assert.equal(doneGone.missing, true)
    assert.equal(failedNoPath.missing, undefined)
  } finally {
    await h.reset()
  }
})

/* ---------------- 文案与接线 ---------------- */

test('chat.redownload / chat.localDeleted 中英都有，不漏翻', () => {
  setLocale('zh-CN')
  assert.equal(t('chat.redownload'), '重新下载')
  assert.equal(t('chat.localDeleted'), '本地文件已删除')
  setLocale('en')
  // 必须断言具体英文：t() 在 en 缺 key 时会回退 zh，只断言「不等于 key」会把中文当通过
  assert.equal(t('chat.redownload'), 'Download again')
  assert.equal(t('chat.localDeleted'), 'Local file deleted')
  setLocale('zh-CN')
})

test('新命令走线上契约：ipc 封装 + invoke_handler 注册', () => {
  assert.match(ipcSource, /localFilesExist = \(paths\) => invoke\(WIRE\.cmd\.local_files_exist/)
  assert.match(libRs, /async fn local_files_exist\(paths: Vec<String>\) -> Vec<bool>/)
  const handler = libRs.split('generate_handler![')[1].split('])')[0]
  assert.match(handler, /\blocal_files_exist\b/, '新命令必须注册进 invoke_handler')
  // 判据是 exists() 不是 is_file()：目录附件的落盘结果是一个目录
  assert.match(libRs, /Path::new\(p\)\.exists\(\)/)
})

test('失败要收尾：「对方不在线」「目录建不出来」也推失败事件，不挂在下载中', () => {
  // 早退分支必须走 fail_download（只 return Err 的话前端卡片永远停在「下载中」）
  assert.match(
    netRs,
    /let Some\(peer\) = ctx\.st\.peers\.get\(key\) else \{\s*return fail_download_early\(ctx, key, pkt_no, file_id, "对方不在线"\);/
  )
  assert.match(
    netRs,
    /if let Err\(e\) = std::fs::create_dir_all\(&dir\) \{\s*return fail_download_early\(ctx, key, pkt_no, file_id, &format!\("创建接收目录失败: \{e\}"\)\);/
  )
  assert.match(netRs, /fn fail_download_early<T>\(/, '收尾要走同一个函数，别各写一遍')
})
