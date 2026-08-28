# 手写最小 Sentry 协议客户端替换 SDK

Status: implemented

## Problem

sentry SDK 栈是发布 stub 中最大的可移除体积：符号名含 `sentry_types::` 的代码合计 311.6KiB `.text`（`cargo bloat --release --filter "sentry_types::"`，本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std；其中约 170KiB 单态化代码计在 serde_json/serde_core/serde 名下），另有 `sentry` 20.2KiB、`sentry_core` 26.6KiB，合计约 350KiB。

单项中 `Envelope::from_slice` 占 40.5KiB，仅服务于提权管道的 envelope 转发，并由此拖入整棵协议 Deserialize 树。实际使用面远小于 SDK 能力：`capture_anyhow` 错误事件、tracing 面包屑、browser/app/user 上下文、提权进程经管道转发、粗粒度性能事务。

## Decision

`utils/sentry.rs` 是一个手写最小 Sentry 协议客户端，sentry/sentry-tracing 依赖已移除，后端（自托管 Sentry，仅反代 API）与 CI 的 PDB 上传流程不变：

- 错误事件由 `capture_anyhow` 以 `serde_json::json!` 手拼：`event_id`、`timestamp`、`platform`、`release`、`environment`、`level`、`exception.values`（anyhow 链逐层展开，root cause 在前）、`breadcrumbs`、`user`、`contexts`。envelope 为三行格式（header、item header、payload），POST 到按 DSN 拼出的 envelope URL，鉴权走 `X-Sentry-Auth` 头。
- 面包屑由 `BreadcrumbLayer`（自写 tracing Layer）把 INFO 及以上事件写入 100 条环形缓冲，事件发送时附带快照，对应原 sentry-tracing 的 Breadcrumb 过滤行为。
- 提权进程置 pipe 模式且不是事件上报点：`TACommandError::serialize` 在 pipe 模式跳过 capture，错误经管道回主进程后由会话边界统一上报，一次失败一个事件、时间线完整（panic 例外，见下）。面包屑跨进程转发保留：提权侧本地留存（panic 事件用）并以 `{"breadcrumb": <json>}` 经 `PIPE_OUTBOX` 转发，主进程原样并入环形缓冲，纯 `serde_json::Value` 传递；envelope 通道（`{"envelope": <raw>}`，主进程不解析直接 POST）作为防御性路径保留，当前无调用方。trace header 转发与 `IpcOperation` 每操作 transaction 删除（`IpcInner.context` 一并删除）。
- 性能事务为会话级 `Transaction`：`run_install` 记 `install`/`update`，`run_uninstall` 记 `uninstall`，trace context 带 `status`（ok/cancelled/internal_error），供后端把失败与取消会话从时长分布中过滤。安装流水的阶段 span（`metadata`、`hash-scan`、`download`、`runtimes`、`finalize`）经 `Transaction::timed` 以 await 墙钟计时，扁平一层、parent 一律指向根；`metadata` span 只包 `fetch_metadata` 网络部分，版本选择弹窗的用户等待不计入。吞吐数值挂 transaction 级 measurements：`hash_scan_files`/`hash_scan_bytes`、`download_files`/`download_bytes`。卸载与 mirrorc 会话只有 transaction 无阶段 span。
- panic hook（`install_panic_hook`）在 abort 前先拉起独立崩溃提示进程（`crash-dialog <event_id>` 隐藏子命令，用户立即看到反馈并获得可反馈的事件编号），再拼 fatal 事件同步发送——独立线程 + 独立 runtime（不依赖可能已损坏的主 runtime）、5 秒上限（客户端 read_timeout 30 秒，黑洞后端会让崩溃中的进程僵住半分钟）。崩溃信息与事件编号同时写 stderr（静默/脚本场景的消费口，无控制台时为无害空操作）；静默安装等无人值守场景不弹框；崩溃提示进程自身不再装弹框 hook，防递归。实现 [崩溃捕获](../proposed/2026-08-28-crash-capture.md) 的第一级。同步路径不走提权管道：`panic = "abort"` 下排队消息来不及被写循环消费，提权进程的 panic 事件在本进程直发 HTTP。
- 发送为异步 fire-and-forget，`INFLIGHT` 计数 + `flush(timeout)` 供 silent 退出路径在 `std::process::exit` 前等待在途事件。
- 上报过滤沿用 [遥测通道职责收敛](./2026-08-28-telemetry-channels.md) 的 `classify` 判断，调用方负责，客户端本身无条件发送。

## Alternatives considered

- 保留 SDK 仅加 `before_send` 过滤：解决噪音不解决体积。
- 第三方轻量 Sentry 客户端：生态中无维护良好的最小实现。
- 迁移到计数遥测端点：无聚合、无符号化、无告警，错误详情无处安放。

## Verification

| 判据 | 结果 |
|---|---|
| `cargo tree` 无 sentry 系 crate；bloat filter 为空 | PASS：`cargo tree -i sentry(-types)` 无匹配；`cargo bloat --filter "sentry"` 仅剩本项目 `utils::sentry` 模块（约 50KiB 含 tracing 单态化） |
| 同测量条件下 `.text` 减少 ≥ 300KiB | PASS：release 二进制 5,041,664 → 4,494,336 字节（−534KiB，本机 msvc、opt-level=s + LTO，基线为去 clap/h2 后构建）。CI 侧参照：去 clap/h2 使 CI 产物 5,009,408 → 4,632,064，sentry 变更的 CI 体积待下次构建核对 |
| 主进程与提权进程的测试错误事件在后端可见、分组正常、release 关联正确 | 未验证：待实际触发后在后端核对 |
| 面包屑随事件到达、时序与 tracing 日志一致 | 部分：环形缓冲行为有单测；端到端待后端核对 |
| 会话事务按 name 聚合、阶段子 span 可见、单文件下载不产生 span | 部分：span 结构有代码保证（仅五个阶段调用点）；后端聚合待核对 |

## Consequences

- 协议细节自行维护：envelope header 格式、429 处理、字段兼容性随后端升级变化。字段集合已最小化，后端自托管、升级节奏可控。
- 发送可靠性低于 SDK（无持久化队列）：错误路径有 `flush` 兜底，崩溃路径由 panic hook 同步发送覆盖；native crash 仍依赖 [崩溃捕获](../proposed/2026-08-28-crash-capture.md) 的 minidump 后续。
- 换来约 534KiB 二进制缩减与提权管道解析面的消失。
