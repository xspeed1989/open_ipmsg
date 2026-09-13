// 后端命令统一封装：避免组件里直接散落 invoke 字符串
import { invoke } from '@tauri-apps/api/core'
import { listen, emit } from '@tauri-apps/api/event'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'

/** 注册后端事件监听（自动解包 Tauri 事件对象的 payload；返回 unlisten 函数） */
export const listenEvent = (event, handler) =>
  listen(event, (e) => handler(e.payload))

/** 从独立窗口（遮罩/查看器）向所有窗口广播事件 */
export const emitToMain = (event, payload) => emit(event, payload)

/** 获取配置（含本机信息 hostname / ips、加密开关 encrypt 与公钥指纹 key_fp） */
export const getConfig = () => invoke('get_config')

/** 保存配置，patch: { nickname, group, download_dir, encoding, theme, lang, encrypt } */
export const saveConfig = (patch) => invoke('save_config', { patch })

/** 在线用户列表 */
export const getUsers = () => invoke('get_users')

/** 全部历史会话摘要（含离线的，中栏展示用） */
export const listSessions = () => invoke('list_sessions')

/** 重新广播上线（BR_ENTRY） */
export const refreshUsers = () => invoke('refresh_users')

/** 不在模式开关（BR_ABSENCE + ABSENCEOPT；text 为空保留现值） */
export const setAbsence = (on, text) => invoke('set_absence', { on, text })

/** 撤回我方发出的某条文本消息（DELMSG） */
export const recallMessage = (key, pkt) => invoke('recall_message', { key, pkt })

/** 广播群发（BROADCASTOPT 同报） */
export const broadcastMessage = (text) => invoke('broadcast_message', { text })

/** 封书/密码锁开封（密码锁场景校验 password） */
export const unlockMessage = (key, pkt, password = null) =>
  invoke('unlock_message', { key, pkt, password })

/** 主动索取对端不在通知文（GETABSENCEINFO） */
export const getAbsenceInfo = (key) => invoke('get_absence_info', { key })

/** 主动发起主机列表交换（BR_ISGETLIST） */
export const requestHostlist = () => invoke('request_hostlist')

/** 读取某会话历史记录 */
export const getHistory = (key, limit = 300) => invoke('get_history', { key, limit })

/** 发送文本消息，返回落库后的消息记录 */
export const clearHistory = (key) => invoke('clear_history', { key })

/** 删除某会话（微信式）：清掉本地记录并把联系人从列表隐藏；
 *  对端再发消息时后端自动恢复。返回被删掉的记录条数 */
export const deleteContact = (key) => invoke('delete_contact', { key })

/** 全文搜索聊天记录；key 省略则搜索全部会话 */
export const searchHistory = (query, key, limit) =>
  invoke('search_history', { query, key, limit })

export const sendText = (key, text, secret = false, password = false) =>
  invoke('send_text', { key, text, secret, password })

/** 发送附件（多个文件路径）+ 可选正文，一条消息同时携带；返回消息记录 */
export const sendFiles = (key, paths, text = '', secret = false, password = false) =>
  invoke('send_files', { key, paths, text, secret, password })

/** 按未读总数切换托盘图标（有未读时带红点角标） */
export const setUnread = (total) => invoke('set_unread', { total })

/** Linux 原生可点击通知：点击通知（正文/「打开」按钮）→ open-chat 事件 →
 *  弹出主窗口并切到对应会话。Windows/macOS 仍走插件 sendNotification */
export const notifyMessage = (key, title, body, lang) =>
  invoke('notify_message', { key, title, body, lang })

/** 读系统剪贴板里的文件列表（Linux 下 webview 拿不到，走原生 GTK 剪贴板） */
export const clipboardFilePaths = () => invoke('clipboard_file_paths')

/** 读系统剪贴板里的位图（截图粘贴用；返回 {mime,size,b64} 或 null） */
export const clipboardImage = () => invoke('clipboard_image')

/** 把粘贴进来的文件内容落盘，返回可发送的本地路径 */
export const stagePastedFile = (name, b64) => invoke('stage_pasted_file', { name, b64 })

/** 在独立窗口里打开一张本地图片（仿微信图片查看器） */
export const openImageViewer = (path, name) =>
  invoke('open_image_viewer', { path, name })

/** 图片查看器「另存为」：把本地图片复制到用户选择的目标路径 */
export const copyFileAs = (source, dest) => invoke('copy_file_as', { source, dest })

/** 发送剪贴板图片（base64 原始数据，后端落盘后按附件公告） */
export const sendClipboardImage = (key, text, b64, mime) =>
  invoke('send_clipboard_image', { key, text, b64, mime })

/** 下载对端文件（后台任务，进度走 file-progress 事件）；rid 为对端公告的原始 ID 串 */
export const downloadFile = (key, pktNo, fileId, name, rid, size, isDir) =>
  invoke('download_file', { key, pktNo, fileId, name, rid, size, isDir })

/** 标记入站消息已读，并对要求回执的消息发送 READMSG；返回发出的回执数 */
export const markRead = (key, pkts) => invoke('mark_read', { key, pkts })

