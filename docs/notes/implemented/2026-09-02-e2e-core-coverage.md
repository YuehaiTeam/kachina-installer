# 核心流程 e2e 补全

Status: implemented

## Problem

安装器需要在不扩大每次回归成本的前提下覆盖当前 DFS2 批量下载协议，以及阶段一下载失败后的 updater survivability。packed 文件与完整安装包等价，现有 fixture 同时提供安装器、metadata、embedded index 和文件数据。

阶段二中断、提交失败、目标被覆盖和 metadata 改变属于文件提交协议的精确边界，由 [文件提交协议](./2026-09-02-atomic-file-commit.md) 的 Rust 单测覆盖。

## Decision

### DFS2 stub

`tests/server.mjs` 使用现有 packed 完整安装包作为 DFS2 resource。`GET <api>?with_metadata=1` 返回 `Dfs2Metadata`，`data.metadata` 使用 fixture 生成的 `RepoMetadata`，`data.index` 的 hash、相对路径、offset 和 size 与 packed resource 一致，`installer_end` 对应该 resource 的 installer 前缀结束位置。

`POST <api>` 创建 session；challenge 场景先返回 HTTP 402、`challenge: "md5"`、`data: "<hash>/<source>"` 和 `sid`，再校验客户端携带的 challenge response。challenge 数据遵循 `dfs.rs::solve_dfs2_challenge` 的 MD5 后缀搜索规则。

`POST <api>/session/<sid>/<res>` 接收全部 ranges 并返回覆盖这些 ranges 的 `Dfs2BatchChunkResponse`。签名 URL 指向 stub 的实际 Range 下载端点，该端点返回 packed resource 中对应的字节。`GET <api>/session/<sid>/<res>?range=` 保留为单个 URL 接口，批量用例通过状态断言实际下载没有走 fallback。`DELETE <api>/session/<sid>/<res>` 接收并记录 `Dfs2DeleteRequest`。

### E2E 场景

`tests/dfs2.mjs` 提供 `online-install-dfs2` 和 `online-update-dfs2`。两个场景验证 metadata、session、MD5 challenge、批量签名 URL、Range 下载、session 删除和最终文件集；`assertDfs2BatchCoverage` 要求 batch endpoint 被调用、single URL 请求为零、实际下载 ranges 全部由 batch response 覆盖。

`tests/updater-survival.mjs` 使用普通 packed HTTP source，不绑定 DFS2。测试 server 在 v2 metadata/index 请求完成并开始实际 Range 下载后持续断开连接；更新进程失败，安装目录中的受管文件和 canonical updater 保持 v1，staging 被清理；关闭故障后再次运行 updater 成功更新到 v2。该场景代表阶段一网络失败，并不为每种故障变体增加独立矩阵项。

`tests/offline-install.mjs` 在成功安装后检查安装路径对应的同级 staging candidate 和 `%TEMP%\\kachina-staged\\<path-hash>` 均不存在。

### Test entry and CI

`package.json` 提供 `test:dfs2` 和 `test:updater-survival`，`test:all` 将两个聚合入口加入完整回归。CI matrix 保留原有十项场景，并增加一个 `dfs2` 聚合 job 和一个 `updater-survival` 聚合 job，不为故障变体逐项增加 job。

CI 另设 `unit-test` job，在 Windows 2022 上执行 `cargo test --manifest-path src-tauri/Cargo.toml`；release job 等待该 job 成功后才发布。

测试 server 的故障控制只读取测试进程环境变量，不进入 installer 的 release binary。正式 release binary 不包含阶段退出或自动取消的测试逻辑。

### Scope exclusions

DFS1 不增加独立 E2E，该路径已 deprecated。H3 / QUIC 夹具和 `update-cancelled` 的 WebView2 自动化不属于本次已实施覆盖；取消不通过隐藏环境变量或 release binary 内的测试逻辑伪造。

## Alternatives considered

