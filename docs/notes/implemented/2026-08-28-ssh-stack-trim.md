# SSH/SFTP 栈整体瘦身

Status: implemented

## Problem

SSH 下载源（`ssh+http://` 隧道与 `sftp://`）在现网调度概率仅 2-3%，却占约 350KiB `.text`：russh 122KiB + russh-sftp 42KiB + ssh-key 13KiB + flurry 14KiB + 自身中间件 128.5KiB + 散布单态化（`cargo bloat`，本机 x86_64-pc-windows-msvc、非 build-std、opt-level=s + LTO）。低命中路径背高体积不成比例。

已证伪的方向：对中间件做机械收缩（错误构造收敛 `#[cold]`、重连块去重）实测仅 −1.5KB——体积在 async 状态机本身，不在冷路径胶水。

## Decision

三层同时落地。

**连接模型是每请求一条 SSH 连接，不复用。** SSH 节点是"低命中、高突发"，而弱网对抗依赖 TCP 多连接——把 N 路并发塞进少数连接的多路复用与该目标相悖；节点为自有，拉 10+ 条 SSH 连接无 MaxStartups 顾虑。连接池、`MAX_STREAMS` 轮转、LRU 驱逐、`ActiveStreamGuard`、SFTP 会话缓存以及复用场景专属的"重连一次"逻辑均不存在；连接随响应流结束而关闭。`SshMiddleware` 与 `SftpMiddleware` 因此是无状态单元结构体。

**SFTP 客户端是自写的最小实现**，替代 russh-sftp。协议面为 SFTP v3 只读子集的 4 条报文：INIT/STAT/OPEN/READ。CLOSE 省略——连接随流结束整体关闭，服务端自然回收句柄。READ 保持 4 路流水线与乱序缓冲以保吞吐，短读按缺口补发。flurry 与 chrono 随 russh-sftp 出图。

**fork（xytoki/russh）在 `client-minimal` 下继续收窄客户端功能面。** 已 gate 的部分包括 pty/shell/exec/signal/env/window-change/x11 通道请求、agent 转发、远端与 streamlocal 端口转发、keyboard-interactive 认证、keepalive/ping、no-more-sessions、extension-info 等待、xon-xoff，以及服务端主动通告的主机密钥列表（`hostkeys-00@openssh.com`）。保留握手/KEX/rekey/password+none 认证/direct-tcpip/session+subsystem。主机密钥日志行不再调用 `to_openssh`，改打算法名——前者为一条 debug 消息带进整套 PEM 编码器。`handle_msg` 的兜底分支在最小档不格式化消息，否则 `Msg`/`ChannelMsg` 整棵树的 `Debug` 会被保活。

`crypto-cng` 同时启用 `ssh-key/ecdsa`。该后端验证 `ssh-rsa`/`rsa-sha2-256` 与 `ecdsa-sha2-nistp256` 两类主机密钥签名，而 `algo-minimal` 的协商列表也提供这两者；RSA 公钥解码只需 `alloc`，ECDSA 解码需要 `ecdsa`，缺失时服务端一旦出示 ECDSA 主机密钥，会在进入验签前以 `AlgorithmUnknown` 失败。

## Alternatives considered

- 从零写最小 SSH 传输层：再省约 50KiB，但 rekey、窗口流控的协议正确性自担，投入产出比不如裁 fork。
- feature-gate 整个 SSH 能力：功能是真实需求（2-3% 调度到），不可去。
- 保留连接池只简化实现：池/轮转的复杂度是复用语义固有的，实测也压不出体积；且复用本身与弱网对抗目标相悖。
- 不启用 `ssh-key/ecdsa`，只依赖线上节点全为 RSA 主机密钥：可省 16KiB，但客户端协商顺序把 `ecdsa-sha2-nistp256` 排在 `rsa-sha2-256` 之前，任何提供 ECDSA 主机密钥的节点都会握手失败，取值不稳。
- 以 `OpaquePublicKey` 承载 ECDSA 主机密钥，避开 `ssh-key/ecdsa`：不可行。opaque 编码为 `string 算法名 || string 密钥字节`，而 ECDSA blob 是 `string 算法名 || string "nistp256" || string Q`，多一层长度前缀，round-trip 非字节等同，交换哈希与指纹都会错。

## Verification

测量条件：`cargo bloat`，本机 x86_64-pc-windows-msvc、非 build-std、opt-level=s + LTO。

| 判据 | 结果 |
|---|---|
| `cargo tree` 无 russh-sftp、flurry | PASS：两者均不在依赖图中 |
| SSH 全家 `.text`（`russh\|ssh_key\|capabilities::(ssh\|sftp)` 过滤合计）降至 ~220KiB 以下 | PARTIAL：350KiB → 235.8KiB。缺口来自 `ssh-key/ecdsa` 的 16KiB；不启用该 feature 时为 ~220KiB，但 ECDSA 主机密钥即无法握手 |
| Go 测试 server 跑通 `ssh+http://` 与 `sftp://` 的整文件与 Range 下载（200/206/416 语义）、指纹不匹配拒连 | PASS：`capabilities::sftp::tests::ssh_e2e`（`#[ignore]`，见 `tests/sshd/`），RSA 与 ECDSA 两种主机密钥各跑一轮 |
| 并发突发（≥8 路并行下载）成功，产生独立 TCP 连接 | PASS：e2e 中 8 路并行 Range 下载，各自独立连接 |
| 大文件下载途中 rekey 成功不断流 | 未覆盖：测试 server 未调低 rekey 阈值 |

russh crate `.text` 由 122.3KiB 降至 108.7KiB。自身中间件（`capabilities::(ssh|sftp)` 过滤）为 106.0KiB。发布二进制（x86_64-win7-windows-msvc + build-std + optimize_for_size）在启用 `ssh-key/ecdsa` 前后为 3710464 → 3726848 字节。

单测 58 项通过。

## Consequences

每请求握手增加延迟（弱网 2-4 RTT 加 KEX 计算）。调度到 SSH 节点本就是兜底路径，握手成本相对下载时长可忽略，换来的是天然的多 TCP 连接弱网韧性。

自写 SFTP 客户端面对非 OpenSSH 服务端的兼容性由流水线、乱序缓冲与短读补发覆盖，协议面窄至 4 条报文；Go 测试 server 提供 `-shortread` 注入以持续验证缺口补发。

fork 裁剪沿用既有 `client-minimal` feature 风格，与上游 diff 保持可读，代价是上游合并时需要维护这些 gate。

rekey 路径保留但未经端到端验证。单流大文件必然命中该路径，测试 server 需支持调低 rekey 阈值才能补上这条判据。
