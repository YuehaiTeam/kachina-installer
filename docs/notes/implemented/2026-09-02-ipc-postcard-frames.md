# 提权管道帧编码：换行分隔 JSON 改为长度前缀 postcard

Status: implemented

## Problem

提权管道两端是同一二进制的两个进程，wire 形状对外不可见，但一直用 serde_json 编解码。[编译配置级体积裁剪](./2026-09-02-profile-size-trim.md) 之后重测，IPC 类型的 JSON 派生代码仍占 65.0KiB `.text`（`cargo bloat --filter` 覆盖 `IpcOperation`/`IpcResult`/`PipeMsg`/`Progress`/`IpcError`/`InstallFile*`；本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std），其中 `IpcOperation` 的 `visit_enum` 21.6KiB、`IpcResult` 的 16.8KiB。JSON 派生代码的大头是字段名字符串匹配、每个具名变体一套 `__Field` 枚举、未知字段跳过与缺字段错误路径，这些在两端同构的私有协议里全是无用功。

换行分隔的文本帧还有一个既有缺陷：服务端 `handle_pipe` 用 `read_line` 收帧，对端关闭时 `read_line` 返回 `Ok(0)`、`buf` 为空，落入"空行跳过"分支持续 `continue`，既不退出也不广播 `Disconnect`。

## Decision

管道帧为 4 字节小端长度前缀 + postcard 编码体（`src/ipc/mod.rs` 的 `encode_frame`/`decode_frame`/`read_frame`），帧长上限 64MiB 在编解码两侧同时检查，只用于识别帧头错位。IPC 类型的 serde 派生与外部标签风格不变（见 [IPC 外部标签改造](./2026-08-28-ipc-external-tagging.md)、[IPC 类型化](./2026-08-29-typed-ipc.md)）；postcard 是位置编码，`#[serde(default)]` 无害，但两类载荷不能直接过管道：

- `serde_json::Value` 需要 `deserialize_any`，postcard 不支持。`PipeMsg::Breadcrumb` 改为承载 JSON 文本，主进程 `utils::sentry::add_breadcrumb_json` 解回 `Value`；`Envelope` 本就是文本。
- 带 `skip_serializing_if` 的类型在位置编码下会错位。`RepoMetadata` 仅经 `IpcResult::RunMirrorcInstall` 过管道，该变体改为承载归档内 `.metadata.json` 原文（`Option<String>`），提权侧只解析一次供本地删除列表使用，会话侧用既有的 `serde_json::from_str::<RepoMetadata>` 解析；`MirrorcChangeset` 只在提权侧消费，不再随结果返回。

`IpcError::from_ta` 不再先 `serde_json::to_value(&TACommandError)`：那一步只为触发 `Serialize` 里的上报钩子，而提权进程处于 pipe 模式，钩子恒跳过；去掉后 `uac_ipc_main` 可以在非 pipe 模式的测试进程里运行而不产生上报副作用。

`handle_pipe` 拆为读、写两个任务加一个监督任务：`read_frame` 不可取消，不能与写端共用 `select!`；任一任务结束（对端关闭、帧错位、读写错误、`ManagedElevate` 被丢弃使 `mpsc` 关闭）即中止另一任务，令管道两个半边一起释放、客户端看到 EOF 退出，并广播 `Disconnect` 唤醒 `ManagedElevate::run` 的等待方。客户端 `uac_ipc_main` 读循环改为 `read_frame`，解码失败视为帧错位直接断开。管道任务退出后 `ManagedElevate::run` 向 `mpsc` 投递失败即返回 `IPC_ERR`，不再在 broadcast 上等待永远不会到的回包。

## Alternatives considered

- bincode：serde 派生体积相当，但 2.x 自带 derive 体系、1.x 停更；postcard 的 `default-features = false` + `use-std` 只带 `serde`，是最小的选项。
- 给 `RepoMetadata` 去掉 `skip_serializing_if`：该结构是 builder 写出的 `.metadata.json` 契约，字段是否落盘影响文件形状，不能为管道改。
- 为 `Value` 面包屑定义类型化结构体过管道：面包屑的家是 `utils/sentry.rs` 的 `json!` 构造，两端再各维护一份字段清单不值；JSON 文本一行搞定。
- 保留换行 JSON、只修 EOF 空转：修法一样简单，但不解决体积问题；换帧格式后 `read_line` 路径整体消失。

## Verification

| 判据 | 结果 |
|---|---|
| 帧编解码往返 | PASS：`ipc::tests::pipe_msg_shapes_roundtrip`（`Result`/`Option`/嵌套 `InsightItem`/元组变体）、`operation_with_default_fields_roundtrip`（`#[serde(default)]` 字段）、`read_frame_splits_stream_and_reports_eof`（连续两帧、干净 EOF、截断帧报错、超长帧头报错） |
| 真实命名管道端到端 | PASS：`ipc::manager::tests::pipe_roundtrip_and_disconnect` 在本进程内创建 ACL 管道，客户端跑 `uac_ipc_main`，服务端跑 `handle_pipe`；`Ping` 回 `Ok`、`KillProcess(u32::MAX)` 回 `Err` 且 message 含 `OPEN_PROCESS_ERR`、丢弃 `mpsc` 发送端后等待方收到 `Disconnect`、客户端在 5 秒内退出 |
| 体积 | PASS：3,102,208 → 3,047,424 字节（−54,784；本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std） |
| CI 产物（x86_64-win7-windows-msvc + build-std + optimize_for_size） | PASS：2,806,784 → 2,753,536 字节（−53,248）。该 CI 构建同时包含 [Native 重构 CR 第二轮遗留](./2026-09-02-native-refactor-cr-followup.md) 的两次修复提交，其新增代码计入其中，本改动单独的降幅不低于此数 |
| 单测 | PASS：`cargo test` 13 passed（kachina-builder）+ 63 passed, 1 ignored（kachina-installer） |

## Consequences

- 管道帧不再可读；排查提权通信只能靠两端日志。两端为同一 exe，无跨版本兼容义务。
- 提权进程被外部结束后主进程立即得到 `Disconnect`，正在等待的操作以 `IPC_ERR` 失败而不是无限等待，`handle_pipe` 任务随之退出；之后的提权操作同样立即以 `IPC_ERR` 失败，`ManagedElevate` 不会重新拉起提权进程。
- `ManagedElevate::run` 对 broadcast `Lagged` 的处理仍是退出循环报 `IPC_ERR`（容量 100，进度消息高频时理论上可触发），与本改动无关，未动。
- 新增过管道的类型须避开 `serde_json::Value` 与 `skip_serializing_if`；违反时表现为解码错误导致断连，不会静默错位。`ipc::tests` 的往返测试是加类型时的落点。
- CI 的 e2e 以管理员身份运行，`already_elevated` 短路，不经过管道；管道路径的回归由本进程内的命名管道测试承担。
