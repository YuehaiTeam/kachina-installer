# 移除 multipart/byteranges 下载路径

Status: implemented

## Problem

`ipc_install_multipart_stream` 处理服务端以 `multipart/byteranges` 响应多 Range 请求的下载模式，约 245 行解析逻辑加 `multer` 依赖。现网已无节点以此模式响应，调度器也不支持下发该模式，代码是纯死路径，白付约 50KiB 二进制体积。

## Decision

multipart 下载路径整体移除：`IpcOperation::InstallMultipartStream` 变体、`ipc_install_multipart_stream` 函数、`multer` 依赖。多块下载统一走 `InstallMultichunkStream`（逐块独立请求），其共用的辅助函数（`get_chunk_size`/`get_chunk_position`/`should_decompress_chunk`/`create_multi_http_stream`）保留。

## Alternatives considered

- feature-gate 保留：无消费方的代码留 gate 只是延迟删除，且仍占维护面。
- 保留以防未来节点支持：调度器协议不支持下发，先有调度器侧改动才有意义，届时再实现不迟。

## Verification

| 判据 | 结果 |
|---|---|
| `multer` 出依赖树 | PASS：`cargo tree -i multer` 无匹配 |
| 无引用残留 | PASS：全仓 grep 无 multipart 引用（除 `create_multi_http_stream` 命名），前端与 e2e 测试本就不引用 |
| 体积 | PASS：release 二进制 −50,688 字节（本机 msvc、opt-level=s + LTO）；与外部标签改造合并的提交使 CI 产物 4,090,368 → 3,844,608 |
| 现有下载模式不受影响 | 单测全过；multichunk/direct 路径未改动，e2e 由 CI 覆盖 |

## Consequences

- 若未来调度器重新支持 multipart 下发，需重新实现（可从本提交的历史找回）。
- 换来约 50KiB 体积与一个依赖的消失。
