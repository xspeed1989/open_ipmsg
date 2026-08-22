// 后端命令统一封装：避免组件里直接散落 invoke 字符串
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

/** 注册后端事件监听（自动解包 Tauri 事件对象的 payload；返回 unlisten 函数） */
export const listenEvent = (event, handler) =>
  listen(event, (e) => handler(e.payload))

/** 获取配置（含本机信息 hostname / ips） */
export const getConfig = () => invoke('get_config')

/** 保存配置，patch: { nickname, group, download_dir, encoding } */
export const saveConfig = (patch) => invoke('save_config', { patch })

/** 在线用户列表 */
export const getUsers = () => invoke('get_users')

/** 重新广播上线（BR_ENTRY） */
export const refreshUsers = () => invoke('refresh_users')

/** 读取某会话历史记录 */
export const getHistory = (key, limit = 300) => invoke('get_history', { key, limit })

/** 发送文本消息，返回落库后的消息记录 */
export const sendText = (key, text) => invoke('send_text', { key, text })

/** 发送附件（多个文件路径），返回消息记录 */
export const sendFiles = (key, paths) => invoke('send_files', { key, paths })

/** 下载对端文件（后台任务，进度走 file-progress 事件）；rid 为对端公告的原始 ID 串 */
export const downloadFile = (key, pktNo, fileId, name, rid) =>
  invoke('download_file', { key, pktNo, fileId, name, rid })

/** 标记入站消息已读，并对要求回执的消息发送 READMSG；返回发出的回执数 */
export const markRead = (key, pkts) => invoke('mark_read', { key, pkts })

/** 事件常量 */
export const EVT = {
  usersUpdated: 'users-updated',
  msgIn: 'msg-in',
  fileProgress: 'file-progress',
  msgRead: 'msg-read',
}
