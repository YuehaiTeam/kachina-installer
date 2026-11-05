# 重构审阅问题的修复

Status: implemented

## Problem

审阅基准为重构前的 `ae461aa9ddd5a8f5e445459e14f5d611921f9938`，审阅对象为 `a52a4c66a9e75d86c130be2e029345a761829c24`。本 note 覆盖代码审阅发现，以及测试反馈中的 fallback 报错、取消等待和离线运行库打包失败。安装身份与注册表写入见[根据安装记录决定注册行为](./2026-09-20-install-identity-registry.md)，暂存文件 ACL 随 rename 进入目标的问题见[跨账号提权](../proposed/2026-09-24-cross-account-elevation.md)。下面区分重构引入的问题、未说明的行为变化与旧版本已有的问题。

### 重构引入的问题

| 问题 | 触发条件与后果 | 代码依据 |
|---|---|---|
| fallback 中止掩盖真实错误 | 某个并发任务失败后，另一任务的 fallback 看到 `has_error`，返回普通错误 `merged fallback cancelled`。`join_all` 的结果按任务排列，后续选择的首个错误可能是中止错误，最终按内部错误弹窗，丢失真正失败的原因。该文本本身不能证明用户点击了取消。 | [下载与 fallback](../../../native/session/run.rs)、[错误分类](../../../native/utils/code.rs) |
| 取消等待缺乏反馈，且存在取消后仍提交的窗口 | 界面只禁用按钮；丢弃下载 future 不会取消提权进程已经接收的操作，清理暂存目录前仍需 `wait_idle`。DFS 会话结束请求、扫描和解压也可能延长等待。下载成功后，在结束远端会话或准备自身映像期间收到的取消，没有在提交前统一检查，可能继续完成安装。 | [会话取消与提交](../../../native/session/run.rs)、[提权任务等待](../../../native/ipc/manager.rs)、[取消界面](../../../web/screens/Running.tsx) |
| 嵌入名称限制拒绝受支持的运行库 | 普通条目只接受 ASCII 字母、数字、点、下划线和连字符，而受支持的 `Microsoft.VCRedist.2015+.x64`、`Microsoft.VCRedist.2015+.x86` 含 `+`。运行库按完整 tag 查找条目，改名会使其无法离线匹配。读取器另加了 512 字节上限。 | [追加写入](../../../native/builder/append.rs)、[包格式读取](../../../native/pack_format.rs)、[运行库定义](../../../native/installer/runtimes.rs) |
| 退出清理路径进入 shell 命令 | 退出清理用 `cmd /C ... rmdir /s /q`，参数引用只考虑空格和制表符。合法路径中的 `&` 可改变命令含义，例如没有空格的 `D:\Apps&Games.kachina-staged` 会被拆开。旧版已有 shell 引用不足，但递归删除暂存目录扩大了误删范围。 | [退出清理](../../../native/installer/uninstall.rs)、[命令参数构造](../../../native/utils/process.rs) |

### 重构引入的、未明确说明的功能交互或展示不一致

| 问题 | 触发条件与后果 | 代码依据 |
|---|---|---|
| CDK 验证失败后取消，旧值未恢复 | `handle_set_cdk` 在验证前写入正式 options；关闭输入界面只恢复来源 URI，不能恢复旧 CDK 和验证状态。 | [CDK 提交](../../../native/session/commands.rs)、[来源切换界面](../../../web/App.tsx) |
| 隐藏来源失去临时显示入口 | 来源列表过滤隐藏项，仅保留当前来源；旧版临时显示隐藏来源的交互没有对应入口，也没有明确的取消支持说明。 | [来源列表](../../../native/session/commands.rs)、[前端](../../../web/App.tsx) |

### 旧版本已有的问题

