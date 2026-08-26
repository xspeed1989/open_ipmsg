# 设计文档：IPMsg 端到端加密（消息体 + TCP 文件流）

日期：2026-08-25
状态：已评审通过（会话内设计评审），待实现
关联：README「暂未实现」清单中的「加密协商（GETPUBKEY/ANSPUBKEY）」

## 1. 背景与目标

当前 Open IPMsg 的消息往来与文件传输均为明文。官方 IP Messenger 自 v3 起提供
标准化的加密扩展，v4 追加 TCP 文件流加密。本设计为 Open IPMsg 实现**与官方协议
完全一致**的加密能力：

1. 消息体端到端加密（含文件公告元数据）
2. TCP 文件内容流加密（下载方发起、AES-CTR）
3. **与不支持加密的客户端无缝互通**（自动回退明文）
4. 与官方 v3/v4/v5-dev 客户端可互操作

### 非目标

- IPDict 新格式通道（官方 v5-dev 中尚未成为主干，见 §10 调查结论）
- 私有加密算法（不满足互通需求）
- 保密消息（SECRETOPT）的 UI 语义

### 官方 V5/IPDict 调查结论（2026-08 源码核查）

shirouzu/ipmsg master 中 IPDict 为并存新格式：接收端嗅探 `IP2:` 前缀分流，
dict 版发送通道在业务代码中无调用方且受配置门控；广播包仅尾部附加可选的
`\nIP:<base64>` 字段。**消息加密协商机制 v3/v4/v5-dev 一脉相承**，按经典协议
实现即可全版本互通。

## 2. 协议规格（权威依据）

依据：shirouzu/ipmsg `protocol.txt`（第 14 版）与 `ipmsg.h` Ver4.50+。
以下常量值取自 ipmsg.h：

| 名称 | 值 | 用途 |
| --- | --- | --- |
| `IPMSG_GETPUBKEY` | `0x00000072` | 取对端公钥 |
| `IPMSG_ANSPUBKEY` | `0x00000073` | 公钥应答 |
| `IPMSG_ENCRYPTOPT` | `0x00400000` | 报文加密标志 |
| `IPMSG_ENCFILEOPT` | `0x00000800` | 文件流加密标志 |
| `IPMSG_CAPFILEENCOPT` | `0x00040000` | 广播表明支持文件流加密 |
| `IPMSG_NOENC_FILEBODY` | `0x04000000` | 请求加密但流明文的性能变体 |
| `IPMSG_RSA_1024 / RSA_2048` | `0x02 / 0x04` | RSA 能力位 |
| `IPMSG_BLOWFISH_128 / AES_256` | `0x020000 / 0x100000` | 会话密钥能力位 |
| `IPMSG_PACKETNO_IV` | `0x00800000` | IV 用包号（本设计不使用，IV 全零） |
| `IPMSG_SIGN_SHA1 / SHA256` | `0x20000000 / 0x40000000` | 签名能力位 |

### 2.1 握手线格式

```
GETPUBKEY  扩展部 = "<capa_hex>"                       （如 "103002"）
ANSPUBKEY  扩展部 = "<capa_hex>:<E>-<N>"               E=公钥指数hex, N=模数hex
```

### 2.2 加密消息线格式（SENDMSG，命令置 ENCRYPTOPT）

```
扩展部 = "<capa_hex>:<E_hex>:<body_hex>[:<sig_hex>]"
capa_hex = 本次所用组合的能力位 OR（RSA_2048|AES_256|SIGN_SHA256 = 0x40100004）
E_hex    = 会话密钥(32B) 经对方 RSA 公钥 PKCS#1-v1.5 加密后的 hex
body_hex = 明文(UTF-8/GBK 编码字节 + 尾部 '\0')经 AES-256-CBC(PKCS#7) 加密，
           IV = 全零；文件公告跟在正文 '\0' 之后一并进入密文
sig_hex  = 对 body_hex 字符串的 RSA-SHA256 签名（PKCS#1-v1.5），可选但默认携带
```

官方参考实现仅支持两种组合：
1. **RSA-2048 / AES-256**（可选 PACKETNO_IV / SIGN_SHA1 / SIGN_SHA256 / BASE64）
2. RSA-1024 / Blowfish-128（可选 PACKETNO_IV / BASE64）

### 2.3 TCP 文件流加密线格式（协议第 11 版）

