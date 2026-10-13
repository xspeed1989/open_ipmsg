// node --test scripts/ —— 自定义表情包界面契约（源码级断言，不依赖浏览器运行时）
//
// 这些断言锁的是「不能被误改掉的行为」：面板结构、缩略尺寸上限、
// 右键收纳入口、发送链路与协议复用。改动这些地方时必须同步改测试。
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { WIRE } from '../src/lib/wire.generated.js'
import { normalizeEmojis } from '../src/lib/emoji.js'

const picker = readFileSync(new URL('../src/components/EmojiPicker.vue', import.meta.url), 'utf8')
const chat = readFileSync(new URL('../src/components/ChatWindow.vue', import.meta.url), 'utf8')
const ipc = readFileSync(new URL('../src/lib/ipc.js', import.meta.url), 'utf8')

function cssOf(src) {
  return src.match(/<style\b[^>]*>([\s\S]*?)<\/style>/)?.[1] || ''
}
function declarations(css, selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const matches = css.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}[^{]*\\{([^}]*)\\}`, 'g'))
  return Array.from(matches, (m) => m[1]).join('\n')
}

/* ---------------- 表情面板 ---------------- */

test('面板没有顶部页签，改由底部两个图标按钮切换视图', () => {
  assert.doesNotMatch(picker, /class="tabs"/, '顶部页签应已移除')
  assert.doesNotMatch(picker, /class="tab"/)
  assert.match(picker, /class="panel-bar"/)
  const bar = picker.slice(picker.indexOf('class="panel-bar"'), picker.indexOf('class="panel-bar"') + 2600)
  assert.equal((bar.match(/class="bar-icon/g) || []).length, 2, '底部只应有两个图标按钮')
  assert.match(picker, /emoji\.tabUnicode/)
  assert.match(picker, /emoji\.stickerHint/)
  // 「拖动可排序…」那行提示已按需求去掉
  assert.doesNotMatch(picker, /emoji\.manageHint/)
  assert.match(picker, /function stickerHint/)
  // 三个视图：普通表情 → 图片表情包 → ❤️
  assert.match(picker, /if \(view\.value === 'unicode'\) view\.value = 'library'/)
  assert.match(picker, /else if \(view\.value === 'library' \|\| view\.value === 'pack'\) view\.value = 'stickers'/)
})

test('❤️ 页就是图片表情包本身：与表情库同一份数据、同一个顺序', () => {
  const favBlock = picker.slice(picker.indexOf("view === 'stickers'"), picker.indexOf("<!-- 自定义表情库 -->"))
  // 渲染的是 emojis（图片表情），不是 emoji、也不是另一份「收藏」数据
  assert.match(favBlock, /v-for="e in emojis"/)
  assert.match(favBlock, /emit\('pickSticker', e\)/)
  // 支持拖拽排序（与表情库同一套 onDrop → reorder_emojis 落库）
  assert.match(favBlock, /@drop\.prevent="onDrop\(\$event, e\)"/)
  assert.match(favBlock, /class="sticker"/)
  // 整个组件里不能再有「收藏」语义
  assert.doesNotMatch(picker, /favorites|favHas|favThumbs|promoteFav|persistFav|localStorage/)
})

test('「移到最前」写回后端 = 顺序持久化到 registry.json', () => {
  assert.match(picker, /async function menuMoveFront/)
  assert.match(picker, /list\.unshift\(hit\)/)
  assert.match(picker, /await ipc\.reorderEmojis\(list\.map\(\(x\) => x\.id\)\)/)
  // 已在最前 / 表情已不存在都要有明确反馈，不能静默
  assert.match(picker, /alreadyFirst \? t\('emoji\.alreadyFirst'\) : t\('emoji\.moved'\)/)
  assert.match(picker, /if \(i < 0\) \{[\s\S]{0,80}emoji\.gone/)
  // 拖拽排序同样落库
  assert.match(picker, /async function onDrop/)
  assert.match(picker, /reorderEmojis\(/)
})

test('右键菜单：图片表情上是「移到最前 / 删除」，空白处只有导入导出', () => {
  const menu = picker.slice(picker.indexOf('class="menu"'), picker.indexOf('class="menu"') + 1200)
  // 有 entry 时才显示移到最前 / 删除
  assert.match(menu, /v-if="menu\.entry"/)
  assert.match(menu, /emoji\.moveFront/)
  assert.match(menu, /emoji\.delete/)
  // 导入导出一律可用（底部行为条已不再放它们）
  assert.match(menu, /emoji\.importImages/)
  assert.match(menu, /emoji\.importPack/)
  assert.match(menu, /emoji\.exportPack/)
  assert.match(picker, /function openBlankMenu/)
  assert.match(picker, /function menuImportImages/)
  assert.match(picker, /function menuImportPack/)
  assert.match(picker, /function menuExportPack/)
  // 底部行为条里不能再有 bar-btn（导入导出已移走）
  assert.doesNotMatch(picker, /class="bar-btn"/)
  assert.doesNotMatch(picker, /bar-sep/)
})

test('面板宽度刚好容纳 9 列：横向不裁切、纵向可滚动', () => {
  const css = cssOf(picker)
  const panel = declarations(css, '.emoji-panel')
  // 边框 2 + 行内边距 18 + 网格 392 + 纵向滚动条约 10 = 422：
  // 宽度必须把滚动条算进去，否则内容多到出滚动条时第 9 列又被裁（实测差 6px）
  assert.match(panel, /width:\s*422px/, '面板宽度必须容纳整行 9 列 + 纵向滚动条')
  assert.match(panel, /padding:\s*10px 9px 8px/)
  assert.match(panel, /--emoji-cols:\s*9/)
  assert.match(panel, /--emoji-cell:\s*40px/)
  assert.match(panel, /--emoji-gap:\s*4px/)
  const uni = declarations(css, '.uni')
  assert.match(uni, /grid-template-columns:\s*repeat\(var\(--emoji-cols\),\s*var\(--emoji-cell\)\)/)
  assert.match(uni, /overflow-y:\s*auto/)
  assert.match(uni, /overflow-x:\s*hidden/)
  const stickers = declarations(css, '.stickers')
  assert.match(stickers, /overflow-y:\s*auto/)
  assert.match(stickers, /overflow-x:\s*hidden/)
  // 固定格子尺寸（1fr 会把格子越挤越小且不滚动）；
  // 且必须为纵向滚动条留宽度：5×71+4×6 = 379 ≤ 382（内容盒 392 − 滚动条约 10）
  assert.match(stickers, /--sticker-cell:\s*71px/)
  assert.match(stickers, /min-height:\s*0/, 'flex 子项 min-height:auto 会把网格压扁成不滚动')
  assert.match(stickers, /grid-template-columns:\s*repeat\(var\(--sticker-cols\),\s*var\(--sticker-cell\)\)/)
  assert.match(stickers, /grid-auto-rows:\s*var\(--sticker-cell\)/)
  assert.match(declarations(css, '.panel-body'), /overflow:\s*hidden/)
})

test('面板吞掉滚轮事件，不让它滚到后面的聊天窗口', () => {
  // 面板是挂在 ChatWindow 里的 fixed 层，事件会冒泡到 .msgs 滚动容器 → 聊天区被带着滚
  assert.match(picker, /class="emoji-panel"[^>]*@wheel\.stop/, '面板根节点必须 @wheel.stop')
})

test('底部行为条按钮成组靠左，不分散到两端', () => {
  const css = cssOf(picker)
  const bar = declarations(css, '.panel-bar')
  assert.match(bar, /justify-content:\s*flex-start/, '应靠左成组')
  assert.doesNotMatch(bar, /space-between/, '不能再左右分散')
  assert.match(bar, /gap:\s*6px/)
  assert.match(declarations(css, '.bar-icon'), /width:\s*34px/)
  assert.match(picker, /function stickerHint/)
  // 心形一个按钮走三个视图：普通表情 → 表情包 → 常用
  assert.match(picker, /if \(view\.value === 'unicode'\) view\.value = 'library'/)
  assert.match(picker, /else if \(view\.value === 'library' \|\| view\.value === 'pack'\) view\.value = 'stickers'/)
})

test('面板高度固定：切到表情少的视图也不会缩水', () => {
  const css = cssOf(picker)
  const body = declarations(css, '.panel-body')
  assert.match(body, /height:\s*var\(--panel-body-h\)/)
  assert.match(declarations(css, '.emoji-panel'), /--panel-body-h:\s*352px/)
  // 自定义库/包预览都铺满固定内容区，而不是各自撑高
  assert.match(css, /\.lib-body,\s*\n\.pack-body \{[^}]*flex:\s*1/)
  assert.match(declarations(css, '.stickers'), /overflow-y:\s*auto/)
  assert.match(declarations(css, '.stickers'), /flex:\s*1/)
})

test('滚动发生在固定内容区内，面板本身不会被内容撑高', () => {
  const css = cssOf(picker)
  // 内容区固定高 + 裁切；网格与包列表各自滚动，而不是把面板越撑越高
  assert.match(declarations(css, '.panel-body'), /overflow:\s*hidden/)
  assert.match(declarations(css, '.stickers'), /overflow-y\s*:\s*auto/)
  assert.match(declarations(css, '.pack-list'), /overflow-y\s*:\s*auto/)
})

test('缩略图滚动到可见才加载（懒加载 data URL），失败留占位不再重试', () => {
  assert.match(picker, /async function loadThumb/)
  assert.match(picker, /thumbs\.value\[e\.id\] = 'loading'/)
  assert.match(picker, /读取失败|catch \{[\s\S]{0,80}thumbs\.value\[e\.id\] = ''/)
  // 库里可能有上千张：打开面板不能逐张 read_image_data，交给 IntersectionObserver
  assert.match(picker, /new IntersectionObserver/)
  assert.match(picker, /function watchCell/)
  assert.match(picker, /:ref="\(el\) => watchCell\(el, e\)"/)
  assert.doesNotMatch(
    picker,
    /for \(const e of emojis\.value\) loadThumb\(e\)/,
    '不允许在 loadList 里全量预取缩略图',
  )
})

test('导入对话框与收藏使用同一份表情格式白名单', () => {
  assert.match(ipc, /import \{ STICKER_EXTS \} from '\.\/emoji\.js'/)
  assert.match(ipc, /extensions: STICKER_EXTS/)
  assert.match(picker, /ipc\.pickStickerFiles\(\)/)
})

test('WebM 在表情网格与悬停预览用 video 静音循环播放，WebP 仍用 img', () => {
  const videos = [...picker.matchAll(/<video\b[\s\S]*?\/>/g)].map((m) => m[0])
  assert.equal(videos.length, 4, '两种表情视图的网格与悬停预览都需要视频渲染')
  for (const video of videos) {
    assert.match(video, /v-if="isVideoSticker\(/)
    for (const attr of ['autoplay', 'loop', 'muted', 'playsinline']) {
      assert.match(video, new RegExp(`\\b${attr}\\b`))
    }
    assert.match(video, /:src="thumbs\[/)
    assert.doesNotMatch(video, /\bcontrols\b/)
  }
  assert.equal((picker.match(/<img v-else\b/g) || []).length, 4)
  assert.match(cssOf(picker), /\.sticker img,\s*\.sticker video\s*\{/)
  assert.match(cssOf(picker), /\.hover-preview img,\s*\.hover-preview video\s*\{/)
})

test('聊天 WebM 预览可右键收藏，解码失败退回文件卡片且不反复读取', () => {
  const video = chat.match(/<video\b[\s\S]*?\/>/)?.[0] || ''
  assert.match(video, /isVideoSticker\(f\.name\)/)
  for (const attr of ['autoplay', 'loop', 'muted', 'playsinline']) {
    assert.match(video, new RegExp(`\\b${attr}\\b`))
  }
  assert.match(video, /@contextmenu\.prevent="openCtx\(v\.m, \$event\)"/)
  assert.match(video, /@error="f\._previewFailed = true"/)
  assert.match(chat, /v-if="looksLikeSticker\(f\.name\) && f\.src && !f\._previewFailed"/)
  assert.match(chat, /if \(!looksLikeSticker\(f\.name\) \|\| f\.src \|\| f\._imgLoading \|\| f\._previewFailed\) return/)
  assert.match(chat, /<img v-else :src="f\.src"/)
})

test('导入数量不设上限：前端不再有批次拦截，文案也一并清掉', () => {
  assert.doesNotMatch(picker, /MAX_IMPORT_BATCH/)
  assert.doesNotMatch(picker, /emoji\.tooMany/)
})

test('拖拽排序仍然写回后端顺序', () => {
  assert.match(picker, /function onDrop/)
  assert.match(picker, /reorderEmojis\(/)
  assert.match(picker, /draggable="true"/)
})

/* ---------------- 聊天窗口接入 ---------------- */

test('点选自定义表情直接发出（不进草稿框）', () => {
  assert.match(chat, /async function sendSticker/)
  assert.match(chat, /await sendEmojiTo\(key, entry\.id\)/)
  assert.match(chat, /@pick-sticker="sendSticker"/)
  // 不能把表情当文本插进输入框
  assert.doesNotMatch(chat, /insertEmoji\(entry/)
})

test('右键菜单有「添加到表情」，且只对可入库的图片/WebM 出现', () => {
  assert.match(chat, /v-if="canAddToEmoji\(ctxMenu\.msg\)"/)
  assert.match(chat, /emoji\.addToEmoji/)
  assert.match(chat, /async function addMsgToEmoji/)
  assert.match(chat, /ipc\.importEmoji\(\[src\]\)/)
})

test('未下载的图片先下载再收纳（等 file-progress 的 done 路径）', () => {
  assert.match(chat, /function waitFileDownload/)
  assert.match(chat, /ipc\.EVT\.fileProgress/)
  assert.match(chat, /p\.done\) finish\(p\.path \|\| ''\)/)
  assert.match(chat, /const wait = waitFileDownload\(key, m\.pkt, f\.id\)/)
  assert.match(chat, /await downloadFile\(m, f\)/)
})

test('收纳前先按文件系统实际情况判断，不信任消息上的 state 标记', () => {
  // 历史导入的附件只有文件名、没有内容；下载完整落盘却记成 failed 的情况也真实存在；
  // 一律先问后端「本地到底有没有这个文件」，否则会把「本地明明有」判成「没下载」
  assert.match(chat, /async function resolveLocalImage/)
  assert.match(chat, /ipc\.emojiSrcAvailable\(candidate \|\| '', name \|\| ''\)/)
  assert.match(chat, /let src = await resolveLocalImage\(attachPath\(f\), f\.name\)/)
  assert.match(chat, /emoji\.srcUnavailable/)
  // 兜底找到的路径要用起来（后端可能返回下载目录里的另一个位置）
  assert.match(chat, /return r\?\.ok && r\.path \? r\.path : ''/)
  // 下载失败的真实原因要透给用户，不能统一糊成「尚未下载」
  assert.match(chat, /throw new Error\(f\.error \|\| t\('emoji\.srcUnavailable'\)\)/)
  // 自己发出的附件不能再去「向对端下载」：找不到本地文件就如实说明
  assert.match(chat, /if \(m\.dir === 'out'\) throw new Error\(t\('emoji\.srcGone'\)\)/)
  const ipcSrc = readFileSync(new URL('../src/lib/ipc.js', import.meta.url), 'utf8')
  assert.match(ipcSrc, /invoke\(WIRE\.cmd\.emoji_src_available, \{ path, name \}\)/)
  const lib = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8')
  assert.match(lib, /emoji::emoji_src_available/)
})

test('下载失败时清掉落盘的半截文件，不留「状态 failed 但文件完整」的矛盾记录', () => {
  const rs = readFileSync(new URL('../src-tauri/src/net.rs', import.meta.url), 'utf8')
  const body = rs.slice(rs.indexOf('fn fail_download('), rs.indexOf('fn fail_download(') + 1400)
  assert.match(body, /let actual = std::fs::metadata\(&p\)/)
  assert.match(body, /if actual != expect \{/)
  assert.match(body, /remove_file\(&p\)/)
  assert.match(body, /f\["path"\] = Value::Null/)
  assert.match(body, /f\["state"\] = "failed"\.into\(\)/)
})

test('表情在气泡里按缩略尺寸渲染：150px 上限 + 去掉白底衬垫', () => {
  const css = cssOf(chat)
  const sticker = declarations(css, '.chat-img.emoji-sticker')
  assert.match(sticker, /max-width\s*:\s*150px/)
  assert.match(sticker, /max-height\s*:\s*150px/)
  assert.match(declarations(css, '.img-wrap.sticker-wrap'), /background\s*:\s*transparent/)
  assert.match(chat, /:class="\{ 'emoji-sticker': isSticker\(f\) \}"/)
})

test('面板定位会把高度夹进窗口内（面板接近半屏高，小窗口不能溢出）', () => {
  // 实测：微信口径的 9 列面板高约 446px，720 高的窗口里若只靠
  // computePopupPosition 的上下翻转，底部行为条会被裁掉、点不到
  assert.match(chat, /function placeEmoji\(panelH\)/)
  assert.match(chat, /const maxTop = Math\.max\(margin, window\.innerHeight - panelH - margin\)/)
  assert.match(chat, /const top2 = Math\.min\(Math\.max\(preferred, margin\), maxTop\)/)
  assert.match(chat, /top: top2 \+ 'px'/)
})

test('表情识别用「库内文件名 + 已发出的缓存副本名」两张集合', () => {
  assert.match(chat, /store\.emojiCacheFiles/)
  assert.match(chat, /store\.emojiFiles/)
  assert.match(chat, /isStickerFile\(f, emojiNames\.value\)/)
})

test('图片与文件卡片都能右键打开菜单（未下载的小图也能收纳）', () => {
  // 图片的右键挂在 <img> 上（div 包裹层），文件卡片挂在卡片本体上
  assert.match(chat, /class="img-wrap"[\s\S]{0,400}@contextmenu\.prevent="openCtx\(v\.m, \$event\)"/)
  assert.match(chat, /class="file-card" @contextmenu\.prevent="openCtx\(v\.m, \$event\)"/)
})

/* ---------------- 协议复用 ---------------- */

test('图片仍走「粘贴图片」协议，WebM 走普通附件，不新增 wire 标记', () => {
  const rs = readFileSync(new URL('../src-tauri/src/emoji.rs', import.meta.url), 'utf8')
  assert.match(rs, /stage_clipboard_image/)
  assert.match(rs, /clip_pos: Some\(0\)/)
  assert.match(rs, /if kind == "webm"/)
  assert.match(rs, /stage_clipboard_file\(data_dir, file, &b64\)/)
  assert.match(rs, /let \(path, opts\) = stage_for_send/)
  // 官方客户端不认识的表情专属属性位不能出现
  assert.doesNotMatch(rs, /FILE_EMOJI|EMOJIOPT/)
})

test('ipc 引用的命令名来自 wire 产物，产物里也没有死条目', () => {
  // 真源那一跳（wire.rs → generate_handler!）由 Rust 侧的
  // every_wire_command_is_registered 守着；这里守「JS 只引产物、不手抄名字」。
  const used = new Set([...ipc.matchAll(/WIRE\.cmd\.([a-z0-9_]+)/g)].map((m) => m[1]))
  assert.ok(used.size >= 40, `ipc 只引了 ${used.size} 个命令，是不是又手抄了？`)

  const declared = new Set(Object.keys(WIRE.cmd))
  for (const name of used) {
    assert.ok(declared.has(name), `WIRE.cmd.${name} 不在产物里（改名后没跑 --dump-wire？）`)
  }
  assert.deepEqual(
    [...declared].filter((n) => !used.has(n)),
    [],
    '产物里有没人引用的命令（要么补封装，要么从 wire.rs 删名字）',
  )
  // 表情相关的命令既要在 ipc 里封装过，也要注册到 invoke_handler
  const lib = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8')
  for (const fn of [
    'list_emojis', 'import_emoji', 'delete_emoji', 'rename_emoji', 'reorder_emojis',
    'send_emoji', 'export_emoji_pack', 'inspect_emoji_pack', 'import_emoji_pack',
  ]) {
    assert.ok(used.has(fn), `ipc 缺少 ${fn} 的封装`)
    assert.ok(declared.has(fn), `wire.rs 里没有 ${fn}`)
    assert.match(lib, new RegExp(`emoji::${fn}`), `${fn} 未注册到 invoke_handler`)
  }
})

/* ---------------- 缓存副本名（连发两条表情的大小问题） ---------------- */

test('归一化把「历史缓存副本名」带进来（否则只有最新那条能缩略）', () => {
  const list = normalizeEmojis([
    {
      id: 'a',
      file: 'a.png',
      cache_file: 'clip_2.png',
      cache_files: ['clip_2.png', 'clip_1.png'],
    },
    { id: 'b', file: 'b.png' },
  ])
  assert.deepEqual(list[0].cacheFiles, ['clip_2.png', 'clip_1.png'], '历史副本名要带上')
  assert.deepEqual(list[1].cacheFiles, [], '没有历史时是空数组，不是 undefined')
})

test('store 把「最近一次 + 历史」并成一张映射表', () => {
  const store = readFileSync(new URL('../src/store.js', import.meta.url), 'utf8')
  assert.match(
    store,
    /flatMap\(\(e\) => \[e\.cacheFile, \.\.\.\(e\.cacheFiles \|\| \[\]\)\]\)/,
    '只取最近一次会导致第二条发出后第一条不再缩略'
  )
})
