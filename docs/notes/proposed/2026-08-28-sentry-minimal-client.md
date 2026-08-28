# 手写最小 Sentry 协议客户端替换 SDK

Status: proposed

## Problem

sentry SDK 栈是发布 stub 中最大的可移除体积：符号名含 `sentry_types::` 的代码合计 311.6KiB `.text`（`cargo bloat --release --filter "sentry_types::"`，本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std；其中约 170KiB 单态化代码计在 serde_json/serde_core/serde 名下），另有 `sentry` 20.2KiB、`sentry_core` 26.6KiB，合计约 350KiB。

单项中 `Envelope::from_slice` 占 40.5KiB，仅服务于提权管道的 envelope 转发（`ipc/manager.rs`、`utils/sentry.rs` 的 `forward_envelope`），并由此拖入整棵协议 Deserialize 树（Event 全量反序列化路径合计约 50KiB）。

实际使用面远小于 SDK 能力：`capture_anyhow` 错误事件、tracing 面包屑、browser/app/user 上下文、提权进程经管道转发、`traces_sample_rate: 1.0` 的性能事务。SDK 的 panic integration 与 backtrace 均未启用。

## Proposal

以手写最小 Sentry 协议客户端替换 SDK，后端（自托管 Sentry，仅反代 API）与 CI 的 PDB 上传流程不变：

- event 以 `serde_json::json!` 手拼，仅含 `event_id`、`timestamp`、`release`、`level`、`exception.values[{type,value}]`、`breadcrumbs`、`user`、`contexts` 字段；按 DSN 拼 envelope URL POST。
- 面包屑改为自写 tracing Layer 写入环形缓冲，事件发送时附带快照。
- 提权管道改传原始 JSON 字符串，主进程直接转发不解析，协议 Deserialize 整树出图。
- 上报过滤沿用 [遥测通道职责收敛](./2026-08-28-telemetry-channels.md) 的 `Expected` 标记判断。

性能事务保留会话级粗粒度形态：一次安装会话一个 transaction（`install`/`update`/`uninstall`），阶段为其一层子 span——`metadata`、`hash-scan`、`download`（整个并发下载阶段的墙钟时间，不细到单文件）、`runtimes`、`finalize`。span 为扁平数组、`parent_span_id` 一律指向根，全部在主进程编排侧（`session/run.rs`）记录，提权操作按 await 墙钟计时，无跨进程 trace 传播，提权管道的 span header 转发删除。吞吐类数值（校验字节数/文件数、下载总字节）挂 transaction 级 `measurements`（span 级 data 在自托管后端不聚合）。函数级 span、采样逻辑不实现；单文件下载不产生 span，网络性能的家是 DFS insight；流式消费下载流的 patch 阶段不单独计时（其时长即下载时长，无独立信息量）。在错误事件通道上的增量约 100 行、二进制增量 <10KiB。若自托管后端版本不支持 custom measurements，兜底为把被测量编码进附加子 span 的 start/end 时间差，duration 分布任何版本均可聚合。

## Alternatives considered

- 保留 SDK 仅加 `before_send` 过滤：解决噪音不解决体积。
- 第三方轻量 Sentry 客户端：生态中无维护良好的最小实现。
- 迁移到计数遥测端点：无聚合、无符号化、无告警，错误详情无处安放。

## Acceptance criteria

- `cargo tree` 中 `sentry`、`sentry-core`、`sentry-types`、`sentry-tracing` 不再出现；`cargo bloat --filter "sentry_types::"` 为空。
- 同测量条件下 `.text` 减少 ≥ 300KiB。
- 主进程与提权进程触发的测试错误事件在后端可见、分组正常、release 关联正确。
- 面包屑随事件到达，时序与 tracing 日志一致。
- 会话事务在后端按 name 聚合出 duration 分布，阶段子 span（metadata/hash-scan/download/runtimes/finalize）在事务详情中可见；单文件下载不产生 span。

## Risks

- 协议细节自行维护：envelope header 格式、429 rate limit 响应处理、字段兼容性随后端升级变化。缓解：字段集合最小化，后端为自托管、升级节奏可控。
- 发送可靠性低于 SDK（无持久化队列）：进程退出前未发出的事件丢失。缓解：错误事件在失败路径同步发送；崩溃路径由 [崩溃捕获](./2026-08-28-crash-capture.md) 的下次启动上传覆盖。
