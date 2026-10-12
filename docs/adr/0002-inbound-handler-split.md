# 入站报文处理：dispatcher 只留 preamble，每个命令一个 handler module

`net.rs` 的入站路径现在是两个巨函数：`handle_datagram` 569 行（解析、去重、解密、命令分发、回执、事件、自动应答、自动下载 spawn 全在一个 body 里）、`handle_sendmsg` 288 行（15 步，从拆附件段到 spawn 自动下载）。任何一处协议行为改动都落在同一个函数里，78 个用例也只能从文件底部伸手进来（`mod tests` 还把文件劈成两段：测试之后仍有产线代码，`file_request_inner` 被测试前向引用）。决定按 base command 拆：dispatcher 只保留所有命令共享的那几步 + 一张 handler 表，11 个「只是回一条报文 / 转发」的 arm 合成一个 module，5 个会改会话状态的（`SENDMSG` / `BR_ENTRY` / `ANSENTRY` / `ANSLIST` / `BR_EXIT`）各占一个。**已按此落地。** 实际 interface 是 `(ctx: &NetCtx, from: SocketAddr, pkt: &proto::Packet, key: &str)` —— 比「`(pkt, &NetCtx)`」多两个参数，因为每次分派都得知道来源地址与会话 key；四个参数由 dispatcher 统一备好，handler 不再各自重算。`handle_datagram` 与 `handle_sendmsg` 的签名都保持可从测试调用，所以当时钉着签名的 8 + 14 个调用点没有重写 —— 「用例搬到 handler interface」那条留给真正重写用例时做，本次先保住回归网不动。

## Considered Options

- **23 个 arm 各占一个 module**：粒度看起来最干净，但其中 11 个的 interface 完全相同（「收到这类报文就回一条」），拆开等于造 11 个 shallow module —— 合起来才够 deep。
- **handler 返回 effect 清单、dispatcher 统一执行 I/O**（与前端的事件迁移对称）：被否。Rust 侧的问题是 **locality** 而不是可测性（测试本来就能把报文直接喂给 handler）；为了对称把 socket 的异步写压成可序列化结构，还会让 dispatcher 重新长成一个巨物。

## 复现清单的两条命令

行号会烂，锚点不会：

```bash
# arm 清单（带行号，含嵌套 match）
awk '/^async fn handle_datagram/,/^}/ {printf "%d\t%s\n", NR, $0}' src-tauri/src/net.rs

# handle_sendmsg 的 15 步（每步一行）
awk '/^async fn handle_sendmsg/,/^}/ {printf "%d\t%s\n", NR, $0}' src-tauri/src/net.rs

# 钉住当前签名的测试调用点（拆完要改走 handler interface）
grep -c 'super::handle_sendmsg(' src-tauri/src/net.rs   # 8
grep -c 'super::handle_datagram(' src-tauri/src/net.rs  # 14
```

## Consequences

- **handler 的 interface**：`async fn(pkt, &NetCtx) -> Result<Outcome>`，回执与事件由 handler 用拿到的 `NetCtx` 自己发；dispatcher 只负责 preamble（自回声过滤、`proto::parse`、`mark_seen` 去重、诊断转储）与分派。
- **测试面**：按 arm 的用例搬到对应 handler module 的 interface 上；`handle_datagram` 保留为公开入口，只留少量真报文端到端用例走 dispatcher（现在 8 + 14 个调用点不必全留）。**上一条测试面决策**（前端事件迁移只覆盖事件驱动那半边）见 ADR-0001，两者互不影响。
- **`mod tests` 劈开文件的问题自然消失**：命令的用例跟着命令走，不再需要从文件底部前向引用后面的产线代码。
- 只动 `net.rs` 内部结构：线上行为、`wire.rs` 的契约（命令名 / 事件名 / payload 字段）与 `dirwire` 的目录流格式都不受影响。
