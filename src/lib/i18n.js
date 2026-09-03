/**
 * 界面多语言（i18n）：简体中文 + 英语。
 *
 * - locale 是 Vue ref：模板里调用 t() 渲染时会跟踪它，setLocale 之后所有
 *   界面文案自动重渲染。
 * - t(key, vars) 支持 {name} 风格的占位符替换；未知 key 依次回退到英文、
 *   简体中文、原 key 本身（便于发现漏翻）。
 * - 语言偏好存后端 config.json 的 lang 字段：'' 表示「未设置，跟随系统」，
 *   由 detectLocale() 按 navigator.language 探测（仅映射到 zh-CN / en）。
 * - 语言名的展示用「语言自己的名称」：简体中文 / English。
 */

import { ref } from 'vue'

/** 受支持的语言代码（按设置页展示顺序） */
export const SUPPORTED_LANGS = ['zh-CN', 'en']

/** 各语言用自己语言书写的名称 */
export const LANG_NAMES = {
  'zh-CN': '简体中文',
  en: 'English',
}

const zh = {
  // ---- 通用 ----
  'me': '我',
  'unknown': '未知',
  'online': '在线',
  'offline': '离线',
  'unknownUser': '未知用户',
  'ungrouped': '未分组',
  'cancel': '取消',
  'close': '关闭',
  'file.tag': '[文件]',
  'attach.tag': '[附件]',
  'sep.list': '、',
  // ---- 标题栏 ----
  'titlebar.min': '最小化',
  'titlebar.max': '最大化',
  'titlebar.restore': '还原',
  'titlebar.close': '关闭',
  // ---- 左侧栏 ----
  'sidebar.settings': '设置',
  'sidebar.profile': '个人信息与设置',
  // ---- 中栏（联系人列表） ----
  'list.searchPh': '搜索联系人 / 聊天记录',
  'list.clear': '清空',
  'list.contacts': '联系人',
  'list.records': '聊天记录',
  'list.contactsCount': '联系人（{n}）',
  'list.recordsCount': '聊天记录（{n}）',
  'list.recordsSearching': '聊天记录（搜索中…）',
  'list.groupCount': '（{n}）',
  'list.searching': '搜索中…',
  'list.noMatch': '没有匹配的聊天记录',
  'list.empty': '暂无会话',
  'list.emptySub': '请确认对方已运行 IPMsg 客户端（UDP 端口 2425），或点击聊天窗口右上角「刷新」重新广播',
  'list.deleteSession': '删除会话',
  'list.deleteTitle': '删除会话',
  'list.deleteConfirm': '确定删除与「{who}」的会话吗？\n本地聊天记录将被删除且无法恢复（不影响对方）。对方再发消息时会重新出现。',
  'list.deleteOk': '删除',
  // ---- 设置弹窗 ----
  'settings.title': '设置',
  'settings.language': '语言',
  'settings.nickname': '昵称',
  'settings.nicknamePh': '在局域网内显示的名字',
  'settings.group': '群组',
  'settings.groupPh': '如：研发部（可留空）',
  'settings.downloadDir': '接收目录',
  'settings.choose': '选择…',
  'settings.theme': '外观主题',
  'settings.themeSystem': '跟随系统',
  'settings.themeLight': '浅色',
  'settings.themeDark': '深色',
  'settings.encoding': '发送编码',
  'settings.encodingUtf8': 'UTF-8（推荐，客户端间互通）',
  'settings.encodingGbk': 'GBK（兼容老版中文飞鸽）',
  'settings.encrypt': '消息加密',
  'settings.encryptOn': '已开启',
  'settings.encryptOff': '已关闭',
  'settings.encryptHint': '关闭后与所有联系人使用明文通讯',
  'settings.absence': 'Away mode',
  'settings.absenceText': 'Away message (auto-reply & away info)',
  'settings.absenceHint': 'Broadcasts an "away" status; incoming messages get auto-replied with this text, peers may request it.',
  'settings.passwordUse': 'Password feature',
  'settings.password': 'Local password',
  'settings.passwordHint': 'When sending with the password lock, the receiver must enter the same local password to view; agree on the passphrase beforehand.',
  'settings.agentAddr': 'NAT relay agent address (AGENT)',
  'settings.agentHint': 'ip:port (e.g. 10.0.0.5:2425). Messages are relayed through the agent for cross-NAT delivery.',
  'settings.masterAddr': 'Directory master address (DIR_MASTER)',
  'settings.dirMode': 'Directory mode',
  'settings.dirModeOff': 'Off',
  'settings.dirModeUser': 'Member (use master list)',
  'settings.dirModeMaster': 'Master (aggregate the network list)',
  'settings.dirHint': 'Directory service: the master aggregates members per segment; members POLL it periodically.',
  'settings.allowSendList': 'Allow peers to request my host list',
  'settings.ipdict': 'Advertise IPDict capability (v5 format)',
  'settings.v6mcast': 'IPv6 multicast discovery (ff15::979 / ff02::1)',
  'settings.v6mcastHint': 'Disable for pure IPv4. Turn off when diagnosing interop issues with official Windows clients (avoids dual v4/v6 entries in their roster).',

  'settings.absence': '不在模式',
  'settings.absenceText': '不在通知文（自动回复与离开信息）',
  'settings.absenceHint': '开启后广播「离开」状态；收到的消息自动回复此文本，对方可主动索取离开信息。',
  'settings.passwordUse': '密码功能',
  'settings.password': '本机密码',
  'settings.passwordHint': '勾选密码锁发送时，对方必须先输入与本机一致的密码才能查看；双方需提前约定同一口令。',
  'settings.agentAddr': 'NAT 中继代理地址（AGENT 协议）',
  'settings.agentHint': '格式 ip:port（如 10.0.0.5:2425）。配置后发送的消息经代理转发，用于跨 NAT 互达；代理由任意启用了协议的实例承担。',
  'settings.masterAddr': '成员主地址（DIR_MASTER）',
  'settings.dirMode': '成员主模式',
  'settings.dirModeOff': '关闭',
  'settings.dirModeUser': '成员（使用成员主列表）',
  'settings.dirModeMaster': '成员主（汇总全网列表）',
  'settings.dirHint': '成员主目录服务：主节点汇总各段成员，成员定期 POLL 获取全网列表。',
  'settings.allowSendList': '允许对方索取我的主机列表',
  'settings.ipdict': '声明 IPDict 能力（v5 新格式）',
  'settings.v6mcast': 'IPv6 组播成员发现（ff15::979 / ff02::1）',
  'settings.v6mcastHint': '关闭后纯 IPv4。与官方 Windows 客户端混合组网遇到互通异常时可关闭排查（防止对端名单里出现 v4/v6 双条目）。',

  'settings.fpLabel': '本机密钥指纹',
  'settings.fpTitle': '点击复制本机密钥指纹',
  'settings.fpCopied': '已复制',
  'settings.chatRecords': '聊天记录',
  'settings.importBtn': '导入官方 IP Messenger 聊天记录…',
  'settings.importing': '导入中…',
  'settings.importHint': '选择官方 IPMsg（v4.5+）的日志数据库 ipmsg.db，可多选；按对方 IP 归入对应会话，附件仅记录文件名。可重复执行，不会产生重复记录。',
  'settings.selfInfo': '本机信息',
  'settings.hostname': '主机名',
  'settings.ip': 'IP 地址',
  'settings.port': '协议端口',
  'settings.note': '修改昵称/群组后会自动重新向局域网广播上线。',
  'settings.save': '保存',
  'settings.alertNickname': '请填写昵称',
  'settings.alertSaveFailed': '保存失败：{e}',
  'settings.pickDirTitle': '选择接收文件的保存目录',
  'settings.pickDbTitle': '选择官方 IP Messenger 的日志数据库',
  'settings.dbFilter': 'IPMsg 日志库（ipmsg.db）',
  'settings.alertImportFailed': '导入失败：{e}',
  'settings.importLineOk': '✔ {name}：导入 {n} 条',
  'settings.importDone': '导入完成：共 {total} 条消息',
  'settings.importSkipped': '（跳过 {n} 条已存在/备忘录）',
  'settings.importNewSessions': '，新增 {n} 个会话',
  'settings.importMerged': '，按名称+主机归并 {n} 个重复会话',
  'settings.importNoNew': '\n没有新消息可导入。',
  // ---- 接收人选择弹窗 ----
  'picker.defaultTitle': '选择接收人',
  'picker.empty': '没有可选的会话',
  'picker.cancelEsc': '取消（Esc）',
  'picker.send': '发送（{n}）',
  // ---- 聊天窗口 ----
  'chat.clearHistory': '清空聊天记录',
  'chat.refreshUsers': '重新广播上线，刷新在线用户',
  'chat.copy': '复制',
  'chat.reply': '回复',
  'chat.forward': '转发',
  'chat.recall': '撤回',
  'chat.recalledTip': '消息已撤回',
  'chat.secretSend': '封书（需对方点开查看）',
  'chat.secretSealed': '封书：对方发来的保密消息，点击打开',
  'chat.pwdLock': '密码锁（对方需输入本机密码查看）',
  'chat.pwdLocked': '密码保护的消息',
  'chat.pwdPrompt': '请输入本机设置的密码以查看此消息：',
  'chat.unlock': '打开（开封）',
  'chat.unlockedOk': '已开封',
  'chat.unlockFail': '无法打开：{e}',
  'chat.broadcast': '广播群发（全网络同报）',
  'chat.broadcastSent': '已广播',
  'chat.multicast': '多选群发',
  'chat.multicastTo': '选择群发对象',
  'chat.leave': '离开',
  'chat.multiSelect': '多选',
  'chat.selected': '已选 {n} 条',
  'chat.mergeForward': '合并转发',
  'chat.batchSend': '批量发送',
  'chat.sendFiles': '发送文件',
  'chat.sendFolder': '发送文件夹',
  'chat.screenshotSoon': '截图功能开发中',
  'chat.emoji': '表情',
  'chat.send': '发送',
  'chat.inputPh': '输入消息…',
  'chat.enterHint': 'Enter 发送 / Ctrl+Enter 换行 / 可直接粘贴图片或文件',
  'chat.findPh': '在本会话中查找',
  'chat.findPrev': '上一个',
  'chat.findNext': '下一个（Enter）',
  'chat.findClose': '关闭（Esc）',
  'chat.open': '打开',
  'chat.download': '下载',
  'chat.retry': '重试',
  'chat.revealDir': '所在文件夹',
  'chat.saved': '已保存',
  'chat.folderSaved': '文件夹已保存',
  'chat.downloadEnc': '下载中（加密流）',
  'chat.decrypted': '解密完成',
  'chat.sent': '已发送',
  'chat.failed': '失败',
  'chat.read': '已读',
  'chat.unread': '未读',
  'chat.histImport': '历史附件 · 仅文件名',
  'chat.delayedNote': '离线留言 · 原发送时间 {t}',
  'chat.delayedTitle': '对方在 {t} 发出，你当时不在线，上线后才补投',
  'chat.queuedNote': '离线留言 · 对方上线后自动投递',
  'chat.placeholderTitle': '选择一个会话，开始聊天',
  'chat.placeholderSub': '局域网内基于 IPMsg 协议（UDP/TCP 2425）',
  'chat.replyTo': '回复 {nick}：',
  'chat.cancelReply': '取消回复（Esc）',
  'chat.removePending': '从待发送列表移除',
  'chat.pendingEnter': 'Enter 发送',
  'chat.dropOpen': '松开后打开「{name}」的会话并加入待发送',
  'chat.dropAdd': '松开后加入待发送列表',
  'chat.dropChoose': '请先选择会话，或把文件拖到中栏的联系人上',
  'chat.dropSupport': '支持多个文件与文件夹 · Enter 发送',
  'chat.forwardTo': '转发给',
  'chat.batchTo': '批量发送给',
  'chat.peer': '对方',
  'chat.sigWarn': '签名校验失败',
  'chat.viewImg': '点击在新窗口查看原图',
  'chat.clipboardImage': '剪贴板图片',
  'chat.pasteFile': '粘贴文件',
  'chat.alertSelectSession': '请先在左侧选择一个会话',
  'chat.alertFillContent': '请先在输入框填写要批量发送的内容（文字或粘贴的图片/文件）',
  'chat.alertCopyFailed': '复制失败：{e}',
  'chat.alertNoCopy': '这条消息没有可复制的内容',
  'chat.alertSendFailed': '发送失败：{e}',
  'chat.alertNoForward': '选中的消息没有可转发的内容',
  'chat.alertSentTo': '已发送给 {n} 人：{names}',
  'chat.alertPartial': '成功 {ok} 人；失败 {fail} 人：{names}',
  'chat.alertStageFailed': '准备附件失败：{e}',
  'chat.alertImgTooBig': '图片超过 32MB，请改用「发送文件」',
  'chat.alertPasteFailed': '粘贴失败：{e}',
  'chat.alertPickContact': '请先选择要发送给谁，或把文件拖到中栏的联系人上',
  'chat.alertOpenImg': '打开图片失败：{e}',
  'chat.alertOpen': '打开失败：{e}',
  'chat.alertReveal': '打开文件夹失败：{e}',
  'chat.alertClear': '清空失败：{e}',
  'chat.clearConfirm': '确定清空与「{who}」的聊天记录吗？\n本地记录将被删除且无法恢复（不影响对方）。',
  'chat.clearTitle': '清空聊天记录',
  'chat.clearOk': '清空',
  'chat.pickFilesTitle': '选择要发送的文件',
  'chat.pickFolderTitle': '选择要发送的文件夹',
  // ---- 图片查看器 ----
  'viewer.image': '图片',
  'viewer.missingPath': '缺少图片路径',
  'viewer.failed': '图片打开失败：{e}',
  'viewer.loading': '正在加载…',
  'viewer.zoomIn': '放大 ( + )',
  'viewer.zoomOut': '缩小 ( - )',
  'viewer.fitTitle': '适应窗口 ( 0 )',
  'viewer.actualSize': '原始大小',
  'viewer.fit': '适应',
  'viewer.oneToOne': '1:1',
  'viewer.rotate': '旋转 90° ( R )',
  'viewer.reveal': '所在文件夹',
  'viewer.close': '关闭 ( Esc )',
  'viewer.saveAs': '另存为…',
  'viewer.saved': '已保存',
  'viewer.saveFailed': '保存失败：{e}',
  // ---- 转发/引用（发送文本里的本地标记） ----
  'forward.noContent': '没有可转发的内容',
  'forward.notDownloaded': '「{name}」尚未下载，无法转发',
  'forward.fileName': '文件',
  'merge.qOpen': '【',
  'merge.qClose': '】',
  'reply.qOpen': '「',
  'reply.qClose': '」',
  // ---- 通知预览 / 日期 ----
  'preview.etc': ' 等{n}个',
  'preview.offline': '[离线留言]',
}

