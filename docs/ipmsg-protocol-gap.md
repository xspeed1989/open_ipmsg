# IPMsg 协议差距分析（对照官方最新稳定源码）

日期：2026-09-02
对照基准：shirouzu/ipmsg master（Windows IP Messenger 官方源码，
协议头 `src/ipmsg.h` Ver4.50（2017-06-12）、根目录 `protocol.txt` 草案 14 版；
线上客户端线为 5.8.x）。Open IPMsg 侧以 `src-tauri/src/{protocol,net,crypto,state}.rs`
当前工作区代码为准。

## 一、命令覆盖对照（基本命令 = 低 8 位）

| 命令 | 值 | 官方用途 | Open IPMsg 现状 |
| --- | --- | --- | --- |
| NOOPERATION | 0x00 | 无操作 | 只定义常量，不处理 ✅（无操作本就无需处理） |
| BR_ENTRY | 0x01 | 上线广播 | ✅ 广播 + 应答 ANSENTRY + 能力位 |
| BR_EXIT | 0x02 | 下线广播 | ✅ 退出时尽力广播 |
| ANSENTRY | 0x03 | 应答上线 | ✅ 注册对端 + 触发预握手 |
| BR_ABSENCE | 0x04 | 不在模式变更 | ⚠️ 仅当作在线 upsert 处理；**ABSENCEOPT 语义与「离开」状态无 UI** |
| BR_ISGETLIST | 0x10 | 主机列表能力探索 | ❌ 只定义常量 |
| OKGETLIST | 0x11 | 主机列表能力应答 | ❌ 只定义常量 |
| GETLIST | 0x12 | 请求主机列表 | ❌ 只定义常量 |
| ANSLIST | 0x13 | 回送主机列表 | ❌ 只定义常量 |
| ANSLIST_DICT | 0x14 | IPDict 版主机列表（v5） | ❌ 未定义 |
| BR_ISGETLIST2 | 0x18 | 新版主机列表探索 | ❌ 未定义 |
| SENDMSG | 0x20 | 发送消息 | ✅ 文本/附件/加密/分段/离线入队 |
| RECVMSG | 0x21 | 送达确认 | ✅ 对 SENDCHECKOPT 必回，进制双写兜底 |
| READMSG | 0x30 | 已读通知 | ✅ 收发双向已实现 |
| DELMSG | 0x31 | 封书破弃通知（消息删除/撤回告知） | ❌ 只定义常量；**无撤回消息功能** |
| ANSREADMSG | 0x32 | READMSG 带 READCHECKOPT 时的确认 | ❌ 只定义常量 |
| GETINFO | 0x40 | 取版本信息 | ✅ 应答 SENDINFO |
| SENDINFO | 0x41 | 版本信息应答 | ✅ 构造/发出；入站仅记录不展示 |
| GETABSENCEINFO | 0x50 | 取不在通知文 | ❌ 未定义 |
| SENDABSENCEINFO | 0x51 | 回送不在通知文 | ❌ 未定义 |
| GETFILEDATA | 0x60 | TCP 取文件 | ✅ 流式/断点/解密/多方言 |
| RELEASEFILES | 0x61 | 放弃接收释放槽 | ✅ |
| GETDIRFILES | 0x62 | TCP 取目录树 | ✅ **双向已实现**（发送 collect_dir_ops/serve_dir_stream，接收 download_dir_task/fetch_dir_tree） |
| DIRFILES_AUTH | 0x63 | 密码保护目录传输 | ❌ 未定义 |
| DIRFILES_AUTHRET | 0x64 | 密码目录应答 | ❌ 未定义 |
| GETPUBKEY | 0x72 | 取公钥 | ✅ 预握手/自愈重握手 |
| ANSPUBKEY | 0x73 | 回公钥 | ✅ 宽容解析 + 持久化 |
| AGENT_REQ/ANSREQ/PACKET/PROXYREQ | 0xa0–0xa3 | NAT 中继代理（跨网段/透传） | ❌ 未定义 |
| DIR_POLL…DIR_AGENTREJECT 等 | 0xb0–0xb8 | 成员主（DIR_MASTER）目录服务 | ❌ 未定义 |

## 二、选项 / 能力位覆盖对照