| 问题 | 触发条件与后果 | 代码依据 |
|---|---|---|
| Mirror酱 归档根目录推断错误 | 去掉根目录控制文件后，以其余文件的最长公共前缀推断外层目录。根目录含 `.metadata.json` 和 `changes.json`、变更文件仅有 `Assets/a.bin` 与 `Assets/b.bin` 时，会误把 `Assets/` 当包装目录，漏读 metadata，并剥掉实际应保留的路径。 | [归档前缀与 metadata 查找](../../../native/thirdparty/mirrorc.rs) |
| 无待写文件时跳过其他工作 | `to_install.is_empty()` 提前进入已最新分支，未以删除清单和运行库需求判断是否还有工作，可能漏掉纯删除更新或运行库处理。 | [DFS 会话](../../../native/session/run.rs) |
| 卸载的数据保留选项不覆盖包内初始数据 | `hashed` 中所有路径直接进入删除列表，未按 `userDataPath` 排除；不勾选删除用户数据也会删除随包提供、随后被用户修改的配置。 | [卸载清单](../../../native/session/run.rs) |
| 卸载文件删除错误被丢失 | `rm_list` 的两层异步结果只收集外层任务错误，实际 `remove_file` 的错误未进入返回值；会话收尾也只记录已有错误，仍显示卸载完成。 | [删除结果](../../../native/installer/uninstall.rs)、[卸载结果展示](../../../native/session/run.rs) |

## Decision

### 保留的设计

更新时保留已有 `userDataPath` 文件、补齐缺失的默认文件；非空 `ignoreFolderPath` 跳过更新；运行库安装失败提示用户后允许应用文件安装结束。这些行为有明确用途，不因代码审阅而统一改成失败或覆盖。[计划生成](../../../native/session/plan.rs)与[运行库安装](../../../native/session/run.rs)是对应实现。绿色版不创建注册记录见[按安装记录登记](./2026-09-20-install-identity-registry.md)。

修复遵守[文件提交协议](./2026-09-02-atomic-file-commit.md)：提交阶段不可取消，暂存写入停止后才清理，中断恢复与自更新处理由该协议定义。本 note 只处理进入提交前的取消判定和等待反馈。

### 真实失败与连带中止

[`install_files`](../../../native/session/run.rs) 中任一任务失败即置 `has_error`。尚未开始的任务看到该标记直接跳过；正在执行的合并回退在每个成员开始前检查它，以 `Cancelled` 结束。`collect_ops` 返回第一个不是 `Cancelled` 的错误，与任务在列表中的位置无关；只有全部错误都是取消时才返回取消。

用户取消由会话取消 token 判断，不依赖错误文本。`Cancelled` 不上报，也不按内部错误弹窗；真实失败按自身错误码展示与上报，遵守 [UI 契约](./2026-09-02-ui-contract.md)。

### 取消反馈与提交前判定

运行中的取消状态有三种：`Available`、`Requested`、`Unavailable`（[`CancelState`](../../../native/session/state.rs)）。WebView 路径上，`GuiRuntime::cancel_running` 在会话锁内取消 token、把状态置为 `Requested`，并记日志 `cancel accepted at stage <stage>`。之后的进度更新保留 `Requested`，WebView 与原生界面都显示 `running.cancelling`。

`GuiUi::begin_commit` 持同一把锁判定：token 已取消则返回 false，会话不提交；否则把状态置为 `Unavailable`，之后的取消请求只记日志。原生进度框的 `NativeCancel` 用同样的锁让取消与 `begin_commit` 互斥。

`begin_commit` 由 `commit_staged` 与 `recover_or_discard` 调用；不提交文件的已最新路径（DFS 没有要提交的单元、Mirror酱 版本相同）在写注册表与快捷方式之前也调用它。DFS、Mirror酱 和"已最新但需刷新自身映像"三条路径都在 `self_image_units` 准备完自身映像之后才进入提交，所以准备期间收到的取消同样阻止提交。接受取消后不再启动新的下载任务，并向执行进程发送 `CancelDownload`；执行进程中的下载在等待请求名额、解析 URL 与读取响应期间响应取消。

丢弃暂存目录前先调用 `wait_idle`，等提权侧的在途操作结束，日志记录在途数量与等待耗时；`cleanup_dfs2` 记录结束 DFS 会话的耗时。两条日志区分本地在途操作与远端收尾。

### 嵌入名称与长度

