# ADR-0005：对端密钥轮换的自愈（DecryptErr 自动回复）

- 状态：**已落地**（协议层 `37cf5db` + 呈现层 `a521e3a`/`6e14538` + 单测 `e65e6d6`）
- 相关：ADR-0004（接收去重）之后的实测驱动修复；`CONTEXT.md` 的「对端信任」

## 背景

原版 IPMsg 重启后会生成**新的 RSA 密钥对**。此时若我方还缓存着它的旧公钥：

- `net.rs` 的 `maybe_start_handshake` 用 `.filter(|p| peer_pubkey(&p.key).is_none())` 挑对端
  —— **只挑「缓存为空」的**。缓存里躺着过期钥匙时，预握手永远不再启动；
- 于是我方一直用旧钥匙加密，对端一直解不开 → **每一条都发不出去**；
- 对端唯一的反馈是一条普通 `IPMSG_SENDMSG`，正文里嵌 `==== AutoReply(DecryptErr) ====`
  和英文「My PubKey is updated... press the refresh button and resend」。**协议层没有专门的
  命令位或事件**，所以「出站失败」这个方向只能看正文。

真机复现：用户实测「第一条报错，第二条对端能收到」——即撤缓存 + 重新取钥匙这一轮是有效的。

## 决定

1. **判据收在后端一处**：`is_decrypt_err_autoreply(text)` 匹配**完整包装**
   `AutoReply(DecryptErr)`，不匹配裸 token `DecryptErr`（后者对「用户自己在聊天里打出这串字」
   也成立：后端会白跑一次撤缓存 + 重取，前端还会把人家的话替换成提示）。
2. **自愈**：`maybe_recover_stale_peer_key` 收到该回复 → 撤掉该对端的公钥缓存
   （`forget_peer_key`）+ 把 `encrypt_announced` 置回真 + 立刻 `send_getpubkey` 重新取。
   - 撤过一次后 `peer_pubkey()` 为空 → 后续再来同款回复不会重复动作（**幂等**，避免被对端刷包）；
   - 置回 `encrypt_announced` 是让预握手重试（`HANDSHAKE_RETRY_MAX`）继续兜丢包。
3. **呈现**：后端给记录打 `key_rotated` 标记（`msg` 是嵌套的 `serde_json::Value`，加字段
   **不动线上契约、不需要迁移**，老记录没有该字段即假）；前端只读标记，按 `config.lang`
   渲染本地化提示 + 「重发」按钮，**不在前端做文本判断**（避免「改了后端忘了前端」的漂移）。
   - 「重发」重发的是这条提示之前、我自己发出的**最后一条文本**；找不到就提示手动再发，不静默失败。

## 后果与边界

- 用户不必再去原版那边点「refresh」；第一条仍会失败（对端换钥匙时我们无从预知），第二条起正常。
- **只覆盖出站方向**。入站方向（对端用旧钥匙加密发给我们、我们解不开）我们自己就能结构化发现，
  目前还没接 —— 见 ADR-0006。
- 代价：多一条正文匹配。收紧到完整包装后，误判面只剩「用户手打整串 `AutoReply(DecryptErr)`」
  这一种（后端此时还必须**持有该对端的钥匙**才会动作）。
