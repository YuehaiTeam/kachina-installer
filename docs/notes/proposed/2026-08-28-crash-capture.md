# panic hook 与 minidump 崩溃捕获

Status: proposed

## Problem

发布构建 `panic = "abort"` 且未启用 SDK panic integration，Rust panic 目前完全无上报；Windows 上 abort 经 `__fastfail`，不走 SEH，`SetUnhandledExceptionFilter` 对 panic 不可见。native 崩溃面不小——hpatch/hdiff（C）、msquic、WebView2 COM——访问违例同样无任何记录。两类崩溃都表现为进程无声消失。

## Proposal

两级捕获，panic hook 先行：

1. panic hook（`std::panic::set_hook`，abort 前执行）：采集 panic 消息、location 与面包屑环形缓冲快照，作为错误事件经 [最小客户端](./2026-08-28-sentry-minimal-client.md) 上报；可选 `RtlCaptureStackBackTrace` 采裸地址 + `EnumProcessModules` 拼 `debug_meta`，由后端配合已上传的 PDB 做符号化。
2. SEH minidump：`SetUnhandledExceptionFilter` + dbghelp `MiniDumpWriteDump`（`MiniDumpNormal` 或加 `WithIndirectlyReferencedMemory`，几十 KiB 量级）写入 `%TEMP%`，下次启动时 multipart 上传后端 minidump 端点，服务端符号化。提权子进程安装同一套 hook。

## Alternatives considered

- crashpad/breakpad：进程外写 dump 更可靠，但体积与集成复杂度和安装器不匹配。
- 启用 SDK panic feature：与移除 SDK 的方向冲突。
- 不做：native 依赖面决定了盲区不可接受，且崩溃不留痕迹时问题只能靠用户描述复现。

## Acceptance criteria

- 触发测试 panic（debug 构建或隐藏参数），后端产生事件，含消息、location 与面包屑。
- 触发测试访问违例，`%TEMP%` 产生 minidump，下次启动上传成功，后端栈符号化正确。
- 两条捕获路径均不产生用户可见卡死；hook 内部失败时静默放弃，不影响原有崩溃流程。
- 提权子进程崩溃同样可见，与主进程事件可区分。

## Risks

- 进程内写 dump 是 best-effort：堆损坏时 `MiniDumpWriteDump` 可能失败。接受，不追求 crashpad 级可靠性。
- 上传依赖下次启动，用户崩溃后可能不再运行安装器，存在覆盖率缺口。自更新场景会再启动，部分覆盖。
- minidump 含内存片段，有隐私面。缓解：坚持最小 dump 类型，不采集全内存。
