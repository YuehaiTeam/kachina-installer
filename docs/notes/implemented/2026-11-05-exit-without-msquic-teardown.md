# 进程退出不拆除 msquic

Status: implemented

## Problem

下载客户端 `DOWNLOAD_CLIENT`（`native/main.rs`）是进程级静态量。H3 可用时它持有一个 msquic registration、一个 configuration 与 H3 连接池（`native/capabilities/h3.rs` 的 `H3Inner`），三者活到进程结束。seera-msquic 的 Rust 绑定以 `#[ctor::dtor] close_msquic`（`src/rs/lib.rs`，上游 microsoft/msquic 的绑定相同）在 CRT `exit()` 的退出回调里调用 `MsQuicClose`，库引用归零后 `MsQuicLibraryUninitialize` 拆除整个库。msquic 要求拆库前已关闭全部 registration（`src/core/library.c` 中 `MsQuicLibraryLazyUninitialize` 的断言）。

进程从 `main` 返回时经过 CRT `exit()`，于是：

- release 构建中该断言为空操作，拆库照常释放 `MsQuicLib.Partitions`，主线程随后在 `CxPlatWorkerPoolDelete` 等活连接的 socket 释放 worker pool 引用，等不到。等待期间活连接的定时器（`QuicConnTimerExpired → QuicOperationAlloc`）或收到的数据报（`QuicBindingReceive`）访问已释放的分区，进程以 0xc0000005 结束；没有定时器或数据报触发时进程一直不退出。只有本进程经 http3:// 下载过、连接池里有活连接时出现；没有活连接时 release 正常退出。
- debug 构建中该断言失败，退出码 0xc0000420；只要创建过带 H3 中间件的 `DOWNLOAD_CLIENT` 就出现，与是否走过 http3:// 无关。

两种情况下安装都已完成，出错的是退出码与等待进程结束的调用方，例如以 `-S` 调用安装器或更新器并检查退出码的应用。H3 的启用条件见 `native/capabilities/mod.rs` 的 `probe_h3_support`（Windows 11 build 22000 起、无系统代理），下载源还须给出 http3:// 地址。

## Decision

`main` 的最终收尾 `installer::uninstall::delete_self_on_exit()` 之后调用 `std::process::exit(0)`。它在 Windows 上是 `rt::cleanup()`（冲刷 stdout）加 `ExitProcess`，不执行 CRT 的退出回调，seera-msquic 的 dtor 不运行，msquic 与 H3 资源随进程回收。

所有走到 `main` 末尾的入口共用这一出口：WebView 与原生界面、静默安装、提权子进程、WebView2 安装。错误路径同样经 `std::process::exit` 结束。`crash-dialog` 子命令在 `capabilities::init()` 之前返回，不打开 msquic。

跳过 CRT 退出回调不丢数据：依赖中只有 seera-msquic 使用 `ctor`，本仓库代码不注册退出回调，日志写入无缓冲的文件句柄（`native/utils/log.rs`）。

## Alternatives considered

- 退出前按 msquic 的约定关闭 H3 资源：趁 tokio runtime 仍在时清空连接池、等连接任务结束，再关 configuration 与 registration。`RegistrationClose` 阻塞到所有连接关闭，任一连接句柄未释放，进程就永久卡在退出；还要能从静态客户端取回 `H3Inner`。进程正要退出，这些代价只换来资源归还。
- 修改 seera-msquic 的 dtor（删除，或仍有 registration 时跳过）：与上游绑定分叉，而 registration 活到进程结束是本仓库客户端的设计。

## Verification

条件：x86_64-win7-windows-msvc release（`pnpm build`）与 dev 构建，Windows 11 Insider 26H1 build 28020。H3 用例在 `tests/server.mjs` 前挂一个本地 quic-go HTTP/3 反向代理（不入库，自签证书，签名 URL 带 `#cert=` pin），`KACHINA_E2E_H3=1` 让 DFS2 签名 URL 为 http3://。

| 判据 | 结果 |
|---|---|
| 经 http3:// 下载后，release 安装器退出码为 0 且不卡住 | PASS：fixture `test-app-v1.exe -S -O --source dfs2-v2` 直接拉起 8 次，均退出码 0、约 0.7 s 结束，每次日志有 2 条 http3:// 下载。未加 `std::process::exit` 的构建同条件 8 次中 7 次 0xc0000005、1 次 90 s 内未退出，文件均已装好 |
| 崩溃与卡住出在 msquic 拆库 | PASS：未加 `std::process::exit` 的构建，cdb：卡住进程的主线程为 `close_msquic::__dtor → MsQuicClose → MsQuicLibraryUninitialize → CxPlatWorkerPoolDelete`；崩溃（2 次）在 worker 线程 `QuicConnTimerExpired → QuicOperationAlloc`，读到已释放内存 |
| debug 安装器退出码为 0 | PASS：dev fixture `-S -O --source dfs2-v2`（http 下载，日志有 `H3FallbackMiddleware enabled`）3 次均为 0；去掉 `std::process::exit(0)` 后 3 次均为 0xc0000420 |
| `std::process::exit` 不执行该 dtor | PASS：release 单测二进制经 `DOWNLOAD_CLIENT` 请求 cloudflare-quic.com 后，从测试 harness 的 `main` 返回的 15 次均为 0xc0000005，以 `std::process::exit(7)` 结束的 8 次均为 7 |
| 界面路径走到 `main` 末尾后退出码为 0 | PASS：200 MB 离线探针包停在就绪页时向就绪页窗口（WebView 主窗口、`native-ui` 的 TaskDialog）发 `WM_CLOSE`，两种界面各 3 次，均退出码 0。提权子进程未测：本机会话未提权，UAC 无法自动确认 |
| e2e 断言看到安装器的真实退出码 | PASS：上述 release 单测二进制的两种配置（直接拉起时退出码 7 与 0xc0000005）经 zx 的 pwsh `& exe` 启动时，结果的 `exitCode` 都是 0。`runInstaller`（机制见[核心流程 e2e 补全](./2026-09-02-e2e-core-coverage.md)）下，H3 条件的 `test:online-install-dfs2` 对未加 `std::process::exit` 的 fixture 失败于 `exit code 3221225477`，对修复后的 fixture 3 次通过；安装目录的父路径是普通文件时报告退出码 1 |
| 既有回归 | PASS：release 的 `test:prepare` 与 offline-install、online-install、offline-update、online-update、dfs2、updater-survival、already-latest、uninstall、userdata-ignore、occupied-process、builder-extract-replace、plugin-stub、sfx-run 均通过；单测 builder 37、installer 217（1 ignored）、sfx 40 通过。`test:machine-acl`、`test:registry` 需要提权会话，未跑 |

## Consequences

- 进程不归还 msquic、H3 与其他进程级资源，由操作系统随进程回收；CRT 退出回调一律不执行，日后引入依赖 `atexit` 或 `#[ctor::dtor]` 做收尾的库时，它的收尾同样不会运行。
- 打开过 msquic 的入口若不经 `main` 末尾而直接返回，同样的崩溃与卡住会出现；约束写在 `main` 末尾的注释里。