共享规则在 [`embedded_name.rs`](../../../native/embedded_name.rs)，builder 与安装器都编译它（`pack_format::embedded_name`）。`\0CONFIG`、`\0META`、`\0INDEX`、`\0IMAGE`、`\0THEME` 是精确的内部名称例外。其余名称按单个 Win32 文件名检查：允许 `+`、中文、内部空格与括号；拒绝空名称、`.`、`..`、尾随空格或句点、`/ \ < > : " | ? *`、NUL 与 U+0001～U+001F，以及忽略大小写的 `CON`、`PRN`、`AUX`、`NUL`、`COM<数字>`、`LPT<数字>` 及其带扩展名的形式。

普通 TLV 条目头的名称长度字段是大端 `u16`，上限 65,535 字节；`\0INDEX` 内的名称长度字段是 `u8`，上限 255 字节。读取器按字段长度、剩余文件长度、UTF-8 合法性和名称规则解析，不另设上限。`pack` 在创建输出前用 `validate_pack_names` 检查全部名称，写条目头时以 `u16::try_from` 转换长度，写索引前检查 255 字节上限；`append` 在打开输出前检查每个名称。运行库按完整 tag 查找嵌入条目，`Microsoft.VCRedist.2015+.x64` 这类 tag 原样保存。

### 退出清理

退出时删除暂存根的是 [`spawn_staging_cleanup`](../../../native/installer/uninstall.rs) 启动的 `%SystemRoot%\System32\cmd.exe`。路径不进命令行，而是放在环境变量 `KACHINA_CLEANUP` 中，命令以 `"%KACHINA_CLEANUP%"` 引用；Windows 路径不能含 `"`，所以路径中的 `&` 等字符不会被 cmd 当作语法。目录存在时每隔约 1 秒尝试一次 `rmdir /s /q`，共 20 轮。

待删路径只有 `schedule_delete_on_exit` 一个写入点，内容是本次会话记录的暂存根；`delete_self_on_exit` 幂等。清理进程的映像在 System32，不在待删目录内。

### CDK 编辑

`handle_set_cdk`（[`session/commands.rs`](../../../native/session/commands.rs)）在验证前只把 CDK 状态置为 `Checking`，并记下最近一次已确认的状态，正式 options 不变。验证结果经 `commit_verified_cdk` 提交：先后持有 `cdk_restore` 与会话锁，世代号不一致时什么都不改；成功时一起写入来源、CDK、`Ok` 状态与 Windows 凭据，失败时只把状态置为 `Invalid`。

取消编辑或切换来源时，`restore_abandoned_cdk` 递增世代号并恢复记下的状态，迟到的验证结果因此不能覆盖新来源。空 CDK 直接提交并删除凭据。WebView 中点击 Mirror酱 卡片只打开 CDK 面板，确认后才发 `set_cdk`，取消时发 `cancel_cdk`；原生界面先验证再应用。

### 隐藏来源

后端把全部来源连同 `hidden` 标记发给界面。WebView 的[来源面板](../../../web/panels/SourcePanel.tsx)默认不列 `hidden` 来源，当前选中的隐藏来源除外；面板打开期间连续按 5 次 `,`、相邻两次间隔不超过 2 秒后列出全部来源，关闭面板即恢复默认。原生界面列出非隐藏来源与当前选中的来源，没有临时显示入口。

### Mirror酱 归档路径

[`archive_wrapper_prefix`](../../../native/thirdparty/mirrorc.rs) 以 `.metadata.json` 所在的目录为包装目录：位于归档根目录时不剥前缀，位于唯一一层顶级目录（如 `App/`）时剥除该目录；没有 `.metadata.json` 时不剥。`changes.json` 只在归档根目录读取，其中的删除项按同一前缀剥除。Mirror酱 后端生成增量包时保留上传时的条目路径（Mirror酱 后端 `internal/pkg/patcher/patcher.go`），而 `.metadata.json` 每个版本都变化，增量包中同样包含它。

### 已最新判定

`dfs_staged` 只在没有待写文件、计划删除项为空且 `runtimes_missing` 为假时进入已最新路径。`build_plan` 只保留扫描时实际存在的删除项。`runtimes_missing` 对每个配置的运行库调用 `runtime_installed`，无法判断的 tag 按缺失处理。其余情况走"无文件下载、执行删除与运行库"的分支，同样经过提交单元生成、提交、运行库安装与收尾。

