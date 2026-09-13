# IPC 枚举改 serde 外部标签

Status: implemented

## Problem

`IpcOperation`（`#[serde(tag = "type")]`）与 `InstallFileMode`/`InstallFileSource`（内部标签/untagged）触发 serde 的 `Content` 缓冲反序列化机制：输入先整体缓冲为中间树再二次匹配，每个变体单态化一份，合计 75KiB `.text`（`cargo bloat --filter "serde::private|ContentDeserializer"`，本机 msvc、opt-level=s + LTO）。这些枚举只在同一二进制的两个进程间过提权管道，JSON 形状对外不可见，标签风格是无谓的体积开销。

## Decision

三个枚举改用 serde 默认的外部标签（`{"变体名": {…}}`），`Content` 机制随之出图（残余 2.8KiB 来自 `SourceField`——打包方手写的项目配置格式，`"source"` 字段须同时接受字符串与数组，untagged 必须保留）。同批移除 reqwest 的 `charset` 特性：所有后端均为 UTF-8，`Response::text()` 无该特性时按 UTF-8 解码，`encoding_rs` 的全编码解码表（20KiB）出图。

## Alternatives considered

- 换二进制序列化（bincode 等）：收益更大但改动面大，且 session dump 的可读性有调试价值。
- 只改最大的 `IpcOperation`：`InstallFileMode`/`InstallFileSource` 嵌套其中，不一起改则 `Content` 机制仍被拉进。

## Verification

| 判据 | 结果 |
|---|---|
| `Content` 机制出图 | PASS：filter 体积 75.1KiB → 2.8KiB（残余为 `SourceField`） |
| `encoding_rs` 出图 | PASS：crate 榜单不再出现（原 20.5KiB） |
| 体积 | PASS：release 二进制 −195,584 字节（本机，含 charset 移除）；与 multipart 移除合并的提交使 CI 产物 4,090,368 → 3,844,608 |
| 协议兼容 | 管道两端为同一二进制，无跨版本兼容问题；e2e 测试与前端不引用这些 JSON 形状 |

## Consequences

- session dump（`04-install-ops.json`）的 JSON 形状变化，人工阅读旧 dump 时注意区分版本。
- 未来若有类型需要跨版本/跨端稳定形状，不能沿用"标签风格随意"的假设，需在类型旁注明。
- 管道帧本身已不再是 JSON，标签风格只影响 debug 构建的 session dump；见 [提权管道帧编码](./2026-09-02-ipc-postcard-frames.md)。