const en = {
  // ---- 通用 ----
  'me': 'Me',
  'unknown': 'Unknown',
  'online': 'Online',
  'offline': 'Offline',
  'unknownUser': 'Unknown user',
  'ungrouped': 'Ungrouped',
  'cancel': 'Cancel',
  'close': 'Close',
  'file.tag': '[File]',
  'attach.tag': '[Attachment]',
  'sep.list': ', ',
  // ---- 标题栏 ----
  'titlebar.min': 'Minimize',
  'titlebar.max': 'Maximize',
  'titlebar.restore': 'Restore',
  'titlebar.close': 'Close',
  // ---- 左侧栏 ----
  'sidebar.settings': 'Settings',
  'sidebar.profile': 'Profile & settings',
  // ---- 中栏（联系人列表） ----
  'list.searchPh': 'Search contacts / chat history',
  'list.clear': 'Clear',
  'list.contacts': 'Contacts',
  'list.records': 'Chat history',
  'list.contactsCount': 'Contacts ({n})',
  'list.recordsCount': 'Chat history ({n})',
  'list.recordsSearching': 'Chat history (searching…)',
  'list.groupCount': ' ({n})',
  'list.searching': 'Searching…',
  'list.noMatch': 'No matching chat history',
  'list.empty': 'No conversations',
  'list.emptySub': 'Make sure the other side is running an IPMsg client (UDP port 2425), or click "Refresh" at the top right of the chat window to re-announce',
  'list.deleteSession': 'Delete conversation',
  'list.deleteTitle': 'Delete conversation',
  'list.deleteConfirm': 'Delete the conversation with "{who}"?\nLocal chat history will be deleted and cannot be recovered (does not affect the other side). It will reappear when they message you again.',
  'list.deleteOk': 'Delete',
  // ---- 设置弹窗 ----
  'settings.title': 'Settings',
  'settings.language': 'Language',
  'settings.nickname': 'Nickname',
  'settings.nicknamePh': 'Name shown on the LAN',
  'settings.group': 'Group',
  'settings.groupPh': 'e.g. R&D (optional)',
  'settings.downloadDir': 'Save to',
  'settings.choose': 'Choose…',
  'settings.theme': 'Theme',
  'settings.themeSystem': 'System',
  'settings.themeLight': 'Light',
  'settings.themeDark': 'Dark',
  'settings.encoding': 'Encoding',
  'settings.encodingUtf8': 'UTF-8 (recommended, interoperable)',
  'settings.encodingGbk': 'GBK (legacy Chinese IPMsg)',
  'settings.encrypt': 'Message encryption',
  'settings.encryptOn': 'On',
  'settings.encryptOff': 'Off',
  'settings.encryptHint': 'All chats use plaintext when this is off',
  'settings.fpLabel': 'Local key fingerprint',
  'settings.fpTitle': 'Click to copy the local key fingerprint',
  'settings.fpCopied': 'Copied',
  'settings.chatRecords': 'Chat history',
  'settings.importBtn': 'Import official IP Messenger history…',
  'settings.importing': 'Importing…',
  'settings.importHint': 'Pick the official IPMsg (v4.5+) log database ipmsg.db (multi-select allowed). Chats are grouped by peer IP; attachments are recorded by name only. Safe to run repeatedly.',
  'settings.selfInfo': 'Local info',
  'settings.hostname': 'Hostname',
  'settings.ip': 'IP address',
  'settings.port': 'Protocol port',
  'settings.note': 'Changing the nickname/group re-announces your presence on the LAN.',
  'settings.save': 'Save',
  'settings.alertNickname': 'Please enter a nickname',
  'settings.alertSaveFailed': 'Failed to save: {e}',
  'settings.pickDirTitle': 'Choose the folder for received files',
  'settings.pickDbTitle': 'Select the official IP Messenger log database',
  'settings.dbFilter': 'IPMsg log database (ipmsg.db)',
  'settings.alertImportFailed': 'Import failed: {e}',
  'settings.importLineOk': '✔ {name}: imported {n}',
  'settings.importDone': 'Import complete: {total} messages',
  'settings.importSkipped': ' ({n} existing/memo skipped)',
  'settings.importNewSessions': ', {n} new conversations',
  'settings.importMerged': ', merged {n} duplicate conversations by name+host',
  'settings.importNoNew': '\nNo new messages to import.',
  // ---- 接收人选择弹窗 ----
  'picker.defaultTitle': 'Select recipients',
  'picker.empty': 'No sessions available',
  'picker.cancelEsc': 'Cancel (Esc)',
  'picker.send': 'Send ({n})',
  // ---- 聊天窗口 ----
  'chat.clearHistory': 'Clear chat history',
  'chat.refreshUsers': 'Re-announce presence and refresh users',
  'chat.copy': 'Copy',
  'chat.reply': 'Reply',
  'chat.forward': 'Forward',
  'chat.recall': 'Recall',
  'chat.recalledTip': 'Message recalled',
  'chat.secretSend': 'Sealed message (receiver must open)',
  'chat.secretSealed': 'Sealed message from peer, click to open',
  'chat.pwdLock': 'Password lock (receiver needs local password)',
  'chat.pwdLocked': 'Password-protected message',
  'chat.pwdPrompt': 'Enter the local password to view this message:',
  'chat.unlock': 'Open (unseal)',
  'chat.unlockedOk': 'Unsealed',
  'chat.unlockFail': 'Cannot open: {e}',
  'chat.broadcast': 'Broadcast to everyone',
  'chat.broadcastSent': 'Broadcast sent',
  'chat.multicast': 'Send to multiple',
  'chat.multicastTo': 'Choose recipients',
  'chat.leave': 'Away',
  'chat.multiSelect': 'Select',
  'chat.selected': '{n} selected',
  'chat.mergeForward': 'Merge & forward',
  'chat.batchSend': 'Batch send',
  'chat.sendFiles': 'Send file',
  'chat.sendFolder': 'Send folder',
  'chat.screenshotSoon': 'Screenshot (coming soon)',
  'chat.emoji': 'Emoji',
  'chat.send': 'Send',
  'chat.inputPh': 'Type a message…',
  'chat.enterHint': 'Enter to send / Ctrl+Enter for newline / paste images or files',
  'chat.findPh': 'Find in this chat',
  'chat.findPrev': 'Previous',
  'chat.findNext': 'Next (Enter)',
  'chat.findClose': 'Close (Esc)',
  'chat.open': 'Open',
  'chat.download': 'Download',
  'chat.retry': 'Retry',
  'chat.revealDir': 'Show in folder',
  'chat.saved': 'Saved',
  'chat.folderSaved': 'Folder saved',
  'chat.downloadEnc': 'Downloading (encrypted)',
  'chat.decrypted': 'Decrypted',
  'chat.sent': 'Sent',
  'chat.failed': 'Failed',
  'chat.read': 'Read',
  'chat.unread': 'Unread',
  'chat.histImport': 'Historical attachment · name only',
  'chat.delayedNote': 'Offline message · originally sent at {t}',
  'chat.delayedTitle': 'Sent by the other party at {t}; you were offline, delivered after you came online',
  'chat.queuedNote': 'Offline message · delivered when the recipient comes online',
  'chat.placeholderTitle': 'Select a conversation to start chatting',
  'chat.placeholderSub': 'LAN chat over the IPMsg protocol (UDP/TCP 2425)',
  'chat.replyTo': 'Reply to {nick}: ',
  'chat.cancelReply': 'Cancel reply (Esc)',
  'chat.removePending': 'Remove from pending list',
  'chat.pendingEnter': 'Enter to send',
  'chat.dropOpen': 'Release to open {name}\'s chat and queue the files',
  'chat.dropAdd': 'Release to add to the pending list',
  'chat.dropChoose': 'Select a conversation first, or drag files onto a contact in the middle column',
  'chat.dropSupport': 'Multiple files & folders · Enter to send',
  'chat.forwardTo': 'Forward to',
  'chat.batchTo': 'Batch send to',
  'chat.peer': 'the other party',
  'chat.sigWarn': 'Signature verification failed',
  'chat.viewImg': 'Click to view the original image in a new window',
  'chat.clipboardImage': 'Clipboard image',
  'chat.pasteFile': 'Pasted file',
  'chat.alertSelectSession': 'Select a conversation on the left first',
  'chat.alertFillContent': 'Type the content to batch-send first (text or pasted images/files)',
  'chat.alertCopyFailed': 'Copy failed: {e}',
  'chat.alertNoCopy': 'This message has no copyable content',
  'chat.alertSendFailed': 'Send failed: {e}',
  'chat.alertNoForward': 'The selected messages have nothing to forward',
  'chat.alertSentTo': 'Sent to {n} people: {names}',
  'chat.alertPartial': '{ok} succeeded; {fail} failed: {names}',
  'chat.alertStageFailed': 'Failed to prepare attachments: {e}',
  'chat.alertImgTooBig': 'Image exceeds 32MB — use "Send file" instead',
  'chat.alertPasteFailed': 'Paste failed: {e}',
  'chat.alertPickContact': 'Choose a recipient first, or drag the files onto a contact in the middle column',
  'chat.alertOpenImg': 'Failed to open image: {e}',
  'chat.alertOpen': 'Failed to open: {e}',
  'chat.alertReveal': 'Failed to open folder: {e}',
  'chat.alertClear': 'Failed to clear: {e}',
  'chat.clearConfirm': 'Clear all chat history with "{who}"?\nLocal records will be deleted and cannot be recovered (the other side is unaffected).',
  'chat.clearTitle': 'Clear chat history',
  'chat.clearOk': 'Clear',
  'chat.pickFilesTitle': 'Choose files to send',
  'chat.pickFolderTitle': 'Choose folders to send',
  // ---- 图片查看器 ----
  'viewer.image': 'Image',
  'viewer.missingPath': 'Missing image path',
  'viewer.failed': 'Failed to open image: {e}',
  'viewer.loading': 'Loading…',
  'viewer.zoomIn': 'Zoom in ( + )',
  'viewer.zoomOut': 'Zoom out ( - )',
  'viewer.fitTitle': 'Fit window ( 0 )',
  'viewer.actualSize': 'Actual size',
  'viewer.fit': 'Fit',
  'viewer.oneToOne': '1:1',
  'viewer.rotate': 'Rotate 90° ( R )',
  'viewer.reveal': 'Show in folder',
  'viewer.close': 'Close ( Esc )',
  'viewer.saveAs': 'Save As…',
  'viewer.saved': 'Saved',
  'viewer.saveFailed': 'Failed to save: {e}',
  // ---- 转发/引用（发送文本里的本地标记） ----
  'forward.noContent': 'Nothing to forward',
  'forward.notDownloaded': '"{name}" hasn\'t been downloaded yet and can\'t be forwarded',
  'forward.fileName': 'File',
  'merge.qOpen': '[',
  'merge.qClose': ']',
  'reply.qOpen': '"',
  'reply.qClose': '"',
  // ---- 通知预览 / 日期 ----
  'preview.etc': ' and {n} more',
  'preview.offline': '[Offline message]',
}