### 卸载保留用户数据

未勾选删除用户数据时，`uninstall_files` 从 `hashed` 与 `deletes` 合成的删除清单中去掉位于任一 `userDataPath` 目录下的路径（`is_under`），包内初始数据与运行期生成的数据按同一规则保留。勾选后，这些目录整体交给卸载删除。

### 卸载删除错误

`rm_list` 收集每个文件删除的实际结果，文件不存在视为已删除；`remove_paths` 与空目录清理在单项失败后继续。会话再追加快捷方式与各注册表位置的删除失败；有任何失败时逐条记日志，以 `UNINSTALL_INCOMPLETE` 提示，然后照常进入卸载完成，与 Inno Setup 的 `UninstalledMost` 相同。

## Alternatives considered

- 仅把 `+` 加入字符白名单：能修复当前运行库名称，仍会拒绝中文、空格等合法名称，也无法解决 512 字节限制及读写规则分散。
- 将全部名称长度统一为 255 字节：实现简单，但把索引的格式限制施加给未索引条目；将索引长度字段扩为 `u16` 则破坏生产格式。保留现有两种编码，只统一各自的校验来源。
- 把 `merged fallback cancelled` 统一映射成用户取消：会隐藏其他任务的真实失败；只隐藏弹窗同样无法恢复错误原因。
- 取消时立即返回首页或直接结束提权进程：在途任务可能继续写入，或破坏同时承担收尾工作的执行进程。保留停止写入后清理的顺序，先反馈取消状态。
- 为缺少 metadata 的 Mirror酱 归档强制报错，或把增量归档内容当完整文件清单：前者取消现有支持，后者丢失未变更文件的卸载信息。登记兼容策略见[按安装记录登记](./2026-09-20-install-identity-registry.md)。
- 卸载部分失败时保留注册记录并回到首页供重试：运行中的卸载器已移入暂存目录并在退出后删除，保留的记录指向不存在的卸载入口，关闭窗口后无法再次卸载。NSIS 与 Inno Setup 的卸载同样不回滚，照常删除记录。
- 为所有来源、提权方式、取消时点和文件名增加 e2e 组合：维护成本高，且实际网络、调度和 UAC 易导致偶发失败。匹配规则和时序分支使用确定性单测，操作系统权限另用少量隔离 Windows 场景验证。注册表链路验收见[按安装记录登记](./2026-09-20-install-identity-registry.md)。

## Verification

