# 核心流程 e2e 补全

Status: proposed

## Problem

`tests/` 现有十项场景覆盖的是 packed HTTP 源的成功路径。packed 文件与完整安装包等价，现有 fixture 已包含安装器、metadata、embedded index 和文件数据。

DFS2 的批量签名 URL 与批量下载路径（`session/source.rs` 的 `ensure_dfs2_session`、`prefetch_chunk_urls`，以及 `dfs.rs` 的 DFS2 API）没有 E2E 覆盖。

H3 / QUIC 实际文件下载路径（`capabilities/h3.rs`）没有 E2E 覆盖。

没有任何用例在真实安装器进程发生下载失败后重新运行并验证安装目录和 updater 的可用性；[文件提交协议](../implemented/2026-09-02-atomic-file-commit.md) 的阶段二和恢复核对由 Rust 单测覆盖。

## Proposal

### DFS2 stub

`tests/server.mjs` 增加 DFS2 stub，并复用现有的 packed 完整安装包作为 resource。`GET <api>?with_metadata=1` 返回 `Dfs2Metadata`，其中 `data.metadata` 使用 fixture 生成的 `RepoMetadata`，`data.index` 的 hash、相对路径、offset 和 size 与 packed resource 一致，`installer_end` 对应该 resource 的 installer 前缀结束位置。

`POST <api>` 创建 session，默认返回 `sid`；测试 challenge 时先返回 HTTP 402、`challenge: "md5"`、`data: "<hash>/<source>"` 和 `sid`，再校验客户端携带的 challenge response。challenge 数据遵循 `dfs.rs::solve_dfs2_challenge` 的 MD5 后缀搜索规则。

`POST <api>/session/<sid>/<res>` 接收客户端提交的全部 ranges，返回覆盖这些 ranges 的 `Dfs2BatchChunkResponse`。每个返回 URL 都指向同一 stub 的签名下载端点；下载端点透传 Range，并返回与 packed resource 对应的字节。

`GET <api>/session/<sid>/<res>?range=` 保留为单个签名 URL 接口，但正常用例必须验证批量接口已经覆盖全部实际下载 ranges，不能静默退回该接口。`DELETE <api>/session/<sid>/<res>` 接收 `Dfs2DeleteRequest`，并记录 session 的结束请求。

### 用例

- `online-install-dfs2` / `online-update-dfs2`：控制面 source 使用 `dfs2+http://localhost:8080/api/TestApp`，断言与现有 `online-install` / `online-update` 相同的文件集与更新器自更新哈希；`online-update-dfs2` 运行 challenge 流程；两项均断言 session 创建携带 ranges、批量签名 URL 请求成功、所有实际下载使用批量返回的 URL，并收到 session 删除请求。
- `updater-survival`：测试 server 在 packed HTTP v2 resource 的 Range 下载过程中断开连接；更新进程必须失败，安装目录中的所有受管文件和 updater 仍为 v1，暂存目录已清理；关闭故障后再次运行 updater 必须成功更新到 v2。该用例验证一个代表性的阶段一下载失败，不为每种网络错误增加独立矩阵项。

DFS2 用例使用 DFS2 stub 的控制面与 packed resource；`updater-survival` 复用现有 packed HTTP source。`test:all` 作为完整回归入口追加 `test:dfs2` 与 `test:updater-survival`，CI 矩阵各增加一个聚合 job，不为故障变体逐项增加 job。每个 job 执行前生成一次对应 fixture。

阶段二中断、提交失败、目标被覆盖和 metadata 改变继续由文件提交协议的 Rust 单测覆盖；只有出现稳定且独立的真实进程覆盖价值时，才另行增加 E2E 场景。

取消用例单独排期。若增加 WebView2 自动化，则 `update-cancelled` 使用真实的 WebView 消息入口验证取消；在没有 UI 自动化入口时，不通过隐藏环境变量或 release binary 内的测试逻辑伪造取消。