```
下载请求：GETFILEDATA/GETDIRFILES 命令置 ENCRYPTOPT|ENCFILEOPT
扩展部   = §2.2 格式加密如下内层串：
           "<pkt_hex>:<fileid_hex>[:<offset_dec>]:900000:<aes256_key_hex>"
           900000 == AES_256|PACKETNO_IV（固定值，标识密钥用途）
           本组合规格指定 SHA-1 签名变体
变体     ：900000 换成 4000000（NOENC_FILEBODY）= 只加密请求、流保持明文

流加密   ：AES-CTR(BE)，key = aes256_key，
           nonce(16B) = 请求包号十进制 ASCII 左对齐 10 字节 + 6 字节 0x00
           计数器自 nonce 整体按大端从低字节向上递增
范围     ：响应流的全部字节（GETDIRFILES 的 header+contents 连续加密）
```

## 3. 架构

新增 `src-tauri/src/crypto.rs`（纯函数为主 + 密钥管理），依赖 crate：
`rsa`、`aes`、`cbc`、`ctr`、`blowfish`、`sha1`、`sha2`、`rand`（base64 已有）。

```
crypto.rs
├─ KeyPair       生成 / 加载 / 持久化 (data_dir/ipmsg_key.json)
├─ fingerprint() 模数 SHA-256 前 8 字节冒号 hex（设置页核对用）
├─ parse_anspubkey / build_anspubkey        握手编解码（宽容解析）
├─ seal_message()  / open_message()          §2.2 打包 / 解包（收方宽容双组合）
├─ seal_file_request()                      §2.3 加密下载请求
└─ ctr_keystream_seek / CtrStream           CTR 流式加解密（可跳到任意偏移）
```

`net.rs` 挂接点：`send_message`（出站选择加密/明文）、`handle_datagram`
（GETPUBKEY 应答、ENCRYPTOPT 报文解密）、`open_transfer`（加密下载请求）、
`serve_getfile`（解请求 + 流加密）、广播构造（能力位广告）。

`protocol.rs` 补充常量与 ANSPUBKEY/entry 扩展行解析（确认忽略 `\nIP:` 行）。

## 4. 密钥管理

- 首次启用时生成 RSA-2048（后台线程，避免阻塞启动），存
  `data_dir/ipmsg_key.json`：`{"n_b64","e_b64","d_b64","p_b64","q_b64",...}`
- 启动加载失败（损坏）→ 重新生成并在 diag.log 记录
- 指纹展示于设置页

## 5. 握手与回退状态机

```
收到对端 BR_ENTRY/ANSENTRY 且带 ENCRYPTOPT（我方开关开启时）
        → 后台异步 GETPUBKEY → 收 ANSPUBKEY 缓存 {capa, N, E}

send_message(key) 时查 peer_keys.json 缓存：
  ├─ 有缓存            → 加密发送（enc:true 落库）
  ├─ 无缓存            → 明文发送本次 + 若对端广播声明过 ENCRYPTOPT 则触发后台握手
  └─ 已标记"无能力"    → 明文发送，不再尝试（对方重新上线广播后重置）

收到 GETPUBKEY → 回 ANSPUBKEY
  （我方 capa = RSA_2048|AES_256|SIGN_SHA256|CAPFILEENCOPT，后两项随总开关）
```

- 能力缓存持久化至 `data_dir/peer_keys.json`（重启免握手，官方同款语义）；
  对端离线事件不清除缓存，仅重新上线时刷新
- 我方关闭开关：不 advertise、不应答握手、全部明文发送；
  收到加密报文因无私钥而无法解密 → 气泡显示「无法解密（加密已关闭）」占位

## 6. 一期：消息体加密收发

- 发送固定组合 RSA-2048+AES-256-CBC+SHA-256 签名（§2.2）；GBK 编码模式下
  加密的仍是 `encode_out` 输出的原始字节，加密层与编码层正交
- 接收宽容：RSA-1024+Blowfish-128 组合亦可解开；签名校验失败不丢弃，
  落库 `sig_ok:false`，气泡显示警示标
- 回执类报文（RECVMSG/READMSG/ANSREADMSG）不加密（官方一致）；
  广播类报文永不加密
- 离线队列（PendingOut）投递时按投递时刻的缓存能力决定是否加密
- UDP 上限：官方客户端按固定缓冲接收 UDP 报文，hex 编码又使体积翻倍；
  我方取保守自限——加密后总报文 ≤ 8KB（正文约 ≤ 3.4KB），超限拒发并提示
  「消息过长，加密模式下请分段」（与官方 GetEncryptMaxMsgLen 同源约束）
- 落库记录增加 `"enc": true/false` 与 `"sig_ok": bool`

## 7. 二期：TCP 文件流加密

- 广播与 ANSENTRY 增加 `CAPFILEENCOPT`（受总开关控制）
- **下载方**（我方拉取）：仅当对端广播带 CAPFILEENCOPT 才走加密请求；
  生成随机 AES-256 钥，按 §2.3 构造加密请求（SHA-1 签名变体）