const MESSAGES = { 'zh-CN': zh, en }

/** 当前语言（reactive）：模板里 t() 依赖它，切换后自动重渲染 */
export const locale = ref('zh-CN')

/** 支持的语言判断 */
export function isSupported(lang) {
  return typeof lang === 'string' && SUPPORTED_LANGS.includes(lang)
}

/** 按系统语言探测：只负责映射到受支持的两种 */
export function detectLocale() {
  const nav = typeof navigator !== 'undefined' ? navigator.language || '' : ''
  return nav.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en'
}

/** 应用语言：设置 locale + <html lang>；非法值回退到系统探测 */
export function setLocale(lang) {
  const l = isSupported(lang) ? lang : detectLocale()
  locale.value = l
  if (typeof document !== 'undefined') {
    document.documentElement.lang = l
  }
  return l
}

/** 取当前语言文案；vars 做 {name} 占位符替换 */
export function t(key, vars) {
  const dict = MESSAGES[locale.value] || zh
  let s = dict[key] ?? zh[key] ?? key
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v ?? ''))
    }
  }
  return s
}

/* ---------------- 日期与星期（消息流日期分隔标签用） ---------------- */

const WEEKDAYS = {
  'zh-CN': ['周日', '周一', '周二', '周三', '周四', '周五', '周六'],
  en: ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'],
}
const MONTHS = {
  'zh-CN': ['1月', '2月', '3月', '4月', '5月', '6月', '7月', '8月', '9月', '10月', '11月', '12月'],
  en: ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'],
}

const DAY_MS = 86400000

/**
 * 消息流中的日期分隔标签（按语言）。
 * 今天/昨天用相对词；一周内显示星期；更早显示月日（跨年带年份）。
 * @param {number} ts 秒级时间戳
 * @param {string} [lng] 语言，缺省用当前 locale
 */
export function dayLabel(ts, lng = locale.value) {
  if (!ts) return ''
  const l = isSupported(lng) ? lng : 'zh-CN'
  const d = new Date(ts * 1000)
  const now = new Date()
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000
  if (ts >= midnight) return l === 'en' ? 'Today' : '今天'
  if (ts >= midnight - DAY_MS) return l === 'en' ? 'Yesterday' : '昨天'
  const wd = WEEKDAYS[l][d.getDay()]
  if (ts >= midnight - 6 * DAY_MS) return wd
  const sameYear = d.getFullYear() === now.getFullYear()
  if (l === 'en') {
    const md = `${MONTHS.en[d.getMonth()]} ${d.getDate()}`
    return sameYear ? md : `${md}, ${d.getFullYear()}`
  }
  const zhM = `${d.getMonth() + 1}月${d.getDate()}日`
  return sameYear ? zhM : `${d.getFullYear()}年${zhM}`
}