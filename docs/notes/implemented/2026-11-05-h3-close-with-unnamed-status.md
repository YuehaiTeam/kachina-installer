# 对端以未命名状态关闭 H3 连接时按下载失败处理

Status: implemented

## Problem

h3-msquic-async 0.3.1（crates.io）的 `convert_connection_error` 把 msquic 的 `ShutdownByTransport` 转成 h3 的 `ConnectionErrorIncoming` 时，为判断是否超时对状态调用 `try_as_status_code().unwrap()`。`try_as_status_code` 只认 msquic Rust 绑定的 `StatusCode` 列出的状态，传输层关闭可以带别的状态：msquic 把对端发来的 CRYPTO_ERROR（0x100 加 TLS alert）映射为 `QUIC_STATUS_TLS_ALERT(alert)`（msquic `src/core/connection.c` 的 `QuicErrorCodeToStatus`），而 `StatusCode` 只列出 alert 0、42–46 与 116。上游仓库同样是 `unwrap()`，没有相关 issue 或 PR。

已建立的 H3 连接在打开流、读或写时遇到这种关闭，这个 `unwrap()` 就会 panic。安装器的 panic hook（`native/utils/sentry.rs` 的 `install_panic_hook`）上报 Sentry，非静默时弹出崩溃对话框，然后以退出码 1 结束进程，安装中断。release profile 为 `panic = "abort"`，`catch_unwind` 不起作用，本仓库代码无法在 crate 外拦住这次 panic。

## Decision

`[patch.crates-io]` 把 h3-msquic-async 指向 fork 仓库的 main 分支，与 msquic-async 用同一个 fork。fork 中的 h3-msquic-async 与 crates.io 0.3.1 的源码只差 `convert_connection_error` 这一处及其单测：状态是 `CONNECTION_TIMEOUT` 或 `CONNECTION_IDLE` 时为 `Timeout`，其余状态（包括 `StatusCode` 没有列出的）为 `Undefined`，与其他非超时的传输层关闭相同。

`Undefined` 带着 msquic 的原始错误，由 `native/utils/net_failure.rs` 的 `quic_status` 分类：`StatusCode` 没有列出的状态得到标签 `QUIC_TRANSPORT:<状态的十六进制值>`，归连接失败类，用户看到的码是 `DOWNLOAD_REFUSED`，DFS insight 记录同一标签。

## Alternatives considered

- 等上游修复：修复发布之前，崩溃一直存在。
- 在本仓库内绕过：这次转换发生在 h3 调用 h3-msquic-async 的 `Connection` 时，本仓库代码拿到的已经是转换后的 h3 错误；`panic = "abort"` 下 `catch_unwind` 也拦不住。

## Verification

| 判据 | 结果 |
|---|---|
| 未命名状态的传输层关闭为 `Undefined`，空闲关闭仍为 `Timeout` | PASS：fork 中 h3-msquic-async 的单测 `transport_close_with_an_unnamed_status_is_undefined`（`QUIC_STATUS_TLS_ALERT(48)`）与 `transport_close_on_idle_is_a_timeout`；改回 `unwrap()` 时前者 panic。只测了 seera 后端（`--no-default-features --features msquic-seera,msquic-seera-static`） |
| 经 H3 下载时，对端以 CRYPTO_ERROR 关闭连接不再 panic，码与 insight 为下载失败 | PASS：临时 quic-go v0.65.0 服务器（Go 1.27.3，不入库）完成 ALPN h3 握手，等到客户端的第一个请求流后，以传输错误 0x130（TLS alert 48，unknown_ca）关闭连接。临时测试经 `ipc_install_file` 下载 `http3://`（`#cert=` pin 服务器自签证书），debug 测试构建，x86_64-pc-windows-msvc。crates.io 0.3.1 下 panic 于 `h3-msquic-async-0.3.1/src/lib.rs:68`（`Unknown QUIC_STATUS`）；换成 fork 后返回错误，标签 `QUIC_TRANSPORT:0x80410130`，码 `DOWNLOAD_REFUSED`，insight 记同一标签。release 安装器中 panic 走 panic hook 的路径由代码得出，没有在 release 构建上跑这个用例 |
| H3 下载照常完成 | PASS：x86_64-win7-windows-msvc release（`pnpm build`）。本地 quic-go HTTP/3 反向代理（不入库，自签证书，签名 URL 临时带 `#cert=` pin）挂在 `tests/server.mjs` 前，`KACHINA_E2E_H3=1` 下 `test:online-install-dfs2` 两次通过，代理日志中的 4 个请求都是 HTTP/3.0 |
| 既有回归 | PASS：release 的 `test:prepare` 与 offline-install、online-install、offline-update、online-update、dfs2、updater-survival、already-latest、uninstall、userdata-ignore、occupied-process、builder-extract-replace、plugin-stub、sfx-run 均通过；单测 builder 37、installer 222（1 ignored）、sfx 40 通过 |

## Consequences

- h3-msquic-async 与 msquic-async 都跟随 fork 的 main 分支，升级上游版本要先合进 fork。
- Cargo.lock 锁定 fork 上的一个提交，构建机要能从 fork 的远端仓库取到这个提交。
- `StatusCode` 没有列出的传输层关闭与其他传输层关闭一样按连接失败上报，标签只有十六进制状态值，要对照 msquic 的状态定义才能读出是哪个 TLS alert。
