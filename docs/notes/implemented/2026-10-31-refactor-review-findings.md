# 重构后审阅发现的缺陷、行为变更与冗余

Status: implemented

## Problem

本 note 汇总对重构后代码（基准为重构前的 `951f8cb`）的审阅结论，每条给出触发条件与后果。与[重构审阅问题的修复](./2026-09-20-refactor-review-fixes.md)重叠的条目只保留该 note 未覆盖或修复不完整的部分。

### 缺陷

按对用户的后果排序。

| # | 问题 | 触发条件与后果 |
|---|---|---|
| B1 | 提交回滚失败后仍删除备份 | 提交阶段某个 rename 失败，回滚时把 `old\` 中的文件改回原名也失败（如安全软件持续占用）。`undo_file` 只记日志，随后删除 journal 并删除整个暂存目录，`old\` 中的旧文件随之丢失。安装目录缺文件，用户看到的是按已回滚处理的 `FILE_IN_USE`，下次启动也没有 journal 可恢复。 |
| B2 | 提权子进程写任务自锁 | 写任务是容量 500 的通道的唯一消费者，却在面包屑分支里对同一通道 `send().await`。进度以 `try_send` 写入，管道写慢时通道会满；此时一旦选中面包屑分支就永远等待。启动进程的 `run` 无超时，界面停在进行中。触发：提权安装、并发下载多、子进程输出任意 info 日志。 |
| B3 | Mirror酱 增量包剥掉真实目录 | 归档根目录没有 `.metadata.json` 时，以其余条目的最长公共前缀当作包装目录剥除。变更文件全在同一子目录时（只改一个文件的增量包必然如此），该子目录被剥掉，文件装到安装根目录，原位置文件没有更新。 |
| B4 | 以明文 HTTP 向错误上报后端发送 Mirror酱 CDK | CDK 未经转义拼入请求 query。请求失败时挂 `MIRRORC_UNREACHABLE`（S 类，会上报）。`with_http_context` 只在外层追加脱敏后的 context，原始 `reqwest` 错误仍在链中，其 Display 含完整 URL；`capture_anyhow` 把链上每一节的 `to_string()` 写入事件。错误上报后端使用明文 HTTP。触发：Mirror酱 用户网络故障或超时。 |
| B5 | 提权管道可被其他本地进程抢占 | 管道 DACL 授予 Builtin Users 与 AppContainer GA（含 `FILE_CREATE_PIPE_INSTANCE`），低完整性标签允许写入；`first_pipe_instance` 只保护首次创建，实例数不限；管道名可在 `\\.\pipe\` 中枚举，UAC 对话框期间有数秒窗口。其他进程可先以客户端连上启动进程的实例，再建同名实例等待，提权子进程随后连到它，以管理员身份执行其发来的 `InstallFile`、`WriteRegistry`、`RunUninstall` 等操作。前提是本机已有恶意进程且用户同意了安装器的 UAC。子进程取得了服务端 PID，但只用作暂存锁的属主，不做校验。DACL 与重构前相同。 |
| B6 | 拒绝 UAC 后反复弹出 UAC，最后按内部错误处理 | 会话开始时 `Ping` 的结果被丢弃。`run_elevated` 失败时 `start` 提前返回，`started` 仍为 false，之后每个提权操作重新弹 UAC。错误只带字符串 context `ELEVATE_ERR`，没有挂 `ELEVATE_FAILED`，按未挂码错误上报并显示内部错误。触发：标准用户安装到 Program Files，在 UAC 上点"否"。 |
| B7 | 子进程未连上时启动进程永久等待 | `wait_conn` 对 `connect()` 没有超时。子进程连接失败（非 `PIPE_BUSY` 错误，或重试 10 次后）时只弹 `ELEVATE_FAILED` 对话框后退出，启动进程一直停在等待连接，会话无法结束。 |
| B8 | 静默安装时提权子进程仍弹框 | 崩溃提示开关只对 `Command::Install` 取 `!silent`，`HeadlessUac` 一律为 true；子进程不知道启动进程是静默模式。静默提权安装中子进程 panic 会拉起崩溃提示进程，连接失败会弹模态 `ELEVATE_FAILED`。 |
| B9 | 拖入文件后安装界面被替换 | WebView 没有设置 `AllowExternalDrop(false)`，也没有 `NavigationStarting` 拦截；WebView2 默认允许拖入，拖入文件或链接即导航离开安装界面。此时 `frontend_ready` 已上报，看门狗不会恢复；无边框模式下没有关闭按钮。bridge 不校验消息来源，导航到的页面同样可以调用 bridge。 |
| B10 | 无边框窗口无法拖动 | 页面依赖 `app-region: drag`，这需要开启 `ICoreWebView2Settings9::IsNonClientRegionSupportEnabled`；宿主没有开启，窗口过程也没有 `WM_NCHITTEST` 处理。所有 `windowBorderless: true` 的产品受影响。 |
| B11 | 一次节点解析失败即让整个安装失败 | `install_one` 在重试循环内对 `build_install_op(...)` 使用 `?`，解析错误直接跳出循环，不重试也不回退整文件。混合补丁分支对 `resolve_install_source` 同样用 `?`，补丁不可用时不回落全量；相邻的磁盘补丁分支用的是 `if let Ok`。 |
| B12 | 配了运行库或带 `deletes` 的包到不了"已是最新" | 已最新判定要求 `plan.deletes` 为空且没有配置运行库；计划只按忽略目录过滤 `deletes`，不看文件是否存在。已是最新的安装每次运行都重跑运行库流程、提交空删除，最后显示"更新完成"。 |
| B13 | `pack` 校验失败时退出码为 0 | `pack_cli` 校验失败只 `eprintln` 后返回，builder 入口不为 `Pack` 设置退出码。CI 判为成功，产物缺失或仍是旧文件。 |
| B14 | 安装器解析 `\0INDEX` 无边界检查 | 不检查剩余长度，索引截断即 panic。 |
| B15 | 安装大小超过 4 GiB 时 `EstimatedSize` 偏小 | 先把字节数截断为 `u32` 再除以 1024，"应用和功能"中显示的大小偏小。 |
| B16 | CSS 主题下仍显示默认插图 | 只在 `theme === 'image'` 时渲染图片，其余情况一律渲染默认 SVG。在 `.image` 中自绘的 CSS 主题被默认插图覆盖。 |
| B17 | 占用文件列表显示两遍 | 文案 `prompt.occupied_files.message` 带 `{items}`，确认框又把 `items` 单独渲染为列表；`process_running` 中应用名已在句中，下方又列一次。 |
| B18 | DFS1 节点探测全失败时用不到 `source` | `pick_fastest_test` 在探测全部失败时返回第一个测试地址，`dfs.source` 分支因此不可达。 |
| B19 | 多显示器缩放不同时布局错位 | 清单声明 PerMonitorV2，初始尺寸取主显示器 DPI，窗口过程不处理 `WM_DPICHANGED`。窗口在缩放不同的显示器上时物理尺寸不变，WebView 光栅化随显示器缩放，固定尺寸的布局被裁切或留白。 |
| B20 | 进度百分比倒退 | 提交末尾已到 98，运行库阶段报 96，之后快捷方式阶段又回到 98。 |
| B21 | SFTP 服务端多给字节时卡到超时 | 只处理短读，不检查 DATA 长度超过请求长度；超长块使已吐出位置越过下一个待取键，之后取不到数据，等到操作超时。需要行为异常的服务端才触发。 |
| B22 | "获取 CDK"链接用错字段 | 链接参数用 `project.title`，应为 `appName`，且没有 URL 编码。 |

### 代码事实已确认、效果需真机确认

| 问题 | 代码事实 | 待确认 |
|---|---|---|
| Windows 11 21H2 的背景 | `is_win11` 以 build ≥ 22000 判定；对所有 ≥ 22000 的版本设置 `DWMWA_SYSTEMBACKDROP_TYPE`（22621 起才支持，失败被忽略），并无条件扩展边框到整个客户区、把 WebView 背景设为透明。 | 22000～22620 上客户区的实际渲染效果。 |

### 看起来不对的行为变更

按后果排序。

| # | 变更 | 后果 |
|---|---|---|
| C1 | 配置字段从有默认值变为必填 | 重构前前端为缺失字段补默认值；重构后 `publisher`、`title`、`description`、`windowTitle`、`programFilesPath`、`uninstallName`、`updaterName` 等缺一即 `PKG_BROKEN`。依赖默认值的打包配置重新打包后无法启动。 |
| C2 | 静默与非交互更新改用线上版本 | 重构前 `-S`/`-I` 下一律保留本地版本；重构后自动应答时满足"更新且无嵌入索引"即改用线上版本。依赖安装包固定版本的批量部署会装上其他版本。 |
| C3 | 安装中关闭 WebView 窗口无确认 | `WM_CLOSE` 直接销毁窗口，不检查会话是否在运行，提交阶段同样如此；原生界面把关窗映射为取消。文件由 journal 兜底，但两个渲染器行为不一致。 |
| C4 | 非中英文系统回落到英文 | 语言按区域格式选择，无匹配时取第一列（en-US）；`system_lang` 的 `zh-CN` 兜底只在文案表没有任何语言列时生效。重构前只有中文，zh/en 以外的用户由中文变为英文。 |
| C5 | WebView2 环境不再传浏览器参数 | 环境选项为 `None`，SmartScreen、文本选择小菜单、捏合缩放、滑动导航恢复为 WebView2 默认开启。 |

### 过度设计

| 问题 | 说明 |
|---|---|
| 不可达的 GitHub JS 插件 | GitHub 来源由 Rust 解析，强制插件名为 `github` 时也不派给 JS 插件。 |
| `\0IMAGE` 进程内 base64 往返 | 读取时编码，唯一使用方再解码。 |
| 网络错误按文本分 12 类 | `classify_error` 以子串匹配分类，下游只用 4 个码，与 [UI 契约](./2026-09-02-ui-contract.md)"只看类型、不做文本匹配"相反。响应体错误经 reqwest 包装后 Display 恒为 "error decoding response body"，12 类里实际只走到一类，DFS insight 收到的几乎都是同一个值。 |
| 无发送方的 `PipeMsg::Envelope` | 提权子进程不产生非阻塞上报，这条管道路径没有发送方。 |
| 提权 IPC 进度走第二条通道 | `pending` 已存每个请求的进度回调，进度另走 broadcast。 |
| `REQUEST_CLIENT` 别名 | 标为待移除的别名，全库仍在使用。 |

### 无效测试

| 测试 | 问题 |
|---|---|
| `web/__tests__/` 全部 | 断言从文案表抄来的文本和组件内写死的 intent，测不到界面展示质量；连同 vitest、jsdom、`@testing-library/preact` 依赖。 |
| `compare_offline_install_dump_if_present` 与 `tests/dump-offline-install.mjs` | 数据目录不入库，测试在 CI 中直接返回；dump 脚本只为它产生数据。 |
| `*_is_not_lost` | 先取得 oneshot 再应答，测的是 tokio oneshot 的缓冲。 |
| `pack_embed_matches_legacy_*` | 在测试中手写旧 serde 结构再对拍。 |
| e2e `stagingCandidates` | 用 JS 重写暂存路径哈希；Rust 侧一改，断言检查不存在的路径并照样通过。 |

### 文档与注释

写错的注释：`ipc/mod.rs` 提到不存在的 `Chunk` 变体；`utils/error.rs` 自称 `anyhow-tauri` 库的一部分，附带的 Tauri 示例失效；`session/state.rs` 的 `allow(dead_code)` 理由不成立；`fs/staging.rs` 称日志路径在 `main.rs`，实际在 `utils/log.rs`；`main.rs` 称 command 不是 `Command::Install`，紧接着的 match 处理了它。

过期的 note 与 README：[重构审阅问题的修复](./2026-09-20-refactor-review-fixes.md)写"修复建议尚未实施"，多数修复已落地；[下载规划器](./2026-09-21-download-planner.md)写 Rust 迁移和下载执行器"仍待实施"，代码里已有对应实现；[Preact 渲染器](./2026-09-02-frontend-preact-renderer.md)称主题为 None 时不渲染图片列；[手写参数前端](./2026-08-28-remove-clap.md)称帮助信息用 rfd 弹窗，rfd 已移除；[遥测通道职责收敛](./2026-08-28-telemetry-channels.md)描述的 `classify`、`Expected`、`TACommandError::serialize` 已不存在，与 UI 契约之间没有取代链接；README 的 `pack` 示例把 `-m`（`--metadata`）写成图片参数，图片参数是 `-t`，且仍提到 Tauri。

`docs/design/` 为空，几篇 implemented note 在充当现行行为参考。

## Decision

B1～B13 与 B15～B22 在修复前逐条对照代码核对，均属实（B20 的触发位置与审阅时所记不同，现象相同）；B14 由[共享包读取与边写边校验的写出管线](./2026-11-04-pack-reader-and-write-pipeline.md)修复。C1、C2、C3、C5 恢复重构前的行为或补齐缺失的确认，C4 保留。

### WebView 宿主（B9、B10、B19、C3、C5）

- WebView2 环境参数为 `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`；Settings5 关闭捏合缩放，Settings6 关闭滑动导航，Settings9 开启非客户区支持（`app-region: drag` 依赖它），取不到对应接口时保持运行时缺省；Controller4 `SetAllowExternalDrop(false)`。与重构前 wry 的设置一致。
- `NavigationStarting` 取消离开起始页 origin 的导航，`WebMessageReceived` 丢弃来源不在该 origin 下的消息；`page_origin` 总带结尾斜杠。
- 窗口过程处理 `WM_DPICHANGED`：按系统建议矩形 `SetWindowPos`，随后的 `WM_SIZE` 调整 WebView。
- 关窗：窗口过程把 `WM_CLOSE` 换成 `WM_CLOSE_REQUEST` 交给 UI 循环，页面的 `window_close` 走同一个 `host::request_close`。没有运行中的会话时直接关闭；可以取消时 TaskDialog 确认后 `cancel_running`；取消已受理或当前步骤不可取消时确认后 `force_exit`（`delete_self_on_exit` 后以 1 退出）。同时只开一个确认框。
- 原生进度框的取消按钮始终可点，`NativeCancel::clicked` 在进度框线程上确认；取消与 `begin_commit` 在同一把锁下互斥。文案 `running.force_exit_{title,message,yes,no}`，取消确认复用页面的 `running.cancel_*`。

### 提权 IPC（B2、B5、B6、B7、B8）

- 管道 DACL 只给 SYSTEM 与 Administrators 全部权限，没有完整性标签；`max_instances(1)`。启动进程用 `GetNamedPipeClientProcessId` 核对客户端是本次拉起的子进程（子进程 PID 取自 `GetProcessId`，句柄缺少查询权限而取不到时记 warn 后放行）；子进程用 `GetNamedPipeServerProcessId` 核对服务端是命令行传来的启动进程 PID。
- `accept_helper` 同时等待连接与子进程退出（每 100 ms 查一次句柄），子进程先退出即失败。
- `start` 失败挂 `ELEVATE_FAILED` 并记下，同一会话之后的提权操作直接以同一码失败，不弹 UAC；会话开始的 `Ping` 失败即返回。
- 子进程命令行为 `headless-uac <pipe_id> <log_path> <launcher_pid> [--silent]`；`main` 按 `-S` 设置 `SILENT_SESSION`，带 `--silent` 的子进程不弹崩溃提示、不弹 `ELEVATE_FAILED`。
- 子进程的写任务直接把面包屑写入管道，不经过它自己读的通道。
- 管道上没有事件转发与断连帧：`PipeMsg` 只有 `Progress`、`Ok`、`Err`、`Breadcrumb`、`Resolve`。进度按请求 id 交给 `pending` 中登记的回调，见[原子化文件提交](./2026-09-02-atomic-file-commit.md)的提权管道一节。

### 会话（B11、B12、B18、B20、C1、C2、C4）

- B11：节点解析失败计入当次尝试并继续，第二次起取整文件；混合补丁解析失败落到下一种来源。
- B12：计划只包含扫描到的删除项。已最新判定看"有配置的运行库未安装"（`runtimes::runtime_installed`，与安装前检查共用 `dotnet_spec`、`vcredist_spec`）。
- B18：探测全部失败时 `pick_fastest_test` 返回 `None`，调用方按 `source`、第一个测试地址的顺序回退，与重构前相同。
- B20：提交 95～97，运行库 97，快捷方式 98，注册表 99。
- C1：缺省值与重构前前端相同：`appName`、`regName`、`programFilesPath` 为 "Kachina"，`publisher` 为 "YuehaiTeam"，`exeName` 为 "inst.exe"，`uninstallName` 为 "uninst.exe"，`updaterName` 为 "update.exe"，`title` 为 "Title"，`description` 为 "description"，`windowTitle` 为 " "，`source` 为空串。builder 的 `pack` 仍要求 `windowTitle`、`appName`，与重构前一致。
- C2：自动应答时一律用本地版本。
- C4：保留按区域格式选语言、无匹配取第一列。

### 提交回滚（B1）

复原失败的单元保留在暂存目录中，以 `ROLLBACK_INCOMPLETE` 失败，下次会话从 journal 恢复；规则见[原子化文件提交](./2026-09-02-atomic-file-commit.md)。会话侧 `keeps_staging_for_recovery` 让 DFS 与 Mirror酱 两条提交路径遇到该码时不调用 `finish_staging`。

### Mirror酱 与前端（B3、B4、B16、B17、B22）

- B3：包装目录取 `.metadata.json` 所在位置：在根目录不剥，在一层目录 `X/.metadata.json` 剥 `X/`，没有 `.metadata.json` 不剥。
- B4：状态请求的 URL 用 `url` 拼路径段与 query，所有值百分号编码；`send` / `text` 的错误先 `without_url()`；`capture_anyhow` 与 `capture_panic` 上报前用 `sanitize_urls_in_text` 把文本中的 URL 截到协议、主机与路径。
- B16：CSS 与 HTML 主题下 `.image` 留空，由主题样式绘制；无主题时显示默认插图。渲染规则见[前端重写为 Preact 渲染器](./2026-09-02-frontend-preact-renderer.md)。
- B17：文案句中含 `{items}` 时不另列条目，WebView 与原生两侧同一规则（原生在文案不含 `{items}` 时把条目逐行附在正文后）；`prompt.occupied_files.message` 不含 `{items}`。
- B22：`ProjectView` 带 `app_name`，链接参数为 `?source=` 加 `encodeURIComponent("Kachina" + app_name)`。

### builder 与其他（B13、B14、B15、B21）

- B13：`pack_cli`、`pack` 返回 `Result<(), String>`，所有失败路径都返回错误；builder 打印 `Pack failed: …` 并以 1 退出，删除写了一半的产物。
- B14：`parse_index` 逐项检查剩余长度，见[共享包读取与边写边校验的写出管线](./2026-11-04-pack-reader-and-write-pipeline.md)。
- B15：`estimated_size_kb` 先除后转，超出 DWORD 时取 `u32::MAX`。
- B21：DATA 长于请求即以 `InvalidData` 失败，走既有的请求重试。

### 网络失败分类

下载失败从错误链上的类型分类，同时给出用户码与 DFS insight 的网络失败标签，规则与标签形式见 [UI 契约](./2026-09-02-ui-contract.md)的错误码一节。为此 H3、SSH、SFTP 中间件把原错误作为原因保留，自己的说明作为 context：h3、msquic、russh、hyper 错误保持类型；自建超时为 io `TimedOut`，畸形或截断的 SFTP 报文为 io `InvalidData`；SSH 认证被拒为 `SshAuthRejected`，SFTP 的非 OK STATUS 为 `SftpStatus`。native-tls 为直接依赖，以便按类型识别 TLS 错误。`fs.rs::read_failure` 以同一套类型判断决定是否加 `DECOMPRESS_ERR` context。

下载码的 detail 带网络失败标签，对话框正文里能看到它与系统错误文本。提权子进程的未挂码错误经 `IpcError` 带上分类结果过管道，主进程挂码时得到与未提权路径相同的码与 detail，两种路径的主文案查同一条文案。

### 冗余

- 前端只注册 stub 插件，GitHub 来源只由 Rust 解析。
- `\0IMAGE` 以字节从包读取一路传到 `apply_theme`；`InstallerConfig` 不可序列化，没有 `get_installer_config` 命令，TypeScript 侧没有它的镜像类型；crate 没有直接的 `base64` 依赖。
- 所有 HTTP API 调用经 `API_CLIENT`。

### 测试

- 前端没有组件测试，也没有 vitest、jsdom、`@testing-library/preact`。"界面中字面量形式的 `t('…')` 键都在文案表里"由 Rust 测试 `utils::i18n::tests::web_renderer_keys_exist` 检查。
- 计划回放、`*_is_not_lost`、`pack_embed_matches_legacy_*` 及只为它们存在的类型与 `Deserialize` 派生不存在。
- e2e 的暂存残留断言读安装器日志：会话打开暂存目录时记 `staging opened for "<install>" at "<root>"`，`tests/utils.mjs` 的 `loggedStagingRoots` 按安装目录取出暂存根，`assertStagingRemoved` 在日志里找不到暂存根时失败。

### 文档

注释描述其所在代码。上文列出的 note 与 README 描述现状，[重构审阅问题的修复](./2026-09-20-refactor-review-fixes.md)与[下载规划器](./2026-09-21-download-planner.md)处于 implemented，遥测通道职责收敛与 UI 契约互相链接。`docs/design/` 推迟到这一大版本稳定后再写：重构仍在进行，现在写会让设计文档被反复改成变更流水；在此之前现行行为以 implemented note 为准。

## Alternatives considered

- 并入[重构审阅问题的修复](./2026-09-20-refactor-review-fixes.md)：该 note 的多数条目已实施，状态需要单独整理；本 note 的条目多数不在其范围内，合并会让两份状态混在一起。
- 为每条缺陷补回归测试再修：多数条目修复后在结构上不再可能发生，单独钉住历史缺陷属于无效测试。只为有可观察行为的条目在现有测试夹具中补断言。
- B5 给启动用户一条 ACE：子进程总是提权的，这条 ACE 只会让同一用户的中完整性进程也能连上管道。
- B3 继续按最长公共前缀判断包装目录：只改一个子目录的增量包必然被误剥。`.metadata.json` 是上传包必带的文件，以它的位置判断不受变更范围影响。
- 网络错误并成三类：DFS insight 的值由下载调度器直接聚类，需要细分到原因与协议码才有用；用户看到的码另行保持粗粒度。
- 在现有文本匹配上补模式：响应体错误的 Display 恒为同一句，原因只能从类型取得。
- 在错误对话框脚注另起一行显示网络失败标签：正文的 detail 已经显示整条错误链文本，让 detail 带上标签即可；另设字段要同时改 `Coded`、`IpcError`、bridge 错误载荷与前端类型。
- 删除 e2e 的暂存残留断言而不是改读日志：断言是发现暂存目录泄漏的唯一检查，日志行让它跟随 Rust 侧的路径推导。

## Verification

单测为 `cargo test --bins`（debug，x86_64-pc-windows-msvc）；e2e 为 release bundle（`pnpm build`，target `x86_64-win7-windows-msvc`）上的 `tests/*.mjs` 14 项（prepare、offline-install、online-install、offline-update、online-update、dfs2、updater-survival、already-latest、uninstall、userdata-ignore、occupied-process、builder-extract-replace、plugin-stub、sfx-run），在本机非提权会话中运行，需要提权的 `test:machine-acl` 与 `test:registry` 未跑。GUI 检查以脚本向窗口发消息并截图，安装包为 4000 个 48 KiB 文件的离线包。

| 判据 | 结果 |
|---|---|
| B1：注入回滚失败后，`old\` 与 journal 保留，下次会话完成恢复，安装目录不缺文件 | PASS：`fs::commit::tests::incomplete_rollback_keeps_staging_for_the_next_recovery`（新文件被占用无法复原 → `ROLLBACK_INCOMPLETE`、journal 与 `old\` 保留；解除占用后恢复返回 `Completed`，三个文件为新内容） |
| B2：提权安装在进度洪水与持续日志下完成 | 代码审查：写任务不向自己读的通道发送。未做真实提权安装：本机会话未提权，UAC 同意框无法自动点击 |
| B3：变更文件全在同一子目录的增量包按原路径安装 | PASS：`wrapper_dir_is_where_metadata_sits` 覆盖只改一个子目录的增量包不剥目录 |
| B4：Mirror酱 状态请求失败产生的事件中不出现 CDK | PASS（单测）：`status_url_encodes_every_value`、`urls_in_text_lose_query_and_fragment`。未用真实 Mirror酱 API 验证：没有可用 CDK |
| B5：管道上第二个实例创建失败，非预期进程无法与任一端完成连接 | PASS（单测）：`connection_from_another_process_is_refused`；`pipe_roundtrip_and_disconnect` 带启动进程 PID 跑通。真实 UAC 拉起的子进程能否连上未在本机验证 |
| B6：拒绝 UAC 只弹一次，以 `ELEVATE_FAILED` 呈现且不上报 | 代码审查；本机无法操作 UAC 对话框 |
| B7：子进程退出后启动进程以错误结束会话 | PASS：`helper_exit_before_connecting_fails_the_wait` |
| B8：静默提权安装不出现任何对话框 | 代码审查；CLI 解析单测覆盖 `--silent`。未做真实静默提权安装 |
| B9：拖入文件不改变页面 | 代码审查；GUI 检查中页面正常加载并收到状态，说明 origin 判断放行了起始页 |
| B10：无边框窗口可拖动 | 代码审查：Settings9 开启非客户区支持。未在无边框产品上实测 |
| B11：一次节点解析失败后文件按重试规则继续 | 代码审查：没有本地可控的节点解析失败 |
| B12：已是最新的安装再次运行显示"已是最新"，不重跑运行库 | PASS：v2 fixture（带 `readme.txt` 删除项）静默安装后再运行一次 v2，日志 `already latest, tag=2.0.0`；`deletes_skip_ignored_folder_and_missing_files` 覆盖不存在的删除项不进计划 |
| B13：`pack` 校验失败时退出码非 0 | PASS：release builder `pack -m <不存在的文件>` 退出码 1、无产物；正常 `pack` 退出码 0、有产物 |
| B14 | 见[共享包读取与边写边校验的写出管线](./2026-11-04-pack-reader-and-write-pipeline.md) |
| B15 | PASS：`estimated_size_keeps_installs_over_4_gib` |
| B16 | PASS（GUI 检查）：同一安装包分别不带主题、带 WebP 主题（21,750 字节）、带 CSS 主题（`.image { background: #d00000 }`），就绪页分别显示 SVG 默认插图、WebP 图片、红色色块。zstd 压缩的 HTML 主题未测 |
| B17、B18、B20、B22 | 代码审查 |
| B19 | 未测：本机只有一台显示器 |
| B21 | 代码审查：需要行为异常的 SFTP 服务端。正常与 `-shortread` 两种 `tests/sshd` 服务端下 `capabilities::sftp::tests::ssh_e2e`（ignored，手动运行）通过 |
| C1 | PASS：只含 `source`、`appName`、`windowTitle` 的配置打包后静默运行，越过配置解析，在拉取在线 metadata 时以 `METADATA_UNREACHABLE` 失败（源地址不可达） |
| C2、C5 | 代码审查 |
| C3 | PASS（GUI 检查）：空闲时关窗，进程退出码 0；运行中关窗（3 次）0.5～1.2 s 内出现取消确认，主窗口被禁用，确认后日志 `cancel accepted at stage process_files`、`install cancelled by the user`，目标目录不存在、无注册表记录，窗口回到就绪页；取消中再关出现强制退出确认；原生进度框取消确认属于进度框，取消中再点出现强制退出确认，确认后日志 `forced exit while a session was running`、进程退出 |
| C4 | 保留，不改 |
| Windows 11 21H2 的背景 | 未测：没有 build 22000～22620 的机器 |
| 过度设计删除后功能不变 | PASS：`progress_frames_reach_only_their_request`（真实命名管道：进度只交给对应请求的回调，未知 id 丢弃，结果照常到达）；主题 GUI 检查同 B16 |
| 网络失败按类型分类 | PASS：`utils::net_failure::tests` 经 `create_http_stream` 对本机 TCP 服务器：拒绝端口 → 发送阶段 `NET_CONNECT_REFUSED` / `DOWNLOAD_REFUSED`；503 → `NET_HTTP_503` / `SERVER_HTTP_ERROR`；声明 100 字节只发 3 字节后关闭 → 响应体阶段 `NET_EOF` / `DOWNLOAD_FAILED`；同样后 RST → `NET_RESET`；非法 chunk → `NET_PROTOCOL`；reqwest 包装的响应体中的 `SftpStatus` → `SFTP_STATUS:4`；中间件错误无 context 时的 `SshAuthRejected` → `SSH_AUTH` / `DOWNLOAD_REFUSED`；本地 `StorageFull` 不算网络失败、得 `DISK_FULL`。对 `tests/sshd` 服务端：错误指纹 → `SSH_HOST_KEY`，错误密码 → `SSH_AUTH`，SFTP 文件不存在 → `SFTP_STATUS:2`，SSH 端口拒绝 → `NET_CONNECT_REFUSED`，ssh+http 404 → `NET_HTTP_404`。DNS、证书与 H3 / QUIC 的真实失败未测：本机没有对应服务端 |
| 提权路径与未提权路径的码、detail 一致 | PASS：`code_and_detail_survive_the_elevation_pipe`：拒绝端口的未挂码错误经 `IpcError` 编码成真实管道帧、解码后在主进程挂码，得到 `DOWNLOAD_REFUSED`，detail 与未提权路径逐字相同、以 `NET_CONNECT_REFUSED: ` 开头；去掉 `CarriedFailure` 的还原时该测试失败，得到 `DOWNLOAD_FAILED`。真实提权安装未跑：本机会话未提权 |
| detail 中去掉 URL 后不留空括号 | PASS：`detail_strips_urls`（`for url (…): x` → `for url: x`，`TAG […]: x` → `TAG: x`，去掉 URL 后不留双空格） |
| 无效测试删除后 `cargo test` 与 e2e 通过 | PASS：改坏 `Done.tsx` 的一个键时 `web_renderer_keys_exist` 失败并报出缺失键；在已结束的 plugin-stub 暂存根位置建目录时 `assertStagingRemoved` 报残留，对没有日志行的路径报 "names no staging root" |
| 全部改动后 | PASS：`cargo fmt`、`cargo clippy --all-targets` 无告警；单测 builder 37、installer 217（1 ignored）、sfx 40；`pnpm exec tsc --noEmit` 0 错误；release e2e 14 项通过 |

## Consequences

- 上传包带外层目录、而某次增量包不含 `.metadata.json` 时（发布时没有重新生成元数据），文件按带外层目录的路径装进安装目录。README 要求上传包包含 `.metadata.json`，`gen -t` 的版本号写在其中，每次发布都会变，正常发布的增量包里都有它。
- 错误上报后端仍是明文 HTTP，属于部署侧决定；事件文本中的 URL 只保留协议、主机与路径。
- 提权子进程 PID 取不到时启动进程只记 warn 并放行，此时只靠 DACL（仅提权进程可连）与子进程一侧的 PID 核对。经 UAC 拉起返回的句柄是否带查询权限没有在本机验证。跨账号提权时子进程同样是管理员令牌，DACL 允许，也未实测。
- 导航只放行起始页 origin；插件宿主页与主界面同 origin，`test:plugin-stub` 通过。开发模式（`http://localhost:1420`）未验证。Settings9 在旧运行时上取不到时保持缺省，不报错。
- 强制退出与结束进程同样处理：阶段一只留暂存残留，阶段二留下的 journal 由下次运行恢复。原生取消确认框开在进度框线程的回调里，会话恰在确认框打开时结束时，进度框关闭会连带销毁确认框，按"否"处理（由代码推断，未实测）。
- `pack` 在 rcedit、写临时文件等处仍是 `unwrap`，失败以 panic 退出（退出码 101），不删产物。
- 已安装但残留删除项的目录（删除项指向的文件仍在）会进入一次更新路径，之后判定为已是最新。
- 错误类型不过提权管道，过管道的只有码、文本与网络失败分类结果；其他类型信息（例如本地 io 错误的 kind）在主进程不可见，提权子进程中未挂码的本地文件错误在主进程挂 `DOWNLOAD_FAILED`。
- detail 只剥除 http(s) URL：`sftp://`、`http3://` 地址经 `sanitize_url_for_logging` 去掉 query 与 fragment（含凭据）后，主机与路径仍出现在 detail 中。
- h3-msquic-async 0.3.1 把 msquic 连接错误转成 h3 错误时对状态码 `unwrap()`，遇到未知 msquic 状态会 panic，release 构建 `panic = "abort"` 下进程直接退出。
- TLS 标签里的 OS 码从 native-tls 错误的文本里取（native-tls 不公开 schannel 的 `io::Error`），文本格式变化时只丢 OS 码，标签退为 `NET_TLS`。
- 前端界面没有自动化覆盖，界面改动靠真实窗口检查；文案键的完整性由 Rust 测试守住。
- `docs/design/` 在大版本稳定之前为空，读者从 implemented note 了解现行行为。