### H3 路径

H3 需要 QUIC 服务端夹具，Node 无内置实现；可参照 `tests/sshd/` 以 Go 编写 `tests/h3/`，使用 `quic-go` 提供 HTTP/3、静态文件和 Range 服务，并在 CI 增加 Go 工具链步骤。

H3 用例继续使用普通 HTTP 的 DFS2 API 获取 metadata、创建 session 和批量签名 URL，由 batch response 返回 `http3://localhost:<port>/...` 的 signed URL；实际文件 Range 下载经 H3 middleware 完成。用例为 `online-install-h3` / `online-update-h3`，不把 `h3://` 作为 source parser 的输入。

是否纳入本轮由 Go 夹具的维护成本和 CI 上的 H3 互操作性决定；若不纳入，迁移本 note 时在 `Verification` 中记录取舍。

## Alternatives considered

- DFS1 路径不增加独立 E2E；该路径已 deprecated，DFS2 批量签名 URL 是当前需要验证的下载协议。
- SSH / SFTP 路径 E2E 不纳入；`tests/sshd/` 夹具和 `capabilities/ssh.rs` 的单测已经覆盖该低优先级路径。
- 不把测试环境变量或测试逻辑放入正常 release binary；server 故障使用测试进程控制，阶段二精确的提交边界继续由 Rust 单测覆盖，只有外部进程观察能够稳定命中中间状态时才增加对应 E2E。
- 不把 E2E 留在文件提交协议 note 内；提交协议的单测和 DFS2 / H3 的传输覆盖属于不同生命周期，可以独立排期。
- 不用 Rust 集成测试启动 DFS2 stub；现有 E2E 使用 Node 脚本和 express，继续复用同一种夹具技术。
- 取消不使用隐藏环境变量替代 UI 自动化；没有真实 WebView2 自动化入口时，取消用例不作为本轮硬性判据。

## Acceptance criteria

- 新增并加入 `test:all` 的核心 E2E 全绿：`test:dfs2` 和 `test:updater-survival`；现有十项全绿。
- `online-install-dfs2` 和 `online-update-dfs2` 能证明实际下载使用批量签名 URL；批量接口失败、返回不完整或单个 URL 缺失时用例失败，不接受静默回退作为通过条件。
- `updater-survival` 必须观察到 server 实际中断了 packed HTTP Range 下载；失败后 canonical updater 和受管文件保持 v1，暂存目录清理；重试后更新到 v2。
- `offline-install` 追加断言：成功后安装目录的同级路径和 `%TEMP%\kachina-staged\` 中均不存在对应暂存目录。
- 所有正式 release 构建产物不读取 E2E 控制环境变量，不包含阶段退出或自动取消的测试逻辑。
- H3 若纳入：`tests/h3/` Go 夹具在 CI 上构建并启动，`online-install-h3` / `online-update-h3` 全绿；若不纳入，迁移本 note 时在 `Verification` 中记录原因。

## Risks

- DFS2 stub 与服务端的偏差：stub 只实现 `dfs.rs` 当前定义的字段和 `solve_dfs2_challenge` 支持的 MD5 challenge；服务端新增字段或 challenge 类型不会被该 stub 覆盖。
- packed resource 的 Range 映射：合并下载可能请求跨越多个文件的 Range，stub 必须返回连续 packed 字节，不能只为单文件 hash 生成独立响应。
- `updater-survival` 只覆盖一个代表性的阶段一网络中断；其他下载、patch 和提交失败仍由现有单测或后续有明确收益的窄场景覆盖。
- `update-cancelled` 依赖 WebView2 自动化、CI 图形桌面和可观察的消息入口；没有这些前提时不通过隐藏环境变量替代。
- H3 Go 夹具会增加 CI 工具链和证书管理；`quic-go` 与 `seera-msquic` 的互操作性需要在 Windows runner 上验证。
