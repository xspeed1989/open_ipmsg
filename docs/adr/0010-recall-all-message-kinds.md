# ADR-0010：撤回要能撤回附件，也要说清撤不回什么

状态：已落地（v0.1.19 开发分支）。

## 问题

原来只有文本消息能撤回：右键菜单的撤回项写死 `kind === 'text'`，后端 `net::recall_message`
也硬拒非文本——`E_RECALL_HAS_FILES|附件消息不支持撤回（对方可能已开始下载）`。理由是当时的
实现只发一条 `DELMSG`，而附件消息除了气泡以外还有一个**真的可以被取走**的东西：本机
`AppState.transfer` 里按包号登记的附件槽（`offer_slot` / `lookup_slot`）。只把气泡改成
「已撤回」、槽还留着，等于骗人：对方点一下附件卡片就能把文件下下来。

所以「支持附件撤回」不是放宽一个 `if`，而是要让撤回对附件也成立，同时别在界面上承诺
协议给不了的东西。

## 决定

**1. 撤回范围：我方发出的、未撤回的、非广播的任何消息。**
文本、文件、图片、剪贴板贴图、表情、文件夹在落库时只有两种 `kind`：`"text"` 与 `"file"`
（表情/贴图/图片/文件夹都是带附件的 `"file"`，前端不用区分）。因此后端只按 `kind == "file"`
决定要不要做附件那一套额外动作，前端菜单则统一用 `src/lib/recall.js` 的 `canRecall()`：
`dir === 'out' && recalled !== true && !broadcast`。

**2. 附件撤回的顺序：拆槽 → 通知对方丢清单 → 再通知撤回。**
`src-tauri/src/net.rs` 的 `recall_message`：

1. 先 `outbox.ack_retry` + `ack_pending` 把这条从「待送达确认 / 离线待投递」队列里摘掉。
   这一步不能省：`retry_pending_for` / `flush_pending_for` 重发时会重新 `offer_slot`，
   几秒之后附件槽就被登记回来了，撤回变成「过一会儿又能下载」。
2. `transfer.release_packets(&[pkt])` 撤销该包号下的**全部**槽（一条消息可能多附件，
   文件夹也只是一个槽）。此后对端 `GETFILEDATA` 在 `lookup_slot` 处未命中，连接直接关闭、
   零字节。
3. 对端在线时先发 `RELEASEFILES(0x61)`，extra 就写包号（收端 `id_candidates` 十进制/十六进制
   都试，不新造进制约定）。协议里这条原本是收端「我放弃接收，把槽收回去」的意思，这里反向
   使用：告诉对端「这几个槽不再有数据」，让它排队中的取件就此作废、不必等连接被关掉才
   发现取不到。我方收到这类报文也只是按包号释放同名槽（`recall` 之外没有别的副作用）。
4. 最后发 `DELMSG(0x31)`，extra 同上。
5. 无论对端在线与否，本地 `recalled = true` 并发 `msg-recalled`。

顺序反过来（先 DELMSG）会给对方留一个时间窗：收到撤回之前抢先把文件取走。单测
`recall_file_message_releases_slot_then_notifies_peer` 直接断言报文顺序与「撤回后再取返回零字节」。

**3. 广播信箱不支持撤回，后端给专用错误码。**
广播一次发给多人（`BROADCASTOPT` 同报 + 逐个在线成员），`DELMSG` 只能对某个会话发，广播
地址上也没有会话可回执。与其「看起来撤回成功、其实一个人都没撤回」，不如直接拒绝：
`E_RECALL_BROADCAST|广播消息不支持撤回（一次发给多人，无法逐一通知）`。前端在广播会话里
干脆不显示菜单项（`canRecall(msg, { broadcast: isBroadcast })`），错误码只兜住并发/旧界面。

**4. 界面不承诺撤不回的事。**
已经下载到对端的文件收不回来；对端复制/转存/截图的内容也收不回来。`DELMSG` 在飞秋语义里
是「封书破弃」，本质是一个**告知**，官方客户端可以忽略。因此：

- 撤回后附件卡片、图片内联预览一律不再渲染（前端已撤回分支在 `msg-row` 之前，见
  `ChatWindow.vue` 的 `v-else-if="v.m.recalled"`），而不是渲染成「失效附件」诱导点击；
- 「重新编辑」只对文本出现（附件没有可恢复的草稿）。判据从模板里的
  `v-if="v.m.dir === 'out'"` 改成 `canReedit(v.m)`——撤回范围放宽后，附件消息撤回也会
  落到这一支，按方向判断会留下一个点了没反应的死按钮；
- 文案不写「已删除」「已销毁」之类做不到的词。

## 后果

- 撤回一个附件消息 = 撤销槽 + 两条报文。对端在线时它此后取不到任何字节；对端离线时本地
  照样标记已撤回（语义是「我撤回了」，不是「对端确认了」）。
- 已取走的部分收不回，这是协议事实，不是实现缺陷。
- 前端菜单条件收敛到 `canRecall()` 一个纯函数，测的是函数而不是模板字符串。

## 验证

`cargo test --lib recall_`（5 条，全部用 `encipdict_test_ctx` 的回环假对端）：

- `recall_text_message_sends_only_delmsg`：文本撤回只发一条 DELMSG，不发 RELEASEFILES，
  本地标 recalled、重发队列里没有该包号。
- `recall_file_message_releases_slot_then_notifies_peer`：撤回前 `GETFILEDATA` 能取回
  文件内容，撤回后同一请求读到 0 字节；报文顺序 RELEASEFILES 在 DELMSG 之前。
- `recall_folder_message_releases_dir_slot`：文件夹（`dir_entry`）同样，撤回后
  `GETDIRFILES` 取不到目录树。
- `recall_broadcast_is_rejected_with_dedicated_code`：`E_RECALL_BROADCAST|` 开头，
  记录不改、报文不发（另含「不在广播信箱但带 `broadcast:true`」的记录）。
- `recall_while_peer_offline_still_marks_and_releases`：对端摘出在线表后撤回仍 Ok，
  本地 recalled、槽释放、零报文。

`npm test` 307 pass（原 300 + `scripts/recall.test.mjs` 新增的 7 条：canRecall 的
文本/文件/表情/对方/已撤回/广播各分支、`canReedit`、以及两条源码锁——菜单用
`canRecall` 而不是 `kind === 'text'`，recalled 分支必须在 `msg-row` 之前）。

没覆盖到的：真机双实例互发附件再撤回（本机单实例限制，只能由假对端顶替）。
