# 核心流程 e2e 补全

Status: proposed

## Problem

`tests/` 十项全是 packed HTTP 源的成功路径。线上主要下载路径 DFS2（`session/source.rs` 的 `ensure_dfs2_session` / `create_dfs2_session_with_challenge` / `resolve_dfs2_location` / `prefetch_batch_urls`，`dfs.rs` 的五个 API）零覆盖；H3 / QUIC 下载路径（`capabilities/h3.rs`）流量占比约 20–30%，同样零覆盖。没有任何中断后重跑的用例，尽管重跑即修复是安装器最重要的恢复属性；[文件提交协议](../implemented/2026-09-02-atomic-file-commit.md) 引入的暂存目录、两阶段提交与恢复核对都只有单测，没有一条用例在真实进程被结束后验证安装目录的状态。

## Proposal

### DFS2 stub

`tests/server.mjs` 增加 DFS2 stub：`GET <api>?with_metadata=1` 返回 `Dfs2Metadata`（`data.index` 指向 fixtures 中 hashed 目录、`data.metadata` 为 `gen` 产出）；`POST <api>` 创建 session，默认直接返回 `sid`，`?challenge=1` 时先返回 `challenge: "md5"` 与 `data: "<hash>/<source>"`（与 `dfs.rs::solve_dfs2_challenge` 的 md5 前缀搜索一致）再验证 `challenge` 字段；`GET <api>/session/<sid>/<res>?range=` 返回 `Dfs2ChunkResponse`；`POST <api>/session/<sid>/<res>` 批量返回 `Dfs2BatchChunkResponse`；`DELETE` 接收 `Dfs2DeleteRequest`。chunk URL 指向同一 express 的静态文件并透传 Range。stub 支持两个故障开关：`?corrupt=<file>` 使指定文件的 chunk 返回篡改后的字节；`?delay=<ms>` 使每个 chunk 响应前等待。

### 用例

- `online-install-dfs2` / `online-update-dfs2`：源 uri 为 `dfs2+http://localhost:8080/api/TestApp`，断言与现有 `online-install` / `online-update` 相同的文件集与更新器自更新哈希；`online-update-dfs2` 以 `?challenge=1` 运行。
- `update-corrupt-updater`：stub 对 `updater.exe` 的 chunk 返回篡改字节；断言更新以非零退出码结束、`updater.exe` 哈希仍等于 v1、安装目录及其父目录无 `.kachina-staged`、`%TEMP%\kachina-staged\` 下无对应条目、无 `.instbak`；随后以正常 stub 重跑更新成功。
- `update-interrupted`：stub 以 `?delay=300` 拖慢下载，更新启动 2 秒后 `process.kill`（落在阶段一）；断言每个受 metadata 管理的文件哈希都等于 v1、暂存目录内无 `journal`；重跑一次后文件集与 `online-update` 相同且暂存目录已删除。
- `update-interrupted-commit`：以隐藏环境变量让安装器在阶段二完成一半时 `process::exit`；断言暂存目录内 `journal` 存在；重跑一次后文件集与 `online-update` 相同、暂存目录已删除、stub 未收到任何 chunk 请求（前滚零下载）。
- `update-interrupted-then-overwritten`：同上中断后，把 fixtures 中 v1 的全部文件复制覆盖到安装目录（模拟便携版覆盖）；重跑一次后断言安装目录每个文件哈希等于 v1、暂存目录已删除；再以 `--source local-v2` 正常更新成功。
- `update-interrupted-target-changed`：同 `update-interrupted-commit` 中断后，服务端 metadata 切到 v3（至少一个 journal 内文件的哈希不同）；重跑一次后断言暂存目录已删除、文件集为 v3、stub 收到了 v3 的 chunk 请求（journal 单元的新哈希与本次 metadata 不等，前滚被丢弃）。
- `update-cancelled`：stub 以 `?delay=300` 拖慢下载，更新启动 2 秒后经 silent 模式不可用的取消入口——用例以 WebView 路径运行、通过 `chrome.webview` 消息注入 `{"kind":"cancel"}`——断言每个受管文件哈希等于 v1、暂存目录已删除、进程退出码为 0 且日志含 `cancelled`。
- `install-cross-volume`：安装目录位于 `subst` 出来的盘符下——按卷根比较它是另一个卷，走同级 `<安装目录>.kachina-staged`，而底层同卷保证 rename 仍然成功——断言暂存目录出现在同级并在成功后删除。

### H3 路径

H3 需要 QUIC 服务端夹具，Node 无内置实现；参照 `tests/sshd/` 以 Go 编写 `tests/h3/`（`quic-go` 的 HTTP/3 服务端，静态文件 + Range），CI 增加 Go 工具链步骤。用例 `online-install-h3` / `online-update-h3` 以 `h3://localhost:<port>/...` 为源，断言与 HTTP 版相同。是否纳入本轮由 Go 夹具的维护成本决定，作为本 note 的可选部分，落地时在 Verification 中记录取舍。

