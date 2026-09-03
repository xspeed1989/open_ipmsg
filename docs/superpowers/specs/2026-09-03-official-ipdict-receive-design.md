# 官方 IPMsg IPDict / EncIPDict 接收链路对齐设计

**日期：** 2026-09-03  
**状态：** 设计已确认，待实现计划  
**范围：** 仅重构 IPDict / EncIPDict 接收链路，不重写 UI、配置和经典 `1:...` 协议

## 1. 背景与根因

Windows 官方 IP Messenger 5.8.6 向当前客户端发送消息时，当前客户端收不到，Windows 端进入 delayed 发送队列。真实入站日志显示，同一批密文包被误解为经典报文：

```text
user="EF" host="6" cmd=0x0007a124
```

`0x7a124` 是十进制 `500004`，即外层 IPDict 的 `EF` 值。这证明完整 IPDict 密文没有在入口被正确识别，而是错位套入了经典五字段解析器。

对照官方源码后确认当前实现有三个根本偏差：

1. 官方 `MsgMng::EncIPDict` 发送完整 `IP2:<content_hex_len>:...:Z` 报文；当前实现错误假定它是 `1:<packet_no>:EF...` 形式。
2. 官方 `IPDict::unpack` 保存 key 顺序和 value 原始字节，只在调用 `get_int/get_str/get_bytes/...` 时解码类型；当前实现在 unpack 时用启发式猜类型，会把 `1111111` 或 `abcdef` 这类正文猜成整数。
3. 官方 `SendEntry::MsgLen` 在后半段重试中会附加 64 字节 NUL，规避某些网卡的 UDP 分片校验和问题；当前 `IP2` 入口只接受 `used == data.len()`，因此将 delayed 重试包拒绝并回落到经典解析器。

## 2. 对照基准与版本限制