| 选项 | 值 | 官方用途 | Open IPMsg 现状 |
| --- | --- | --- | --- |
| ABSENCEOPT / SENDCHECKOPT | 0x100 | 不在模式 / 请回送达确认 | ✅ SENDCHECK 全链路；❌ ABSENCE 语义无 |
| SERVEROPT / SECRETOPT | 0x200 | 服务器预留 / 封书 | ❌ 保密消息（封书）未实现 |
| BROADCASTOPT | 0x400 | 广播群发（同报） | ❌ 未实现（README Roadmap 未完成项） |
| MULTICASTOPT | 0x800 | 多选群发 | ❌ 未实现 |
| AUTORETOPT | 0x2000 | 自动应答防乒乓 | ✅ 回执/应答类报文带 |
| RETRYOPT | 0x4000 | HOSTLIST 重传标记 | ❌ 只定义常量 |
| PASSWORDOPT | 0x8000 | 密码锁（消息/文件） | ❌ 只定义常量 |
| DIALUPOPT | 0x10000 | 对拨号成员单发 | ❌ 只定义常量 |
| NOLOGOPT | 0x20000 | 建议不记日志 | ❌ 只定义常量；出站不声明、入站不响应 |
| NOADDLISTOPT | 0x80000 | 单发消息不注册列表 | ✅ 入站分支已处理 |
| READCHECKOPT | 0x100000 | 请回已读通知 | ✅ 双向 |
| SECRETEXOPT | 0x300000 | 封书+已读 | ❌ |
| FILEATTACHOPT | 0x200000 | 文件附件 | ✅ |
| ENCRYPTOPT | 0x400000 | 报文加密 | ✅ |
| ENCEXTMSGOPT | 0x4000000 | 加密含附件元数据 | ✅ 入站/出站都带 |
| CAPUTF8OPT | 0x1000000 | UTF-8 能力声明 | ✅ Entry 恒带 |
| UTF8OPT | 0x800000 | 报文为 UTF-8 | ✅ SENDMSG 用；⚠️ BR 系也置位（规范 §3-9 禁止，见 §五） |
| CLIPBOARDOPT | 0x8000000 | 支持粘图附件 | ✅ 声明 + 接收；⚠️ 我方发出粘图不带 FILE_CLIPBOARD 属性（见 §四） |
| CAPFILEENC_OBSLT | 0x1000 | 废弃 | ✅ 不实现（正确） |
| CAPFILEENCOPT | 0x40000 | 文件流加密能力 | ✅ 声明 + 使用 |
| ENCFILEOPT | 0x800 | 文件流加密请求 | ✅ 用 test 钉死线上值 |
| CAPIPDICTOPT | 0x2000000 | IPDict 能力 | ❌ 不声明（v5 新格式，主线并存未转正） |
| DIR_MASTER | 0x1000000 | 成员主角色 | ❌ 不声明 |

## 三、加密扩展对照（protocol.txt §3-3 / §3-7，ipmsg.h 加密能力位）

| 能力 | 官方 | Open IPMsg 现状 |
| --- | --- | --- |
| RSA-2048 + AES-256 + SHA-256 消息密封 | 标准组合 | ✅ 发送默认组合 |
| RSA-1024 + Blowfish-128 接收 | 参考实现第二组合 | ✅ 接收宽容（`open_message` 双组合，有单测） |
| PACKETNO_IV | 可选 | ✅ 接收支持（CBC IV=包号 ASCII），发送不广告（合法） |
| SIGN_SHA1 / SIGN_SHA256 | 可选 | ✅ SHA-256（消息）与 SHA-1（文件请求规格组合） |
| ENCODE_BASE64 密文组合 | 官方可选（10 版起） | ❌ 不支持也不广告（不广告即不会被要求，互不破） |
| RSA-4096 | 官方头文件已定义 | ❌ 不广告（规范未给标准组合，影响小） |
| TCP 文件流 AES-CTR + 续传对齐 | spec §3-7（11 版） | ✅ 双向 + seek(offset) 对齐 |
| NOENC_FILEBODY（流明文性能变体） | 14 版追加 | ✅ 服务端可解（回明文流）；客户端只发 900000 组合（合理） |
| 加密公告携带 TO: 宛先扩展（13 版） | ENCEXTMSGOPT 有效时 | ❌ 未实现未解析 |
| 密钥指纹用户名（10 版「公开鍵指紋付ユーザ名」） | 用户名字尾附指纹 | ❌ 未实现（不影响旧流程，防伪升级项） |
| 重放防护（packetNo 记忆） | 规范建议 | ❌ 明文包号去重有，加密请求签名重放防护无 |

## 四、文件传输细节对照

- 文件名冒号转义：官方用 `::`（protocol.txt §3-5 明文规定）；本端一律替换为 `_`，
  **解析端不识别 `::`** —— 官方发来含冒号文件名时 `splitn(6)` 段错位、条目被丢。
  ⚠️ 真实互通缺口。
- 我方「粘贴图片」发出 attr=REGULAR、名字 `剪贴板图片_*.png`，不带
  `FILE_CLIPBOARD(0x20)` 与 `CLIPBOARDPOS=位置` 扩展属性、不用 `ipmsgclip_s_*` 命名
  —— 官方对端只能收到普通附件卡片，不会按官方「贴图」内嵌到消息正文。
- 目录流条目类型：官方支持 SYMLINK(4)/CDEV(5)/BDEV(6)/FIFO(7)/RESFORK(0x10)；
  本端**发送时跳过符号链接**、接收时跳过不落盘（内容读掉防错位）——安全取舍，但属未实现项。