- DFS1 路径不增加独立 E2E；当前下载协议覆盖集中在 DFS2 批量签名 URL。
- SSH / SFTP 路径不增加 E2E；`tests/sshd/` 夹具和 `capabilities/ssh.rs` 的单测覆盖该低优先级路径。
- 测试环境变量和故障逻辑不放入正常 release binary；server 故障使用测试进程控制，阶段二精确提交边界由 Rust 单测覆盖。
- E2E 不并入文件提交协议 note；提交协议单测与 DFS2 传输覆盖分别维护。
- DFS2 stub 继续使用 Node 脚本和 express，不另建 Rust 集成测试夹具。
- 取消不使用隐藏环境变量代替 UI 自动化；没有真实 WebView2 自动化入口时，该场景不作为硬性覆盖。

## Verification

| 判据 | 结果 |
|---|---|
| DFS2 install/update 验证 metadata、challenge、batch signed URL、Range 下载和 session 删除 | PASS：`pnpm run test:dfs2` 本地通过；GitHub Actions Windows 2022 release artifact 的 `test (dfs2)` job 通过 |
| DFS2 正常下载不走单 URL fallback，实际 ranges 均由 batch response 覆盖 | PASS：`assertDfs2BatchCoverage` 检查 batch 请求存在、single 请求为零、下载 ranges 全部被 batch 覆盖 |
| updater 阶段一网络失败后仍保留 v1 文件和 canonical updater，staging 清理，重试更新到 v2 | PASS：`pnpm run test:updater-survival` 本地通过；GitHub Actions 的 `test (updater-survival)` job 通过；server 确认实际 v2 Range 下载发生中断 |
| offline install 成功后不留下对应 staging candidate | PASS：`pnpm run test:offline-install` 在新增同级 candidate 与 `%TEMP%\\kachina-staged\\<path-hash>` 检查后通过 |
| 新增聚合入口接入完整回归，CI 不为故障变体逐项增加 job | PASS：`package.json` 的 `test:all` 包含 `test:dfs2` 与 `test:updater-survival`；CI matrix 含各一个聚合 job；最近一次 Windows 2022 run 的原有十项、两个聚合 E2E job 均通过 |
| 正式 release binary 不读取 E2E 故障控制项或测试退出逻辑 | PASS：故障控制仅在 `tests/server.mjs`，阶段退出和自动取消逻辑未加入生产构建 |
| 全量 Rust 单测在 CI 中执行并阻断 release | PASS：独立 `unit-test` job 执行 `cargo test --manifest-path src-tauri/Cargo.toml` 通过，release job 依赖 `build`、`test` 和 `unit-test` |
| H3 / QUIC 和 WebView2 取消覆盖 | NOT INCLUDED：没有纳入本次实施范围，未构建对应夹具或自动化入口 |

## Consequences

- DFS2 的 E2E 覆盖了当前批量 signed URL 协议，并使用 packed 完整安装包验证真实 Range 数据，不增加另一种 fixture 资源格式。
- updater survivability 只有一个代表性的阶段一网络中断场景；网络、patch、提交和 crash 的每一种变体没有逐项进入 CI，提交阶段的精确状态继续由 Rust 单测承担。
- `test:all` 保留为完整回归入口，但日常改动可以只运行受影响的 `test:dfs2`、`test:updater-survival`、`test:offline-install` 或 Rust 单测，避免把十分钟级完整回归作为每次小改动的默认验证。
- CI 仅增加两个聚合 E2E job，避免故障变体无限扩大矩阵；每个聚合 job 内部串行执行其所属场景。
- DFS2 stub 只实现当前 `dfs.rs` 类型和 MD5 challenge，服务端新增字段或 challenge 类型不会自动得到覆盖。
- H3 / QUIC 仍缺少真实 HTTP/3 夹具，WebView2 取消仍缺少 UI 自动化入口；这些路径不属于本次已验证的覆盖范围。
