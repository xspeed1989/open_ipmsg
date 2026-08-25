// 后端命令统一封装：避免组件里直接散落 invoke 字符串
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'

/** 注册后端事件监听（自动解包 Tauri 事件对象的 payload；返回 unlisten 函数） */
export const listenEvent = (event, handler) =>
  listen(event, (e) => handler(e.payload))

/** 获取配置（含本机信息 hostname / ips、加密开关 encrypt 与公钥指纹 key_fp） */
export const getConfig = () => invoke('get_config')

/** 保存配置，patch: { nickname, group, download_dir, encoding, theme, encrypt } */
export const saveConfig = (patch) => invoke('save_config', { patch })

/** 在线用户列表 */
export const getUsers = () => invoke('get_users')

/** 全部历史会话摘要（含离线的，中栏展示用） */
export const listSessions = () => invoke('list_sessions')

/** 重新广播上线（BR_ENTRY） */
export const refreshUsers = () => invoke('refresh_users')

/** 读取某会话历史记录 */
export const getHistory = (key, limit = 300) => invoke('get_history', { key, limit })

/** 发送文本消息，返回落库后的消息记录 */
export const clearHistory = (key) => invoke('clear_history', { key })

/** 全文搜索聊天记录；key 省略则搜索全部会话 */
export const searchHistory = (query, key, limit) =>
  invoke('search_history', { query, key, limit })

export const sendText = (key, text) => invoke('send_text', { key, text })

/** 发送附件（多个文件路径），返回消息记录 */
export const sendFiles = (key, paths) => invoke('send_files', { key, paths })

/** 按未读总数切换托盘图标（有未读时带红点角标） */
export const setUnread = (total) => invoke('set_unread', { total })

/** 读系统剪贴板里的文件列表（Linux 下 webview 拿不到，走原生 GTK 剪贴板） */
export const clipboardFilePaths = () => invoke('clipboard_file_paths')

/** 读系统剪贴板里的位图（截图粘贴用；返回 {mime,size,b64} 或 null） */
export const clipboardImage = () => invoke('clipboard_image')

/** 把粘贴进来的文件内容落盘，返回可发送的本地路径 */
export const stagePastedFile = (name, b64) => invoke('stage_pasted_file', { name, b64 })

/** 在独立窗口里打开一张本地图片（仿微信图片查看器） */
export const openImageViewer = (path, name) =>
  invoke('open_image_viewer', { path, name })

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

/** 事件常量 */
export const EVT = {
  usersUpdated: 'users-updated',
  msgIn: 'msg-in',
  fileProgress: 'file-progress',
  /** 从托盘唤起主窗口：跳到最新的未读会话 */
  openUnread: 'open-unread',
  msgRead: 'msg-read',
}