对照基准为官方公开仓库 [shirouzu/ipmsg](https://github.com/shirouzu/ipmsg) 的 commit `733f2515b34f7a5f84342448540b1a61d9f1dd0b`（源码版本 4.99r3），重点对照：

- `src/TLib/ipdict.cpp` / `ipdict.h`：`pack`、`unpack`、`get_*`、list/dict 解析；
- `src/msgmng.cpp`：`EncIPDict`、`DecIPDict`、`ResolveMsg`、`ResolveDictMsg`；
- `src/sendmsg.cpp` / `sendmsg.h`：`MakeMsgPacket`、`UDP_CHECKSUM_FIXBUF`、delayed 重试发送；
- `src/mainwinmsg.cpp`：SENDMSG 验证、去重、落地和 `RECVMSG` 回执顺序。

官网当前版本是 [5.8.6](https://ipmsg.org/)，但公开仓库没有对应版本源码。因此本设计以 4.99r3 的可审计协议核心为基准，并以 5.8.6 真实报文和真机互通作为最终验收。

“1:1 对齐”指线上格式、类型读取语义、验证顺序、解密与回执行为一致，不要求机械翻译 C++ 类型、Windows CryptoAPI 或 UI 代码。

## 3. 总体架构

入站数据流统一为：

```text
UDP bytes
  → IP2 识别与完整性检查
  → 可选的 64B NUL 重试填充验证
  → IPDict raw-value unpack
  → 外层含 EB ? DecIPDict : 明文 IPDict
  → ResolveDictMsg（必填字段 / 签名 / command|flags）
  → 现有命令分发
  → SENDMSG 落库与 UI 通知
  → RECVMSG ACK
```

经典 `1:packet:user:host:command:extra` 解析只在输入完全不是 `IP2` 时执行。已经识别到 `IP2` 头但内容不完整、长度错误或带非法尾随数据时，必须记录原因并丢弃，不允许用经典解析器二次解释。

## 4. IPDict 数据模型

### 4.1 原始值存储

解包后的每个 value 保留原始字节，并保留 key 插入顺序。解包阶段不再猜测 Int、Str、Bytes、List 或 Dict。

为降低对现有代码的破坏，Rust 实现可保留已有 `Val` 构造变体，但从网络 unpack 得到的值统一进入 `Raw(Vec<u8>)` 或等价的原始缓冲区表示。这个内部选择不能改变下述外部语义。

### 4.2 按 getter 解码

- `get_int`：仅在调用时按官方有符号十六进制规则解码；
- `get_str`：仅在调用时将原始字节作为 UTF-8 文本读取，空值是合法空字符串；
- `get_bytes`：返回完整原始字节；
- `get_dict`：按 `key:hex_len:value` 内容段解析；
- `get_ipdict`：按完整 `IP2:...:Z` 解析；
- `get_*_list`：先按 `hex_len:value` 列表切片，再对每项调用对应类型解码。

同一份 raw value 可被不同 getter 解读，这与官方 `IPDict` 一致。例如 `BODY=1111111` 通过 `get_str` 读回 `"1111111"`，不会因为它也是合法 hex 而丢失正文。

### 4.3 无损重打包

未修改的入站字典重新 pack 时，key 顺序和 value 字节必须逐字节不变。签名验证依赖对除末尾 `SIGN` 外的字典按原始顺序重打包，不允许在 unpack 后因类型猜测改写原始值。

## 5. `IP2` 完整性与重试填充

`Dict::unpack` 继续返回已消费字节数 `used`。网络入口按以下规则判定：

- `used == data.len()`：正常完整 IPDict；
- `data[used..]` 恰好为 64 字节且全为 `0x00`：官方 delayed 重试填充，正常处理；
- `used > 0` 但尾随数据不符合上述条件：partial/invalid IPDict，记录并拒绝；
- `used == 0` 且输入以 `IP2:` 开头：malformed IPDict，记录并拒绝；
- `used == 0` 且不以 `IP2:` 开头：交给经典协议解析。

填充只在完整 `:Z` 之后判定，不能用“先剥所有尾部 NUL”遮蔽报文内部长度错误。

## 6. EncIPDict 解密、签名与 Resolve

### 6.1 外层解密

完整 IPDict 含 `EB` 时视为 EncIPDict 外层，按官方 `DecIPDict` 顺序：

1. `EI` 必须是 16 字节 IV；
2. `EK` 必须是可由本机 RSA-2048 私钥解开的会话密钥；
3. `EB` 使用 AES-256 CTR 解密；
4. 解密结果必须是完整的内层 `IP2:...:Z`，不允许剩余数据。

外层解密状态使用独立 `dec_mode` / metadata 表达，不通过删改 `FLG` 伪装成明文报文。

### 6.2 ResolveDictMsg

按官方顺序读取并验证：

1. `VER == IPMSG_NEW_VERSION`；
2. `PKT`；
3. `UID`；
4. `HID`；
5. `CMD`；
6. `FLG`；
7. 根据命令读取 `STAT`、`BODY`、`NCK`、`GRP`、`CVER`、`FILE` 等可选字段；
8. 按 `command |= flags` 合并兼容命令。

现有“无条件剥掉 `SECRETOPT | ENCRYPTOPT`”不符合官方 `ResolveDictMsg`，将移除。“传输已解密”与“消息是否封书”是两个独立状态，UI 应根据 `SECRETOPT` 保留官方封书语义。

### 6.3 签名验证

对含 `SIGN` 的内层字典：

1. 取除末尾 `SIGN` 外的原始顺序字典重打包；
2. 根据 `PUBE` / `PUBN` 构建或更新发件人公钥；
3. 按 `EF` 声明的 SHA-256 算法验签；
4. 验签失败时拒绝分发和 ACK，并记录失败阶段。

无签名的明文 IPDict 是否可接受继续按官方命令类型和现有兼容策略处理；本次必须确保官方 EncIPDict 的内层签名被实际验证，不能只把 `enc=true` 当成 `sig_ok=true`。

## 7. 分发、去重与 ACK

解析后生成统一的 resolved message，交给现有命令分发。SENDMSG 保留现有会话注册、历史落库、附件处理、通知、封书和不在回复逻辑。

ACK 顺序对齐官方 `MsgSendMsg`：

- 新包：完成解密、结构验证、签名验证、落库与 UI 通知后，再回 `RECVMSG`；
- 重复包：若之前已成功接收，仍立即补回 `RECVMSG`，因为发件人重试正说明旧 ACK 可能丢失；
- 仅当 `SENDCHECKOPT` 存在且不是 `BROADCASTOPT | AUTORETOPT` 时回 ACK；
- ACK 的正文使用内层 `PKT` 的十进制字符串，不使用外层长度或其他临时编号；
- 解密、结构或签名失败：不 ACK，保留对端重试与密钥自愈机会。

## 8. 诊断与安全

诊断日志记录：

- 来源 IP/port；
- UDP 总长度、IPDict `used` 长度、NUL 填充长度；
- 失败阶段（外壳、EI/EK/EB、内层、必填字段、签名、分发或 ACK）；
- 包号、command/flags 和 key 名。

默认不长期记录整封密文，不记录解密后 BODY、会话密钥、私钥或完整附件内容。

## 9. 兼容性与非目标

### 必须保持

- 经典 `1:...` 报文继续走原有 `protocol::parse`；
- 现有明文 SENDMSG、经典加密 SENDMSG、附件、封书、已读回执和不在回复行为不变；
- 现有纯 IPDict 成员主 / DIR 报文继续可用；
- 现有 Rust `put_*` 构造 API 尽量保持，减少出站链路回归。

### 本次不做

- 不重写全部经典 IPMsg 协议；
- 不机械移植 Windows CryptoAPI、MFC/UI 或发送队列管理；
- 不在没有 5.8.6 源码的情况下声称逐行对齐 5.8.6；
- 不为未见过的非官方 `1:<packet>:EF...` 格式保留第二套入口。

## 10. 测试设计

实现遵循 TDD，先让每个回归测试因现有偏差失败，再做最小修复。

### 10.1 IPDict 单元测试

- unpack 后 raw value 可由正确 getter 读取；
- `BODY="1111111"`、`BODY="abcdef"`、`BODY=""` 都通过 `get_str` 原样返回；
- 同一 raw value 可由不同 getter 按官方规则解读；
- bytes 中含 NUL/冒号/非 UTF-8 时长度边界不受影响；
- dict/list/ipdict 嵌套格式正常与截断输入；
- 官方 fixture 解包后原样重打包，逐字节一致。

### 10.2 网络入口测试

- 无填充完整 `IP2` 被识别；
- 尾随恰好 64 NUL 的 `IP2` 被识别；
- 尾随非零数据、错误填充长度和内部长度错误被拒绝；
- malformed/partial `IP2` 不会回落为 `cmd=0x0007a124`；
- 经典 `1:...` 报文仍由原解析器处理。

### 10.3 解密与签名测试

- 完整官方形状 EncIPDict 解密得到内层 `IP2`；
- EI 长度错误、EK 无法解开、EB 被篡改和内层截断均失败；
- 正确 SHA-256 签名通过，字段、顺序或 SIGN 被篡改时失败；
- 验签使用除末尾 SIGN 外的原始重打包字节。

### 10.4 端到端自检

更新 `selftest` E10，使用官方完整 `IP2` 外壳，并覆盖：

- 新消息首次发送；
- 纯数字正文落库与上屏；
- 附加 64 NUL 的 delayed 重试；
- 重复包不重复落库但会补 ACK；
- ACK 使用内层 PKT；
- 封书、附件和 flags 保留官方语义。

### 10.5 完整验证

```bash
cargo test --manifest-path src-tauri/Cargo.toml
pnpm test
pnpm build
cargo run --manifest-path src-tauri/Cargo.toml -- --selftest
git diff --check
```

当前仓库全量 `cargo fmt --check` 已有大量与本次无关的基线差异，不能为了让命令变绿而格式化全仓。本次必须保证新改动本身没有空白错误，且不扩散无关格式变更。

## 11. 真机验收标准

使用 Windows 官方 IP Messenger 5.8.6 向当前客户端发送：

1. 含字母的普通文本；
2. 纯数字 / hex-like 文本；
3. 一条封书消息；
4. 至少一条等待超过重试门槛、实际带 64 NUL 的 delayed 消息。

所有场景必须满足：

- 客户端正确上屏或按封书语义显示；
- 正文逐字符一致；
- 历史中 `enc=true`，签名状态与真实验证结果一致；
- 诊断不再出现 `user="EF" host="6" cmd=0x0007a124`；
- 诊断明确显示 `IP2` 解包、EncIPDict 解密、内层 PKT 落库和同 PKT `RECVMSG`；
- Windows 端收到 ACK，相应条目从 delayed 发送队列消失。