## Alternatives considered

- SSH / SFTP 路径 e2e：`tests/sshd/` 夹具已存在，但该路径流量占比约 3% 且 `capabilities/ssh.rs` 有 Rust 单测，不纳入。
- UI 自动化（CDP 驱动 WebView2、UIA 驱动 TaskDialog）：验证的是文件系统结果，silent 路径即可断言；取消用例例外，取消没有 silent 入口，以 WebView 消息注入替代按钮点击。
- 把 e2e 留在文件提交协议 note 内：两个提案的生命周期不同——提交协议的单测足以让其迁 implemented，e2e 补全（尤其 H3 夹具）可以独立排期，且 DFS2 覆盖本身与提交协议无关。
- 用 Rust 集成测试起 DFS2 stub：现有 e2e 全部是 Node 脚本 + express，再加一套 Rust 侧 stub 是第二种夹具体裁。

## Acceptance criteria

- 新增 e2e 用例加入 `test:all` 并全绿：`online-install-dfs2`、`online-update-dfs2`、`update-corrupt-updater`、`update-interrupted`、`update-interrupted-commit`、`update-interrupted-then-overwritten`、`update-interrupted-target-changed`、`update-cancelled`、`install-cross-volume`；现有十项全绿。
- `update-corrupt-updater` 在文件提交协议落地前的构建（`refactor/native` 上 `49a4505` 及之前）上运行必须失败（`updater.exe` 消失或哈希不等于 v1），证明用例能抓到原问题。
- `offline-install` 追加断言：成功后安装目录父目录下无 `.kachina-staged`。
- `update-interrupted-commit` 断言 stub 的 chunk 请求计数为 0。
- H3 若纳入：`tests/h3/` Go 夹具在 CI 上构建并启动，`online-install-h3` / `online-update-h3` 全绿；若不纳入，本 note 迁 implemented 时在 Verification 中记录原因。

## Risks

- DFS2 stub 与线上服务的偏差：stub 只实现 `dfs.rs` 类型定义的字段与 `solve_dfs2_challenge` 支持的 md5 challenge；服务端新增字段或 challenge 类型不会被 stub 覆盖。
- `update-interrupted` 的 kill 时机：以 `?delay` 拖慢每个 chunk 保证 2 秒时仍在下载；若 CI 机器极慢导致 2 秒时尚未开始写入，用例退化为普通重跑，不会误报失败。
- `update-cancelled` 依赖 WebView2 与 `chrome.webview` 消息注入：CI 的 windows-2022 自带 WebView2 运行时，但需要一个可从测试脚本触达的注入口（例如隐藏环境变量让宿主在启动 N 秒后自发 `Intent::Cancel`）；若注入口本身就是隐藏环境变量，则该用例可以改走 silent 路径。
- H3 Go 夹具：CI 多一个工具链与构建步骤，`quic-go` 版本与 `seera-msquic` 的互操作可能出现协商差异；夹具维护成本是决定是否纳入的主要变量。
- 隐藏环境变量（阶段二中途退出、定时取消）进入发布二进制：仅在 `cfg(debug_assertions)` 或专用 feature 下编译，release 产物不读取。
