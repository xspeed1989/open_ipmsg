# AppState 按不变量分组：先拆对端信任与发件箱

`AppState` 是 27 个字段、91 个 `pub fn` 的 god module，`net.rs` 用 69 个不同方法伸手进来（`diag` 69 次、`config` 48 次、`own_keypair` 35 次、`find_in_record` 26 次、`peers` 23 次）。评审时想的是「每个 handler 只碰一组」，但事实否掉了它：**11 个 handler 同时碰 ≥3 组**（`send_message_opts` 碰 6 组），所以真实目标是让依赖在**语法上**看得见。本次先拆两组试水：**对端信任**（7 字段 + 482 行方法，含唯一的硬不变量：`remember_peer_key` ↔ `durability` 标志 ↔ 测试钩子的**落盘顺序**）与**发件箱**（3 字段 + 110 行，含 `demote_retry_to_pending` 这条横跨两条队列的不变量），做法是 `AppState` 持 `pub(crate)` 子模块、handler 写 `st.trust.…` / `st.outbox.…`。

## Considered Options

- **旧方法保留为转发**（`impl AppState { pub fn upsert_peer(&self, …) { self.peers.upsert(…) } }`）：一个 interface 两个入口，下一轮评审还得再拆一次；试水量的是真实迁移手感，转发会把痛点藏起来。
- **`NetCtx` 直接持各组**（`ctx.trust.…`），`AppState` 退成持久化聚合：会把 `AppState::new` 的加载顺序拆散（config → own_key → peer_keys → pending → hidden 是有意为之），且 47 个命令的 `State<SharedState>` 注入面要跟着改。
- **连基础设施也包一层**（`st.misc.diag(…)`）：`diag` 被调 69 次、`config` 48 次，包一层只是让 69 个调用点变长，而它们本来就不是领域组。

## Consequences

- **可读性复核（本次试水的完成判定之一）**：已分两组的依赖现在肉眼可见 —— `retry_pending_for` 里 `st.outbox.retry_for / bump_retry / demote_retry_to_pending` 与 `st.trust.peer_pubkey / own_keypair` 连成读得通的句子（「用我自己的密钥，密封给那个对端的公钥」）。**但**未分组的 `peers` 仍是**裸字段伸手**（`ctx.st.peers.lock().unwrap()`），`config` / `diag` 依旧高频。结论：收益集中在已分的两组，**下一轮最该收的是 `peers`** —— 字段级伸手是最 raw 的一种泄漏，而顶层留 `config`/`diag` 的决定是对的。
- 持久化随组走：`peer_keys.json` / `ipmsg_key.json` 归 `TrustStore`，`pending_out.json` 归 `OutboxStore`；两条重发队列（`retry_out`）与送达确认计数（`ack_count`）只存内存，保持原样。
- 测试用的字段与钩子（`peer_key_persistence_test_hooks` 及其内部字段）对测试开 `pub(crate)`；`take_pending` 与 `FileEntry::ext` 标 `#[allow(dead_code)]` —— 两者都只有测试在用，从 `pub` AppState 方法变成 crate 私有后 rustc 才指出。
- **已落地的组**（按顺序）：`trust`（对端信任）、`outbox`（重发 + 待投递）、`peers`（对端在线状态）、
  `transfer`（附件槽 + 取文件方言）。`peers` 那组是全仓裸锁最多的一处（25 处 `lock().unwrap()`），
  收进 interface 后读一律返回克隆（锁不跨 `await`）、写一律走 `with_peer` 闭包。
- 归属已定的字段：`offered` + `dialect` → `transfer`（与 presence 零交集，只被取文件路径用）；
  `peer_absence` → `peers`；`ack_count` → `outbox`。
- **刻意留顶层**：`seen_queue/seen_set`（入站去重，已封装在 `mark_seen`/`forget_seen` 后面、
  **零裸泄漏**，与任何领域状态无交集）；`config`/`diag`/`data_dir`/`logs_dir`（基础设施，
  高频调用）。
- 剩下三组未动：history/log、dir/master、agent/relay。下一组做 `history`（调用面最大）。

## 下一组：history/log 的边界（已定，实现待做）

边界与口径（第五轮问答定的）：

- **`logs_dir` 进组**：组自己从 `data_dir` 派生 `logs/`（与 `peer_keys.json` 随 trust、
  `pending_out.json` 随 outbox 一致）。外部只有 net.rs 测试里一处可写性探针用到它。
- **`hidden_contacts` 跟 history**：微信式「删除会话」＝删记录 + 隐藏，是同一条路径上的两个
  动作；`list_sessions` / `delete_contact` / `unhide_contact` / `is_hidden` 都在这组。
- **`hist_lock` 用闭包式持锁**：组暴露 `with_history_lock(f)`，守卫不外流 ——
  `ipmsg_import.rs` 那处「整段导入归并要在锁内」改成把逻辑放进闭包。把导入整段搬进组更干净，
  但工程量大（那文件 900 行），另开一轮。
- 提交切法：**一个提交**（调用面大但全是机械改名，拆开只会多一个红的中间态）。

**试做记录（2026-10-12，未完成，已回退）**：这一组的机械部分走通了 —— 字段/区域搬进
`mod history`、212 处调用点改名（ipmsg_import 21 + selftest 35 + state 84 + net 64 + lib 8），
跑完只剩 ipmsg_import 的括号问题。**卡点只有一处**：把 `ipmsg_import` 里「整段持锁做归并」
那 100 行从 `let _g = st.hist_lock.lock().unwrap();` 改成 `with_history_lock(闭包)`,
要重排一大段的花括号缩进，机械化脚本连改三次都没对上（多一个/少一个 `}`），当时上下文已尽，
选择回退而不是继续盲改。下一轮建议：**只把这一处单独做、单独验证**（先只包闭包、编译通过，
再跑 `consolidate_imported_sessions` 的测试），其余部分照前四组的脚本一次搬完。

事实锚点（行号会烂，用命令当场复现）：

```bash
# 字段与外部访问面
grep -n "hist_lock:\|hidden_contacts:\|logs_dir:" src-tauri/src/state.rs
grep -rn "st\.hist_lock\|st\.logs_dir" src-tauri/src/*.rs   # 1 处（ipmsg_import）+ 2 处（net.rs 测试）

# 组内方法清单（22 个 pub + 私有助手 append_record_locked / rewrite_records_locked /
# rewrite_history / persist_hidden / log_path + 自由函数 load_hidden_contacts）
grep -n "pub fn \(log_record\|upsert_in_record\|search_history\|list_sessions\|clear_history\|is_hidden\|delete_contact\|unhide_contact\|compact_histories\|pending_receipts\|read_history\|update_history_file\|update_history_pkt\|find_history_pkt\|history_contains_text\|mark_in_read\|find_in_record\|find_history_any\|mark_out_read\|mark_out_delivered\|migrate_legacy_history_keys\)" src-tauri/src/state.rs

# 外部调用面（约 47 处）：net.rs 最大是 find_in_record 26 次，ipmsg_import.rs 是 read_history 10 次
for m in find_in_record find_history_any update_history_file log_record read_history; do
  grep -c "st\.$m(" src-tauri/src/net.rs src-tauri/src/lib.rs src-tauri/src/ipmsg_import.rs
done
```