| 判据 | 结果 |
|---|---|
| `+` 运行库 tag、中文和空格名称可读写；TLV 与索引的字段宽度、编码和排列不变；超限错误在修改输出前发生并说明条目和字节限制 | PASS：`embedded_name.rs` 的 `allows_win32_file_names`（含两个 VCRedist tag、中文、空格）、`rejects_separators_reserved_chars_and_controls`、`rejects_device_names_and_extensions`、`internal_names_are_exact_exceptions`、`tlv_and_index_length_limits`；`builder/pack.rs` 的 `tlv_name_over_512_bytes_round_trips`、`index_rejects_names_over_255_bytes`。名称检查先于创建输出是代码事实，没有测试；没有把 `+` tag 追加进包后由安装器找到的端到端测试 |
| 连带中止不替代真实错误；用户取消不作为内部错误上报 | PASS：`collect_ops_prefers_the_real_error_over_cancellation`（取消与真实错误混排、只有取消两种情况）、`cancelled_chunk_cancels_the_merged_request`、`utils/code.rs` 的 `extract_three_states`。合并回退内部的连带中止没有单测 |
| 取消先被接受时不提交；进入提交后拒绝取消；取消状态立即可见，后续进度不覆盖 | PASS：`accepted_cancel_blocks_commit`、`cancel_after_commit_is_ignored`（`GuiRuntime` 与 `GuiUi`）、`later_progress_keeps_a_requested_cancel`。GUI 探针（release 构建，4000 个 48 KiB 文件组成的 200 MB 离线包，本机 Windows）：WebView 中确认取消后日志依次为 `cancel accepted at stage process_files`、`install cancelled by the user`，目标目录与注册记录都不存在，窗口回到就绪页；原生进度框确认取消后日志为 `cancel accepted from progress dialog`。`NativeCancel` 没有单测 |
| 暂存目录删除发生在写入任务结束之后 | PASS：`ipc/manager.rs` 的 `wait_idle_waits_for_dropped_request_result` |
| Mirror酱 根目录控制文件与子目录路径正确识别 | PASS：`wrapper_dir_is_where_metadata_sits`（根目录 metadata 加 `Assets/*`、只有 `changes.json` 与 `Assets/a.bin`、`pkg/` 包装目录、更深层或改名的 metadata，以及一次内存 zip 解压）、`extracts_into_new_dir_and_reports_deletes`、`metadata_variant_and_invalid_archives` |
| 清理路径中的 shell 特殊字符不改变删除范围 | PASS：`staging_cleanup_deletes_ampersand_path_and_spares_sibling`（真实 cmd，路径 `应用 & Games.kachina-staged`，同级文件保留）、`staging_cleanup_retries_until_lock_released`、`utils/process.rs` 的 `env_block_overrides_and_keeps_ampersand` |
| 取消 CDK 编辑保留原有效配置；成功才提交；迟到结果不覆盖新来源 | PASS：`cancel_after_failed_verify_restores_committed_cdk`、`retry_keeps_the_original_committed_snapshot`、`stale_verify_does_not_commit_source`、`fresh_verify_commits_source_and_ignores_a_later_cancel`。WebView 中卡片不直接提交来源是代码事实，没有自动化测试 |
| 隐藏来源的行为有明确结论 | 结论见 Decision：WebView 用 5 次 `,` 临时显示，原生界面不提供该入口。后端保留 `hidden` 标记由 `visible_sources_keeps_hidden_entries` 覆盖；WebView 入口没有自动化测试 |
| 纯删除更新不被当作无事可做 | PASS：`units_deletes_when_nothing_to_install`、`deletes_skip_ignored_folder_and_missing_files`、e2e `test:already-latest`。`runtimes_missing` 与分支选择没有单测，纯删除更新没有 e2e |
| 卸载保留用户数据，并报告实际删除失败 | PASS：`uninstall_files_keeps_paths_under_user_data`、`uninstall_files_includes_user_data_when_not_kept`、`rm_list_missing_is_ok_locked_is_err`、`uninstall_continues_past_a_locked_file`、e2e `test:uninstall`（安装后新建的 `User/` 文件默认保留）。`UNINSTALL_INCOMPLETE` 提示与失败后的注册记录删除没有会话级测试 |
| 提权卸载时安装目录中一个文件被占用 | 人工观察，环境未记录：其余文件、快捷方式与注册记录照常删除，以 `UNINSTALL_INCOMPLETE` 提示后正常结束，卸载器所在的暂存目录在卸载进程退出后删除 |
| 现有 e2e | PASS：`pnpm build` 产出的 release 包（`x86_64-win7-windows-msvc`），本机非提权会话逐个运行 14 个脚本（含 `test:prepare` 与 `test:sfx-run`）。`test:machine-acl` 与 `test:registry` 需要提权会话，本机未运行 |

## Consequences

- 连带中止与用户取消共用 `Cancelled` 类型，两者只靠 `collect_ops` 优先真实错误来区分。
- 结束会话的请求（DFS2 的 DELETE 与插件来源的 `endSession`）整体限时 10 秒，超时只记日志，这次会话的 insight 可能送不到。
- 扫描与解压按请求的停止 token 响应取消（提权时会话发带请求 id 的 `Cancel`）；解压停下时写出的文件留在 `new\`，随暂存目录删除。
- 退出清理依赖 cmd 轮询。父进程退出后仍有进程占用暂存目录中的文件超过约 20 秒时，目录留在原处，由下一次运行打开暂存时清理。
- `append` 遇到不合法的名称以 panic 退出。
- Mirror酱 上传时带包装目录、而某个增量包不含 `.metadata.json` 时，文件装到包装目录之下。
- 隐藏来源的临时显示入口只在 WebView 中存在。