/** 本地标记出站消息已被对端阅读（不发包）；返回实际翻转的条数 */
export const markOutRead = (key, pkts) => invoke('mark_out_read', { key, pkts })

/** 从官方 IP Messenger 日志库（v4.5+ 的 ipmsg.db，SQLite）导入聊天记录；
 *  paths 为所选文件路径数组，返回 { total, skipped, sessionsNew, files, failed } */
export const importIpmsgLogs = (paths) => invoke('import_ipmsg_log', { paths })

/** 写系统剪贴板（右键复制消息内容）。
 *  WebKitGTK 的网页 navigator.clipboard 不可靠，走原生插件保证三平台一致 */
export const copyText = (text) => writeText(text)

/* ---------------- 自定义表情包 ---------------- */

/** 列出自定义表情（后端会剔除图片已丢失的失效项） */
export const listEmojis = () => invoke('list_emojis')

/** 本地是否已有可用图片（历史导入的附件只有文件名；下载失败的记录也可能与磁盘不符）。
 *  返回 { ok, size, path }：path 可能是后端按文件名在下载目录里兜底找到的位置 */
export const emojiSrcAvailable = (path, name = '') =>
  invoke('emoji_src_available', { path, name })

/** 读取本地图片为 base64（表情缩略图/聊天内联预览用，超 32MB 拒绝） */
export const readImageData = (path) => invoke('read_image_data', { path })

/** 从本地图片导入表情（只传路径，读盘与校验都在后端） */
export const importEmoji = (paths) => invoke('import_emoji', { paths })

/** 删除表情（文件 + 索引） */
export const deleteEmoji = (ids) => invoke('delete_emoji', { ids })

/** 重命名表情 */
export const renameEmoji = (id, name) => invoke('rename_emoji', { id, name })

/** 按给定 id 顺序重排表情（拖拽排序） */
export const reorderEmojis = (ids) => invoke('reorder_emojis', { ids })

/** 发送自定义表情（按官方「粘贴图片」协议公告，对端内嵌显示） */
export const sendEmoji = (key, id, text = '') => invoke('send_emoji', { key, id, text })

/** 导出表情包（.ipmojis，标准 zip）；ids 为空 = 全部 */
export const exportEmojiPack = (ids, dest, name) =>
  invoke('export_emoji_pack', { ids, dest, name })

/** 预览表情包内容（不解压落盘），供导入前勾选 */
export const inspectEmojiPack = (path) => invoke('inspect_emoji_pack', { path })

/** 导入表情包；files 为空 = 全部可导入项 */
export const importEmojiPack = (path, files = []) =>
  invoke('import_emoji_pack', { path, files })

/** 选择本地图片（多选；用于导入表情）。标题交给系统默认，避免在这里引入 i18n 依赖 */
export const pickImageFiles = () =>
  openDialog({
    multiple: true,
    directory: false,
    filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'] }],
  })

/** 选择表情包文件（.ipmojis / .zip） */
export const pickEmojiPack = () =>
  openDialog({
    multiple: false,
    directory: false,
    filters: [{ name: 'Sticker pack', extensions: ['ipmojis', 'zip'] }],
  })

/** 选择表情包导出目标路径；取消返回 null */
export const pickEmojiPackSavePath = (defaultPath) =>
  saveDialog({ defaultPath, filters: [{ name: 'Sticker pack', extensions: ['ipmojis'] }] })

/* ---------------- 截图 ---------------- */

/** 开始截图：后端抓屏并打开遮罩窗口，返回 { session, width, height, monitors } */
export const startScreenshot = () => invoke('start_screenshot')

/** 遮罩窗口取图：返回 { b64, mime, slice, scale, total } */
export const shotImage = (session, index) => invoke('shot_image', { session, index })

/** 关闭全部遮罩窗口并释放会话缓存（幂等） */
export const closeShotOverlays = (session) => invoke('close_shot_overlays', { session })

/** 把确认后的 PNG 另存为文件 */
export const saveShotPng = (b64, path) => invoke('save_shot_png', { b64, path })

/** 把 PNG 写进系统剪贴板（Linux 后端 GTK；其他平台返回 PLUGIN 由前端插件兜底） */
export const copyShotImage = (b64) => invoke('copy_image_to_clipboard', { b64 })

/** 事件常量 */
export const EVT = {
  usersUpdated: 'users-updated',
  msgIn: 'msg-in',
  fileProgress: 'file-progress',
  /** 从托盘唤起主窗口：跳到最新的未读会话 */
  openUnread: 'open-unread',
  /** 点击系统通知（Linux 原生）：弹出主窗口并切到对应会话 */
  openChat: 'open-chat',
  msgRead: 'msg-read',
  /** 对端撤回消息（DELMSG） */
  msgRecalled: 'msg-recalled',
  /** 封书/密码锁开封 */
  msgUnlocked: 'msg-unlocked',
  /** 对端不在通知文（GETABSENCEINFO 应答） */
  absenceInfo: 'absence-info',
  /** 截图确认：遮罩窗口 → 主窗口，进入待发送列表 */
  screenshotDone: 'screenshot-done',
  /** 截图「复制」按钮：只写剪贴板、不进待发送列表 */
  screenshotCopy: 'screenshot-copy',
}