- **服务端**（我方供给）：识别 ENCRYPTOPT 请求 → 解出内层（校验签名，
  用请求方缓存的公钥；无缓存则接受但记 diag）→ 按 NOENC_FILEBODY 与否
  决定流是否加密 → serve 时以 CTR 包裹字节流
- **续传对齐约定**：密钥流位置 = 文件绝对偏移。恢复 offset 时双方各自把
  CTR 计数推进 offset 字节（block = offset/16，块内偏移 offset%16），
  保证任意断点续传确定性一致
- 目录传输：GETDIRFILES 的 header+contents 视为单一连续流整体加密
- 现有多方言（hex/dec ID）兼容逻辑仅在**非加密路径**生效；加密请求统一 hex
  （规格规定），解出内层后按精确值匹配文件槽

## 8. 配置与前端

- `Config.encrypt: bool`（默认 true）
- 设置页：「消息加密」开关 + 本机密钥指纹展示 + 说明文案
  （“不支持加密的客户端将自动使用明文通讯”）
- 气泡时间旁小锁图标 = 该条 enc:true；签名异常显示警示标
- 文件传输 UI 不变

## 9. 测试策略

1. 单测（crypto.rs）：密钥持久化往返；§2.2 打包/解包固定向量（尾部 `\0`、
   PKCS#7、签名验证、错误注入）；ANSPUBKEY 解析容错（大小写 hex、缺段）；
   Blowfish 组合解包；CTR nonce 向量；续传偏移密钥流对齐；UDP 上限判定
2. 协议级：扩展现有 `--selftest` 双实例互通自检——两实例分别开启加密，
   完成 握手→密文互发→加密文件互传→断点续传 全链路断言
3. 真机验收清单（自动化部分由 selftest 双实例全链路覆盖；以下为真实环境实测记录）：
   - [x] 与官方 v4.51 Windows 客户端：发现后自动握手，双向密文消息
        （官方→我方消息解密+验签通过 sig=true；我方→官方文字正常显示）
   - [ ] 官方客户端发来加密附件，可正常下载解密（未测，待补）
   - [x] 我方向官方客户端发送附件：文字+文件条目正常显示、可下载
        （修复链：ENCEXTMSGOPT 位 + UTF8OPT 位值 + 服务端请求完整性）
   - [x] 与不支持加密的老客户端：全程明文互通无异常（同网段 221/226 等实测）
   - [x] 关闭加密开关后回到纯明文行为（用户实测确认）

> 2026-08-26 真机互操作修复实录（与官方 v4 Windows 客户端）：
> 1. ANSPUBKEY 尾 \0 剥离 + E 数值解析（官方扩展部 C 字符串惯例）
> 2. 线上 N 维持标准大端（实质测推翻 revendian 假设，官方 revendian 仅抵消
>    CryptoAPI 小端内部表示）
> 3. 服务端 TCP 请求等待完整（\n 收尾/80ms 安静期）——加密下载真实网络必败
>    10054 根因（parse 只验头部、拆包即截断处理）
> 4. UTF8OPT 恢复官方 0x00800000（IPMSG_UTF8OPT 铁证；CAPUTF8OPT=0x01000000
>    仅为 Entry 能力声明）——中文乱码根因
> 5. 加密公告带 ENCEXTMSGOPT(0x04000000)——官方解密后拆分附件段的必要条件，
>    缺位则只见文字不见文件
> 6. PACKETNO_IV 接收支持（CBC IV=包号 ASCII）

> 实现落地记录（2026-08-25）：Task 1–11 全部完成并通过评审；双实例 selftest
> 覆盖握手/密文互发/加密文件传输/能力撤回回退。线格式常量已用测试钉死
> （ENCFILEOPT=0x800、CAPA_FILE_REQUEST=0x20100004）。

## 10. 风险与边界

| 风险 | 缓解 |
| --- | --- |
| 无法本地运行官方客户端做格式比对 | 严格按 protocol.txt 实现 + 双向宽容解析 + selftest 全链路 |
| 第三方实现对 ENCRYPTOPT 广播处理不当 | 总开关一键退回纯明文 |
| RSA 每消息一次运算的性能 | 2048 位单次 <5ms，局域网规模无感 |
| 首条消息可能明文（握手未完成） | 发现即预握手，窗口期极短；与官方语义一致 |
| 加密后 UDP 超长 | 显式报错提示分段 |

## 11. 实施切分建议

一期与二期共享 crypto.rs 基础设施，可同计划分步落地：
A. crypto.rs + 密钥管理 + 单测
B. 握手与消息加密收发（net.rs 挂接 + selftest 扩展）
C. TCP 文件流加密（open_transfer/serve_getfile 改造 + selftest 扩展）
D. 前端设置与气泡标识