- fileattr 高位属性 RONLYOPT/HIDDENOPT/ARCHIVEOPT/SYSTEMOPT 与扩展属性
  UID/GID/PERM/CTIME/MTIME/ATIME/CREATETIME/ACL/ALIASFNAME 均不发送不解析
  （本地 `fileattr::PERM=0x4` 是死代码，未使用，无线上冲突）。
- GETFILEDATA/GETDIRFILES 明文请求不带 UTF8OPT（spec §3-9：附件消息带 UTF-8 时
  请求也应带，让文件名按 UTF-8 表达）—— 官方服务端会按本地码页写文件名，
  中文名靠 GBK 兜底大体可用，日文等其它码页有乱码风险。

## 五、流程 / 行为偏差

1. **不在模式（ABSENCEOPT）整套缺席**：无离开状态 UI、不发 BR_ABSENCE(带 ABSENCEOPT)、
   不回 GETABSENCEINFO/SENDABSENCEINFO。官方在不在模式下还会自动应答带 AUTORETOPT 的消息。
2. **在线消息无 RECVMSG 重传**：官方 §4-12「確認・リトライ」要求超时未确认重发同一包；
   本端只在「离线入队」场景重投（flush_pending_for），在线发出的消息发了就发了。
3. **ANSENTRY 立即回**：官方按成员数/IP 距离随机延迟 0–4 秒防广播风暴，本端即时应答。
4. **BR 系 UTF-8 表达方式**：规范明确 BR_ENTRY/BR_EXIT/BR_ABSENCE **禁用 UTF8OPT**，
   应改用 `\0\n` 后接 `UN:/HN:/NN:/GN:/VS:/PL:` 行（官方 msgmng.cpp 在 CAPUTF8OPT 置位时
   解析这些行）。本端 BR_ENTRY 直接置 UTF8OPT 且不发送 `\nNN:` 扩展 —— 官方客户端实测
   可互通（官方按 UTF8OPT 解昵称），属规范偏差，严格实现对端（飞秋等）可能按代码页误解。
5. **IPv6 缺席**：官方支持 ff15::979 站点组播 + ff02::1 链路组播成员发现，本端纯 IPv4。
6. **洪泛/入口应答节流**：无官方式防风暴随机延迟（同 3）。
7. **TO: 宛先扩展 / 三端中继语义**：ENC 消息解密后不解析 TO: 列表（对单聊无感，群发转发缺失）。
8. **DELMSG（撤回）**：无 UI 无报文，收到对端 DELMSG 也会落入 `_ => {}` 静默丢弃。

## 六、README 与代码的过期项

- README「仍暂未实现：目录递归传输（GETDIRFILES 发送目录会提示暂不支持——接收方向已支持）」
  —— 已过期：`serve_dir_stream`/`collect_dir_ops`/`open_transfer`(GETDIRFILES) 均已在，
  UI 发送文件夹（openFileDialog directory:true）与接收重建都可用。
- Roadmap「文件夹递归传输（GETDIRFILES）」同因此项应勾选；剩余未完成项实为「广播群发模式」「截图发送」。

## 七、建议优先级

P0（低成本、真机互通收益大）：
1. 解析端识别官方 `::` 转义（防含冒号文件名丢条目）；
2. 我方粘图发送带 FILE_CLIPBOARD(0x20) + CLIPBOARDPOS 扩展属性 + `ipmsgclip_s_*` 命名（官方内嵌显示）；
3. 在线消息加 SENDCHECKOPT 超时重发（2–3 次，官方语义）；
4. BR_ENTRY 移除 UTF8OPT 或按规范补 `\0\nNN:/GN:` 扩展；
5. 更新 README（目录双向已支持、Roadmap 勾选）。

P1（功能补齐）：
6. DELMSG 撤回消息（含 UI 与历史标记）；
7. 不在模式：ABSENCEOPT + BR_ABSENCE + GET/SENDABSENCEINFO + 自动应答；
8. BROADCASTOPT 广播群发（Roadmap 已有）；
9. GETFILEDATA/GETDIRFILES 请求按公告带 UTF8OPT；
10. ANSENTRY 防风暴随机延迟。

P2（小众/大工程）：
11. PASSWORDOPT 密码消息/文件、DIRFILES_AUTH 密码目录；
12. SECRETOPT 封书（含 UI 开封交互）；
13. MULTICASTOPT 多选群发；
14. HOSTLIST 系列（BR_ISGETLIST/GETLIST/ANSLIST，拨号场景）；
15. AGENT_* NAT 中继、DIR_* 成员主目录服务；
16. IPv6 组播（ff15::979/ff02::1）；
17. IPDict 新格式（ANSLIST_DICT/CAPIPDICTOPT/IP2: 前缀）。