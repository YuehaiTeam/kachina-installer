# 文件提交协议：暂存目录、两阶段提交与目录单元

Status: implemented

## Problem

安装器向安装目录写文件有三条路径，安全性不一致，自更新在失败时会删掉更新器的最后一份拷贝，中断会留下混合版本，而计划阶段对目标目录里不属于本应用的文件一无所知。

**三条写入路径。**

- 直写：`ipc/install_file.rs` 的 `InstallFileMode::Direct` 与 `install_file_by_reader` 调 `fs.rs::create_target_file`，即 `File::create(target)`——先截断目标再写入。下载中断、进程被结束、断电都会留下半截目标文件，旧版本已不存在；随后的哈希校验在同一个被截断的文件上失败。
- patch：`fs.rs::progressed_hpatch` 写 `<target>.patching`，成功后 `rename(target → .old)`、`rename(.patching → target)`、`remove(.old)`。三步之间没有回滚：第二个 rename 因共享冲突失败时目标缺失、只剩 `.old`。
- Mirror酱：`thirdparty/mirrorc.rs` 的 zip 解压对每个文件 `File::create(out_path)` 直写；自更新另有一套 `.instbak` 改名。zip 下载到安装目录内（`session/run.rs::run_mirrorc` 的 `join_install(install_path, "KachinaInstaller_Mirrorc_<sha256>.zip")`），失败时留在用户目录；API 返回的 `sha256` 只用于拼文件名，下载内容从未与之校验。
- 删除：DFS 路径的 `plan.deletes` 经 `IpcOperation::RmList` 直接 `remove_file`，结果被丢弃；Mirror酱 路径在解压后按 `changes.json` / `.metadata.json` 的清单 `remove_file`。两处删除都不可撤销。

**自更新失败即丢更新器。** 目标是自身时 `fs.rs::prepare_target` 先 `rename(updater.exe → updater.instbak)`，并在同一处把 `installer/uninstall.rs` 的 `DELETE_SELF_ON_EXIT_PATH` 设为 `.instbak`；此刻磁盘上没有 `updater.exe`。随后 `progressed_hpatch` 以 `.instbak` 为旧文件、`updater.patching` 为输出。任何失败（网络中断、hpatch 返回非 1、`RUN_HPATCH_ERR`）只删 `.patching` 并返回错误，不把 `.instbak` 改回原名。用户关窗时 `host/mod.rs` 主循环对 `UiAction::Close` / `WM_QUIT` 调 `delete_self_on_exit()`，按第一步设下的路径 `del /f /q updater.instbak`。更新器与备份同时消失。同一根因的变体：patch 成功但哈希不匹配，留下坏的 `updater.exe` 而 `.instbak` 照删；`Direct` 模式自更新留下半截 `updater.exe` 而 `.instbak` 照删。曾有线上事故表现为更新失败后更新器消失且无备份文件，与此路径一致。

**中断留下混合版本。** 安装模型是"任意本地状态 → metadata 描述的文件集合"，每个文件装完即生效。更新进行到一半被关闭、断电或结束进程，安装目录就处于一部分文件是新版、一部分是旧版的状态；应用没有自检能力，这种状态下启动大概率崩溃。用户重跑安装器可以修复，但在此之前应用不可用，而用户并不知道要重跑。

**计划看不见不受管文件。** `session/run.rs::scan_local` → `IpcOperation::CheckLocalFiles` → `fs.rs::check_local_files` 只按 metadata 的文件清单逐个 `metadata()`，从不 `read_dir`；目标目录里不在 metadata 中的文件（用户数据、日志、用户自行放入的文件）计划完全不知情。仓库中仅有的 `read_dir` 都不是扫描：`run.rs` 对 `ignoreFolderPath` 只取首个条目判非空，`fs.rs::is_dir_empty` 供路径选择器，`uninstall.rs` 供删除。

**临时目录逻辑分散且以失败反推。** `installer/uninstall.rs::run_uninstall` 自卸载时 `rename(exe → %TEMP%\kachina.uninst.<unix 秒>.exe)`，失败即假定跨卷改挪到安装目录父目录——安全软件锁文件、权限不足都会被误判为跨卷；文件名以秒为粒度可撞名；它是 `DELETE_SELF_ON_EXIT_PATH` 的第二个写入点。`host/mod.rs`、`host/native.rs`、`session/run.rs::silent_main` 各有一份 `set_current_dir(%TEMP%)` 与各自的错误呈现——进程 cwd 会锁住所在目录，这一步是安装目录能被 rename 与删除的前提，不只是探测。`installer/runtimes.rs` 与 `module/wv2.rs` 以固定文件名直接下载到 `%TEMP%` 根下，失败时不清理。

**进行中不能取消。** `Intent::Cancel` 在会话层是空操作，`Cancelled` 只由用户拒绝确认的路径构造。在"装完即生效"的写入模型下取消等价于留下混合版本，因此一直没有做；写入进暂存目录后，阶段一的取消只是丢掉暂存目录。

**提权管道进度洪水。** `ipc/manager.rs` 的 `ManagedElevate::run` 用一个容量 100 的 `broadcast` 同时承载进度与结果，`recv()` 返回 `Lagged` 时 `while let Ok` 退出循环、整次操作报 `IPC_ERR`。文件数多、主线程慢时进度消息可以填满通道。

## Decision

### 暂存目录

每次会话一个暂存目录，位置由安装路径确定性推导，两次运行之间可以重新找到：

- 与目标同卷时：`%TEMP%\kachina-staged\<h>\`，`<h>` 为安装路径经 `session/plan.rs::normalize_full` 规范化后 sha256 的前 16 个十六进制字符。用户在安装目录旁看不到任何暂存文件。
- 不同卷时（`GetVolumePathNameW` 对两侧取卷根比较，不存在的路径取最近存在的祖先）：`<安装目录>.kachina-staged\`，例如 `D:\app\someapp.kachina-staged\`。rename 必须同卷才是原子的，跨卷会退化成复制，所以不能用 `%TEMP%`。

盘根不是安装目录：`installer::probe_dir` 对 `C:\` 这类路径返回 `None`，于是 `Settings` 的两个构造点（GUI `Start`、CLI `-D`）、注册表里的旧路径、打包配置的默认路径、自定义 HTML 发来的 `set_path` 在会话开始前都以 `INSTALL_PATH_INVALID` 拦住，就绪页把它显示为不可写；目录选择器（`pick_install_path`，GUI 与 native 共用）把选中的盘根无条件改成 `<盘根>\<app_name>`。装在盘根上的旧安装既不能升级也不能卸载。`staging_root` 对没有父目录的路径挂的 `INSTALL_PATH_INVALID` 因此只是兜底。

目录布局：`new\<相对路径>` 放产出文件，`old\<相对路径>` 放被换下的旧文件（根目录单元的旧目录放 `old\~root`），`dl\` 放运行库安装器与 Mirror酱 归档，`journal` 是提交清单，`lock` 内含持有进程的 pid。安装目录内不再出现任何安装器自己的文件。

`fs/staging.rs` 承载全部临时目录逻辑：`same_volume`、`staging_root`、`free_space`（`GetDiskFreeSpaceExW`）、`path_hash`、`Staging::open`、`enter_neutral_cwd()`（把进程 cwd 切到 `%TEMP%`，失败挂 `TEMP_DIR_UNAVAILABLE`）、`scratch_file(name)`（WebView2 引导器在任何暂存目录存在之前下载，落在 `%TEMP%` 根下）。`host/mod.rs`、`host/native.rs`、`silent_main` 与提权侧 `uac_ipc_main` 都调 `enter_neutral_cwd`，不依赖 cwd 继承。自卸载改为 `installer/uninstall.rs::park_self`：`rename(exe → <staging>\old\<uninstall_name>)`，同卷由 `staging_root` 保证，不再以 rename 失败反推跨卷；`run_uninstall` 把暂存根路径作为 `UninstallOutcome.self_moved_to` 交回会话。运行库安装器下载到 `dl\`，随暂存目录一起清理。

`Staging::open(install_dir)` 按"同级、`%TEMP%`"顺序查看两个候选位置：某个候选的 `lock` 内 pid 存活（`OpenProcess` + `GetExitCodeProcess == STILL_ACTIVE`）且不是本进程 → 挂 `STAGING_IN_USE`（E 类、不上报，文案"另一个安装程序正在处理此目录"）；第一个含 `journal` 的候选保留，其余候选整目录删除；没有可保留的就在 `staging_root` 建新目录；最后写入本进程 pid 到 `lock`，返回根路径与 journal 原文。它以 `IpcOperation::OpenStaging(install_dir)` 运行在有写权限的一侧（需要提权时是提权进程），`Commit` / `Recover` / `DiscardStaging` 同理；主进程只拿到根路径，用它拼 `new\` 下的输出路径。

打开时机在 `Intent::Start` 之后、本次 metadata（或 Mirror酱 API 应答）已知之后：恢复核对要比对 journal 记录的目标内容与本次要安装的内容，就绪页之前拿不到后者；不改路径点开始就是默认路径，改过路径再点开始查的是新路径，两种情形同一处代码。暂存目录不跨越两次会话存活：恢复要么当场完成要么当场丢弃；会话结束（成功、失败、取消、已是最新）时 `DiscardStaging` 整目录删除，唯一例外是本次换掉了运行中的 exe（见下）。

### 统一写入

所有产出文件——直写、patch 输出、HybridPatch、`install_file_by_reader`、Mirror酱 zip 解压的每个文件——一律写到 `new\<相对路径>`，由 `fs.rs::create_staged_file` 创建。`InstallFileArgs.target` 是 `new\` 下的输出路径，新增 `old: Option<String>` 指向安装目录里当前的文件，只有 `Patch` 模式使用它作为基文件。`install_file.rs::finalize_staged` 在 `new\` 下完成 `clear_index_mark`（打包配置的 installer 项，或目标就是当前运行的 exe）、`verify_hash` 与 `File::sync_all()`（`FlushFileBuffers`），任一失败删该文件并返回错误，目标从未被触碰；刷盘是因为 rename 的原子性只在元数据层，NTFS 不记录数据日志，不刷盘则阶段二之后立刻掉电会换上零长度或半截的文件。`progressed_hpatch(old_path, diff, diff_size, out_path, on_progress)` 只读旧文件、写 `out_path`，没有改名逻辑；HybridPatch 先把内嵌基文件解到 `<out>.hybrid-base`，patch 完删除。`.patching` / `.patchold` / `.old` / `.instbak` / `override_old_path` / `prepare_target` / `create_target_file` 不存在。

### 卸载器与更新器

卸载器（`uninstall_name`）与更新器（`updater_name`）是安装器自身生成的两个文件，不在 metadata 里，此前由 `finish_install` 在提交之后以 `File::create` 直写进安装目录。现在它们是阶段一的产出：`IpcOperation::StageSelfImage { install_dir, new_dir, hash_algorithm, names, copy_from }` 在提权侧把镜像写进 `new\<名>`（`copy_from` 为空时取 `local::get_base_with_config` 即自身 `base + config`，并清索引标记；否则复制 `copy_from` 指向的文件）、算哈希、与安装目录里现有同名文件比对，返回每个名字的 `(hash, changed)`；会话对 `changed` 的名字各出一个文件单元并入同一份 journal，随主提交换入，未变的直接删掉暂存副本。`already_latest` 路径没有主提交，需要刷新时单独跑一次只含这些单元的提交。`journal_matches_target` 对这两个名字放行——它们不在 metadata 的期望哈希里，恢复时按 journal 自己记录的哈希核对。

生成 / 刷新的规则按会话情形分：

| 情形 | 更新器 | 卸载器 |
|---|---|---|
| 全新安装 | 自身镜像生成 | 自身镜像生成 |
| 更新，`hashed`（Mirror酱 为归档文件清单）含 updater | 以清单里那份为准，不自己生成 | 已存在 → 以清单里那份 updater 镜像刷新（`copy_from` 指向 `new\` 下的新文件，或未变时安装目录里的现有文件）；不存在 → 不动 |
| 更新，清单不含 updater，运行的是安装目录里的 updater 本身（`is_current_exe(<install>\<updater_name>)`） | 不动：没有比它自己更新的镜像 | 已存在 → 自身镜像刷新；不存在 → 不动 |
| 更新，清单不含 updater，运行的是外来安装包（在线 stub 或打包安装器，`base + config` 与 updater 是同一种东西） | 自身镜像覆盖 | 已存在 → 自身镜像刷新；不存在 → 不动 |

更新时不补回被用户删掉的卸载器；`installer` 元数据推入 `hashed` 的既有条件（`is_update && installer.is_some() && 无内嵌 metadata && hashed 里没有 updater`）不变，推入后即落入"清单含 updater"一行。

### 计划阶段：一趟目录枚举

`fs.rs::check_local_files` 不再逐文件 `metadata()`，而是在 `spawn_blocking` 里对含受管文件的目录 `read_dir` 一趟（`ScanWalk::walk`），受管文件的大小从条目取，同一趟得到每个目录的干净标记。返回 `LocalScan { files, dirty_dirs, reparse_dirs }`，`dirty_dirs` 是相对路径小写 `/` 形式、`""` 为根：

- 文件条目：受管则记 stat；不受管即标本目录脏。
- 子目录条目：是 reparse point（`is_symlink()` 或 `FILE_ATTRIBUTE_REPARSE_POINT`）→ 标父目录脏、记入 `reparse_dirs`、不进入，其下受管文件逐个 `metadata()`；metadata 中没有任何受管文件（含空目录）→ 标父目录脏、不进入；其下受管文件全部在 `skip_hash` 内（`userDataPath` / `ignoreFolderPath`）→ 标父目录脏、不进入、其下文件逐个 stat；否则进入，子目录脏则父目录脏。
- 父目录脏不代表子目录脏：`node_modules\` 根下一个用户文件不妨碍 `node_modules\foo\` 干净。安装目录不存在时返回空 `LocalScan`，即根目录干净。

候选目录在会话侧由计划算出（`session/run.rs::build_units`）：对 metadata 里出现的每个目录（含根），其下全部受管文件都是 `PlanAction::Install`、且不在 `dirty_dirs`、且没有 reparse 子树下的文件 → 候选；取最靠上的候选为目录单元。全新安装（目标目录不存在）时根目录是唯一候选；普通更新（散落几十个文件变更）候选集为空。哈希阶段不变，仍是读盘瓶颈。

reparse point 子树内的受管文件成为**复制单元**：阶段一仍写到 `new\`，阶段二在链接目标所在卷内完成单文件提交——复制到目标同目录的 `<名>.kachina-tmp`、按 journal 算法校验、`rename(目标 → <名>.kachina-old)`、`rename(tmp → 目标)`，回滚用 `.kachina-old`，成功后删除两者。这是唯一会在安装目录内出现临时文件的情形，只发生在用户自行建立的链接之下。

### 两阶段提交

**阶段一（写入）**：对计划中的全部文件只做写入、后处理、校验，不 rename。此阶段的任何失败、取消或中断（网络、哈希、磁盘满、用户取消、进程被结束）发生时安装目录未被修改，应用仍是完整旧版；残留只在暂存目录里，取消与失败当场删除，进程被结束的留到下次会话开始时删除。

**取消**：`SessionUi::cancel_token()` 交出一个 `CancellationToken`；`LiveUi::check_cancel` 在 metadata、哈希扫描、占用确认、下载前后各检查一次，`install_files` 的下载循环 `select!` 它——命中即丢弃整个 `join_all`，在途流随之中止；Mirror酱 下载同样 `select!`。`run_install` 把 `Cancelled` 错误转成 `SessionResult::cancelled`，相位回 `Ready`，暂存目录删除。阶段二不检查 token。渲染器：WebView 的 `Running` 屏幕在 `commit` / `finalize` / `shortcut` / `registry` / `install_done` 之外的阶段显示取消按钮，发 `Intent::Cancel`，`handle_intent` 触发 `GuiRuntime.cancel`（每次 `Start` 换新 token）；native 进度 TaskDialog 以 `ProgressDialog::show_with_cancel` 打开，Cancel 按钮触发 token 并保持对话框打开，直到会话收尾关闭它，此时不设 `TDF_ALLOW_DIALOG_CANCELLATION`，没有关闭框；silent 无取消。

**阶段二（提交）**（`fs/commit.rs::commit`，`IpcOperation::Commit(CommitArgs { staging_root, install_dir, journal })`）：开始前对所有目录单元**重新探测**一次（`dir_is_clean`：目标目录内只有该单元列出的文件、无 reparse point），探测与提交之间隔着整个下载，用户可能已在被判干净的目录里放了文件，变脏的单元降级为逐文件单元。然后写 `journal` 并 `sync_all`——制表符分隔，首行格式版本 `kachina-journal 1`，第二行 `hash\t<算法>`（DFS 路径为 metadata 的哈希算法名，Mirror酱 路径为 `md5`），Mirror酱 路径再一行 `archive\t<sha256>`，其后每行一个单元：`file\t<相对路径>\t<旧哈希|->\t<新哈希>`、`dir\t<相对路径>`（其下文件各占一行 `file`，按前缀归属）、`del\t<相对路径>\t<旧哈希|->`、`copy\t<相对路径>\t<旧哈希|->\t<新哈希>`，按提交顺序：目录单元、文件与复制单元、删除单元。新哈希在写 `new\` 校验时已算出（Mirror酱 路径在解压时算 md5）；旧哈希来自计划阶段的扫描，Mirror酱 路径没有扫描，记 `-`——再逐单元：

- 删除单元：`rename(目标 → old\<相对路径>)`，目标不存在则跳过。删除因此可回滚、进 journal、失败不再被丢弃；`IpcOperation::RmList` 不存在。
- 目录单元：目标目录存在则 `rename(目标 → old\<相对路径>)`，再 `rename(new\<相对路径> → 目标)`。第一个 rename 因内部文件被占用而失败时，不放弃该单元，退化为逐文件提交：`new\<相对路径>\` 下每个文件按文件单元规则移入目标目录。
- 文件单元：目标存在则 `rename(目标 → old\<相对路径>)`，再 `rename(new\<相对路径> → 目标)`。
- 复制单元：按上节规则在链接目标所在卷内完成。
- 根目录单元：目标不存在 → `create_dir_all(父目录)`，`rename(new → 安装目录)`，一次操作；目标存在且为空 → `rmdir(安装目录)`，再同一个 rename；目标非空且干净 → 与普通目录单元相同。
- 每次 rename 遇 os error 32（共享冲突）、33（锁冲突）、5（拒绝访问）时以 50、100、200、400、800 ms 退避重试五次——安全软件扫描新写入文件的窄窗口就在这里。
- 全部单元完成后删除 `journal`，返回 `CommitOutcome { self_replaced }`（任一单元的目标是当前运行的 exe）。进度以 `stage` 键 `commit` 推送（`done` / `total` 为单元计数，文案 `progress.commit`），恢复前滚复用同一键。

阶段二只有本地 rename，是唯一存在混合状态的窗口，长度为单元数次 rename。目录单元把全新安装的窗口缩为一次操作，把整目录替换（node_modules 类）从 3N 次缩为两次。

**阶段二内的失败**：某个 rename 在重试后仍失败，把已换过的单元按逆序复原（`rename(目标 → new\…)`、`rename(old\… → 目标)`，根目录为空被 `rmdir` 的重建空目录），删除 journal 与整个暂存目录，挂 `FILE_IN_USE`（其它 io 错误按 `code_for_local_io` 映射）、subject 为相对路径；安装目录回到完整旧版。`ProbeWritable` 与占用提示在阶段一之前已经排除了已知占用，此路径应当罕见。

**会话收尾**：`Commit` 成功后运行库安装（`dl\`）、快捷方式、注册表照旧（卸载器与更新器已在提交内换入，`finish_install` 不再写它们）；随后 `finish_staging`：`self_replaced` 为假 → `DiscardStaging` 同步删除暂存目录；为真 → `installer/uninstall.rs::schedule_delete_on_exit(暂存根)`，旧 exe 停在 `old\` 里运行中不可删，`delete_self_on_exit` 在退出后 `rmdir /s /q` 整个暂存目录。`schedule_delete_on_exit` 是 `DELETE_SELF_ON_EXIT_PATH` 全库唯一的写入点，会话在提交（或恢复）成功之后与自卸载完成之后各调用一次。

**磁盘空间**：阶段一结束时峰值为现有安装加本次变更文件总大小。计划阶段以变更总量对比暂存目录所在卷的可用空间（`ensure_space`），不足时在写入任何文件之前以 `DISK_FULL` 失败，文案"磁盘空间不足，无法完成安装/更新"；不做逐文件即时提交的退化路径——退化路径会重新引入中断留下混合版本的窗口，且需要一套界面提示。Mirror酱 路径在下载前不知道变更总量，不做此检查。

### `old\` 的产生与寿命

`old\` 只在阶段二产生，阶段一不触碰安装目录。每个单元放入新文件之前，目标已存在的才 rename 进 `old\`；新增文件、不存在的根目录、为空的根目录（`rmdir`）都没有备份。删除单元的"删除"就是 rename 进 `old\`。复制单元不进 `old\`，在链接目标所在卷内改名为同目录的 `<名>.kachina-old`。自卸载时运行中的卸载器 rename 进 `old\`，是唯一发生在阶段二之外的写入。

用途有三个：同一进程内阶段二失败时按逆序复原；恢复前滚途中再次失败时把上次与本次已换的单元一起复原（见下）；停放运行中的 exe（更新器、卸载器）——运行中的镜像只能改名不能删除。核对不通过时整个暂存目录连同 `old\` 一起删除，核对通过且前滚成功时也不需要它。

寿命上限是一次进程：会话结束时随暂存目录同步删除；`old\` 里停着本进程的 exe 时延到退出，由 `delete_self_on_exit` 的 `rmdir /s /q` 一起带走。每个单元为此多付一次同卷 rename，没有数据复制。

### 恢复

会话在本次 metadata 已知之后、哈希扫描之前，若 `OpenStaging` 带回 `journal` 原文，`session/run.rs::recover_or_discard` 先做两层核对，核对是全有或全无的：

- **核对目标**（会话侧，`fs/commit.rs::journal_matches_target`）：`Journal::parse` 失败或格式版本不等 → 丢弃；`hash` 行算法与本次不同 → 丢弃；DFS 路径：每个 `file` / `copy` 单元（含目录单元内的文件）的新哈希等于本次 metadata 对同一路径要求的哈希，每个 `del` 单元仍在本次删除清单中；Mirror酱 路径：`archive` 行等于 API 本次返回的 sha256。任一不等——服务端已发新内容、该文件已从清单移除、哈希算法变了——都是"目标已变"，`DiscardStaging` 后重新 `OpenStaging` 得到空目录，继续本次会话：上次中断的那次更新已不是这次要装的东西，前滚只会先换上旧目标再被本次覆盖。这个项目没有版本概念，安装模型是"任意本地状态 → metadata 的文件集合"，目标的身份就是这组哈希。
- **核对目录**（提权侧，`IpcOperation::Recover(CommitArgs)` → `fs/commit.rs::recover`）：逐单元哈希当前目标。文件与复制单元：当前哈希等于新哈希为"已换"；等于旧哈希（或目标缺失而旧哈希为 `-`）且 `new\` 下有替换文件为"待换"，替换文件缺失为"不可完成"；其余为"有变"。删除单元：目标缺失为"已换"，哈希等于旧哈希为"待换"，其余"有变"。目录单元：目标目录内文件集合与哈希整体等于新集合为"已换"，等于旧集合（目标不存在且旧集合为空亦然）为"待换"或"不可完成"，多一个文件、少一个文件、任一哈希不符、含 reparse point 都是"有变"。旧哈希为 `-` 的单元只有"已换"能被证明，尚未换即视为"有变"。
- **任一单元有变**：用户在两次运行之间改动了目录（覆盖安装了便携版、手动替换了文件），上次那次更新的前提不再成立，不做任何 rename，删除整个暂存目录，返回 `RecoverOutcome::Discarded`，会话重新 `OpenStaging` 继续正常流程，混合状态由本次哈希扫描修复。
- **任一单元不可完成**（`new\` 被删）：把已换的单元复原（`old\` 里还有它们的旧文件），删除暂存目录，返回 `Discarded`。
- **全部已换或待换**：把上次用户要求的更新做完——对待换单元完成两次 rename，删除单元移入 `old\`；随后删除 journal，返回 `Completed { self_replaced }`，暂存目录留给本次会话继续用，此时哈希扫描应得出无需再装任何文件。更新器本身若在中断前已换成新版，接手的就是新更新器，行为相同。
- **前滚途中失败**（某个 rename 重试后仍失败）：与阶段二内失败同一规则——把已换的单元全部复原，包括上次中断前换过的，删除 journal 与暂存目录，报 `FILE_IN_USE`。进程活着就不留下半安装；半安装只在进程意外退出这一种情形下存在。

核对需要读取 journal 内单元的当前目标文件，成本与一次只覆盖变更文件的哈希扫描相当。Mirror酱 路径的 journal 没有旧哈希，中断后除已全部换完的情形外一律丢弃、重下 zip。恢复若换掉了运行中的 exe，`self_replaced` 一路传到 `finish_staging`，本次会话无论成败都改为退出时删除暂存目录。

### Mirror酱 路径

zip 下载到 `<staging>\dl\<sha256>.zip`（`RunMirrorcDownload`）。`RunMirrorcInstall { zip_path, new_dir, sha256 }` 先以 `sha2` 计算摘要与 API 返回的 `sha256` 比对，不匹配删 zip、挂 `MIRRORC_FAILED`、不解压；解压到 `new\<相对路径>`，边写边算 md5 并 `sync_all`，返回 `MirrorcExtract { metadata, files: Vec<(rel, md5)>, deletes }`（删除清单取 `changes.json` 的 `deleted` 与 `.metadata.json` 的 `deletes` 之并），不触碰安装目录、不处理自更新。会话侧据此构造单元：安装目录不存在或为空 → 一个根目录单元；否则归档文件为文件单元、删除清单为删除单元；journal 带 `archive` 行，然后走通用的 `Commit`。`mirrorc.rs` 中没有改名、删除或直写安装目录的代码。

### 提权侧操作

`IpcOperation` 新增 `OpenStaging(install_dir)`、`Commit(CommitArgs)`、`Recover(CommitArgs)`、`DiscardStaging(root)`、`StageSelfImage(..)`；`InstallRuntime` 增加 `dl_dir`；`RunMirrorcInstall` 改为 `{ zip_path, new_dir, sha256 }`；`RmList`、`CreateUninstaller` 删除。`IpcResult` 对应新增 `OpenStaging(StagingOpened)`、`Commit(CommitOutcome)`、`Recover(RecoverOutcome)`、`DiscardStaging`，`CheckLocalFiles(LocalScan)`、`RunUninstall(UninstallOutcome)`、`RunMirrorcInstall(MirrorcExtract)` 形状变化。写入阶段的 `InstallFile` / `InstallMultichunkStream` / `RunMirrorcDownload` / `RunMirrorcInstall` 输出路径都在暂存目录下。

### 提权管道：进度与结果分离

`ManagedElevate::run` 为每个请求登记一个 `oneshot`（`Pending: HashMap<id, Sender>`），管道读任务收到 `PipeMsg::Ok` / `Err` 时按 id 投递并摘除，管道任一端结束时把所有未完成的 `oneshot` 以断连错误唤醒；`PipeMsg::Progress` 走容量 256 的 `broadcast`，`recv()` 返回 `Lagged` 时跳过继续，`Closed` 时只等结果。进度丢失只影响进度条，结果不会丢。

### 端到端验证

本提案的 e2e 用例（DFS2 stub、中断 / 篡改 / 覆盖 / 跨卷各情形）与 DFS2、H3 路径的覆盖补全归入 [核心流程 e2e 补全](../proposed/2026-09-02-e2e-core-coverage.md)；本提案自身以单测验收，见 Verification。

## Alternatives considered

- 保留 patch 与直写两条路径、各自加固：两条路径的失败语义仍不一致，自更新的改名顺序问题要在两处分别修；统一写入后 `progressed_hpatch` 与直写只差"产出的方式"。
- 每个文件旁的 `<target>.kachina-tmp` 作为暂存：目录条目翻倍、半成品对应用和用户可见、同前缀文件名在开启 8.3 短名的卷上加剧碰撞探测；集中暂存目录一处解决。
- 只用同级 `<安装目录>.kachina-staged`、不用 `%TEMP%`：省掉卷判定与跨用户 `%TEMP%` 的边角，代价是更新期间用户在应用目录旁看到一个暂存目录；选择 `%TEMP%` 优先是为了尽量不让用户看到不该看到的文件。
- journal 放在安装目录根下：目标目录不存在时无处可放，目标为空目录时会随根目录单元一起被换走；放在确定性路径的暂存目录里两种情形都成立。
- `ReplaceFileW`：语义与 `MoveFileExW(REPLACE_EXISTING)` 同卷下等价，多一个 API 面。
- 只做单文件原子提交、不做两阶段：每个文件不再半截，但中断仍留下混合版本，与"任意状态 → 目标文件集合"模型结合后是必然状态而非边角。
- MSI 式回滚（复制原文件到备份目录、失败时重放回滚脚本）：需要复制数据；`old\` 改名即备份，回滚同样是改名。
- 版本目录加启动器切换：能把中间态窗口缩为零，但改变应用的安装布局并要求经启动器进入，是应用侧决定。
- 自卸载保留"先试 `%TEMP%` 失败再试父目录"的做法：以 rename 失败反推跨卷会把锁文件与权限问题误判为跨卷，且与自更新形成两套退出清理；由 `staging_root` 事先选址后，两者共用一个机制。
- 阶段二中断后下次启动一律回滚到旧版：旧版不是这个安装模型的目标状态，且目录可能已被用户改动，回滚同样会覆盖用户的改动。
- 不核对、直接按 journal 前滚：用户在两次运行之间覆盖安装便携版后，前滚会把上次的新文件盖到用户刚放好的版本上、把用户的文件移进 `old\` 后删除，再次制造混合版本；核对全有或全无后，任何外部改动都让安装器退回"什么都不做、由用户决定"。
- 把 `new\` 当作已校验缓存并入下次会话的本地源、不设 journal：能自然处理外部改动，但用户没有再次要求更新时暂存目录要么一直留着要么白下载，且需要为"本地源 rename 进位"新增一类计划动作；核对式前滚只在目录确实未变时做事，逻辑边界更清楚。
- 下次启动一律删除暂存目录、不做任何恢复：最简单，代价是阶段二中断的用户重新下载全部变更；中断在阶段二的概率低，但核对的成本也只是哈希一遍变更文件，选择核对。
- 暂存目录内保存 metadata 快照以支持离线恢复：只在"阶段二中断、下次离线、用户仍要更新"三者同时成立时有用，不做；离线时没有元数据就不跑。
- 目录单元允许少量不受管文件、提交后逐个搬回：几百个用户数据文件的目录会退化成几百次搬运，收益消失。
- 只枚举候选目录、其余文件仍逐个 stat：省下的是候选之外的枚举，付出的是每个文件一次经过过滤驱动的打开；小文件多的应用里后者是计划阶段仅次于哈希的成本，整体枚举反而更便宜。
- `CheckLocalFiles` 接受候选目录列表、只返回干净候选：候选要用变更集算，变更集要用扫描出的哈希，两者在同一次往返里互为前提；改为对所有受管目录返回脏标记，候选由会话在计划之后算，多传回的只是一个目录名列表。
- 以本地清单缓存（路径、大小、修改时间、哈希）跳过未变文件的哈希：任何独立于扫描的缓存都可能让更新漏掉文件，风险远大于收益；扫描是唯一真相来源。
- 为目录单元中未变更的受管文件建硬链接以放宽 100% 待写的条件：每文件一次元数据操作，且引入硬链接语义；候选集足够覆盖全新安装与整目录替换两个主要场景。
- 启动时、就绪页之前对默认路径做恢复：那一刻本次 metadata 未知，无法判断 journal 里的内容是否还是这次要装的；放到 metadata 已知之后，默认路径与改过的路径走同一处代码。
- 以 `tag_name` / `version_name` 作 journal 的目标标识：项目没有版本概念，同一 tag 可以重新打包出不同内容，Mirror酱 路径的 `version_name` 与 DFS 的 `tag_name` 也不可比；目标的身份是哈希集合，journal 已经逐单元记录了新哈希，直接比哈希。
- 恢复前滚失败时保留 journal、报占用、等用户下次再前滚：进程活着却留下半安装，与阶段二内失败的规则不一致；已换的单元有 `old\` 可复原，统一回滚。
- 空间不足时退化为逐文件即时提交并以 `staged: false` 提示：重新引入中断留下混合版本的窗口，且要在三个渲染器各加一段提示；直接以 `DISK_FULL` 失败，用户腾出空间重试。
- 取消只留 token 口子、本轮不接按钮：token 穿过会话与文件边界检查已是取消实现的大半，剩余的是两个按钮与在途流的 `select!`，一并做完。
- 阶段二也可取消：阶段二只有本地 rename、秒级，取消即回滚，多一种中间态不值。
- native 进度对话框保留关闭框、把关窗映射为取消：TaskDialog 的关闭框与 Cancel 按钮都产生 `IDCANCEL`，无法区分；去掉关闭框，只留 Cancel 按钮。

## Verification

| 判据 | 结果 |
|---|---|
| `rg -n "File::create\(" src-tauri/src` 的命中仅限写暂存目录的位置、`#[cfg(test)]` 代码与 `src/builder/`；`create_target_file`、`prepare_target` 不再存在 | PASS：命中为 `fs.rs::create_staged_file`、`fs.rs::progressed_hpatch` 输出、`fs/commit.rs` 写 journal、`thirdparty/mirrorc.rs` 解压进 `new\`、`installer/uninstall.rs::stage_self_image` 写 `new\`、测试代码。`create_target_file`、`prepare_target`、`create_uninstaller` 不存在 |
| `DELETE_SELF_ON_EXIT_PATH` 的写入点全库唯一，位于所有 rename 成功且 journal 删除之后；`mirrorc.rs` 与 `run_uninstall` 不再写它 | PASS：静态量私有于 `installer/uninstall.rs`，唯一写入函数 `schedule_delete_on_exit`；`session/run.rs` 在 `finish_staging`（Commit / Recover 返回 `self_replaced` 之后）与 `run_uninstall_inner`（`UninstallOutcome.self_moved_to`）调用它 |
| `rg -n "set_current_dir\|env::temp_dir" src-tauri/src --glob '!**/builder/**'` 的命中仅限 `fs::staging`、日志文件路径、`host/webview.rs` 的 WebView2 用户数据目录与测试代码 | PASS：`fs/staging.rs`（`enter_neutral_cwd`、`scratch_file`、`temp_candidate`、`staging_root`）、`main.rs` 的 `utils::log::init(temp_dir().join("KachinaInstaller.log"))`（日志文件路径，位于 `main.rs` 而非 `utils/log.rs`）、`host/webview.rs`、`utils/process.rs` / `utils/hash.rs` / `fs.rs` / `fs/commit.rs` / `fs/staging.rs` 的测试 |
| 自卸载单测：以 `FILE_SHARE_READ \| FILE_SHARE_DELETE` 打开的文件模拟运行中的卸载器，`run_uninstall` 后该文件位于暂存目录 `old\` 下、`DELETE_SELF_ON_EXIT_PATH == Some(暂存目录)`；安装目录位于盘根时返回安装位置不安全错误且文件未移动 | PASS：`installer::uninstall::tests::park_self_moves_running_image_into_staging_old`（`park_self` 后文件在 `old\uninst.exe`，`schedule_delete_on_exit` 后读回等于暂存根）；盘根拒绝在 `fs::staging::tests::sibling_candidate_shape_and_root_rejection`（`D:\` → `INSTALL_PATH_INVALID`），只对跨卷即需要同级目录的情形生效 |
| `.instbak`、`.patching`、`.patchold`、`with_extension("old")`、`RmList`、`KachinaInstaller_Mirrorc_` 在 `src-tauri/src` 中不存在；`kachina-tmp` / `kachina-old` 仅出现在 commit 模块的复制单元分支 | PASS：`rg` 零命中；`kachina-tmp` / `kachina-old` 仅在 `fs/commit.rs` 的常量、`copy_paths` 与其测试 |
| `rg -n "remove_file\(" src-tauri/src --glob '!**/builder/**'` 的命中仅限暂存目录清理、`installer/uninstall.rs` 的卸载删除与测试代码；安装与更新路径不再直接删除安装目录内的文件 | PASS：`fs/commit.rs`（journal、复制单元临时文件）、`fs.rs` / `ipc/install_file.rs` / `thirdparty/mirrorc.rs` / `installer/runtimes.rs`（暂存目录内的产出、归档、安装器）、`installer/uninstall.rs`（卸载删除、`park_self` 清旧停放）、`installer/mod.rs` 的可写探测文件、`module/wv2.rs` 的 `%TEMP%` 引导器、测试代码。安装目录内的删除只剩卸载 |
| 暂存路径推导单测：同卷取 `%TEMP%\kachina-staged\<h>`，不同卷取 `<安装目录>.kachina-staged`；`<h>` 对大小写与斜杠不同的同一路径相同 | PASS：`staging_root_same_volume_goes_to_temp`、`sibling_candidate_shape_and_root_rejection`、`path_hash_ignores_case_and_slashes` |
| `CheckLocalFiles` 的 stat 来自目录枚举：对受管文件不再调用 `metadata()`（`skip_hash` 子树除外），返回的大小与修改时间与逐个 stat 一致 | PASS（代码结构）：`fs.rs::ScanWalk::walk` 只在 reparse 子树与全 `skip_hash` 子树下逐文件 `metadata()`；无计数器断言。`check_local_files_is_read_only_and_unwritable_false`、`check_local_files_skip_hash_only_stats` 的大小与哈希结果不变 |
| 目录单元探测单测：候选目录根下有一个不受管文件而某子目录完全受管待写时输出仅含该子目录；含 `userDataPath` 的候选不干净；含空目录的候选不干净；含未变更受管文件的候选不在输入中；删除清单中的文件不使候选变脏；每个目录只被枚举一次 | PASS：`fs::tests::dirty_dirs_follow_unmanaged_content`（根脏 / lib 脏 / 空子目录 / 用户数据子树 / 全干净 / 目录不存在）、`session::run::tests::units_subdir_when_root_is_dirty_or_partly_unchanged`（root 脏、`app.exe` 未变 → 只有 `lib` 成单元，`lib/gone.dll` 的删除被目录单元吸收）、`units_root_dir_on_fresh_install`。"每个目录只枚举一次"由递归结构保证，无计数器断言 |
| 提交单测：阶段一第二个文件写入失败时三个目标不变且无 journal；阶段一取消时目标不变、暂存目录删除、返回 `cancelled`；阶段二第二个单元 rename 失败时三个目标等于旧文件、暂存目录已删除；阶段二在第二个单元之后中断时 journal 存在，恢复后三个目标均为新文件、暂存目录已删除；恢复前滚在第三个单元失败时三个目标均等于旧文件、暂存目录已删除；`new\` 被删除后恢复时已换过的单元被复原；目录单元内文件被锁定时退化为逐文件；根目录不存在与为空各一例；空间不足时在写入前返回 `DISK_FULL` | 部分 PASS：`commit_swaps_three_files_and_removes_journal`、`commit_rolls_back_when_second_unit_is_locked`（`FILE_IN_USE`、subject `b.txt`、三目标旧字节、暂存目录不存在）、`interrupted_commit_recovers_forward`（`commit_sync` 的 `stop_after` 钩子）、`recovery_failure_rolls_back_previously_swapped_units`、`recovery_with_new_dir_deleted_rolls_back_swapped_units`、`dir_unit_degrades_when_a_file_is_locked`、`root_unit_missing_and_empty_install_dir`。阶段一写入失败不触碰目标由结构保证（写入只发生在 `new\`），无单独用例；阶段一取消由 `install_files` 的 `select!` 与 `run_install` 的 `Cancelled → SessionResult::cancelled` 实现，未做会话级单测；`DISK_FULL` 由 `ensure_space` 实现，无法在测试中制造满盘，无用例 |
| 自更新单测：模拟运行中的 exe，第二个 rename 失败时原名文件字节等于原文件、`DELETE_SELF_ON_EXIT_PATH` 为 `None`；成功时 `DELETE_SELF_ON_EXIT_PATH == Some(暂存目录)` 且 `old\` 下的旧 exe 字节等于原文件 | 部分 PASS：`self_replaced` 的判定依赖 `std::env::current_exe()`，测试进程无法把任意文件伪装成自身；运行中镜像的 rename 由 `park_self_moves_running_image_into_staging_old` 覆盖，rename 失败后原文件字节不变由 `commit_rolls_back_when_second_unit_is_locked` 覆盖，`schedule_delete_on_exit` 的写入由同一自卸载测试覆盖 |
| Mirror酱 单测：合成 zip（含 `changes.json` 的 `deleted` 项、一个与当前 exe 同名的文件、一个子目录）解压后全部产物位于 `new\` 下、安装目录字节不变、返回的删除清单与 `changes.json` 一致；`.metadata.json` 变体同理；zip 摘要不匹配时返回错误且 `new\` 为空；二者皆无时报归档无效 | PASS：`thirdparty::mirrorc::tests::extracts_into_new_dir_and_reports_deletes`、`metadata_variant_and_invalid_archives` |
| 探测单测补充：候选目录下的 junction 子目录使候选变脏且其内受管文件成为复制单元；提交前重新探测时向已判干净的目录写入一个文件，该目录单元降级为逐文件提交且该文件保留在原位 | PASS：`fs::tests::junction_subdir_is_reparse_and_dirty`、`session::run::tests::units_copy_under_reparse_point`、`fs::commit::tests::dir_unit_degrades_when_no_longer_clean` |
| 复制单元单测：以 junction 指向另一临时目录，提交后目标文件为新内容、`.kachina-tmp` / `.kachina-old` 已清理；注入第二个 rename 失败时目标为旧内容 | PASS：`copy_unit_via_junction`（第二例以后续单元被锁触发整体回滚，复制单元的旧内容恢复） |
| journal 版本单测：首行为 `kachina-journal 0` 或缺失时恢复不执行任何 rename、暂存目录被删除 | PASS：`journal_roundtrip_and_version_gate`（`parse` 返回 `None`）；会话侧 `recover_or_discard` 对 `None` 走 `DiscardStaging` + 重开 |
| 目标核对单测：第二个单元的哈希不等 → 丢弃；`del` 不再在清单 → 丢弃；算法不同 → 丢弃；`archive` 不等 → 丢弃；全部相等进入目录核对 | PASS：`journal_target_check` |
| 恢复核对单测：目录未变时恢复完成剩余单元；第三个单元的目标被替换时不执行任何 rename、被替换内容保留；第一个（已换）单元被改动时同样整体丢弃；删除单元的目标被放回同哈希文件时视为未变；旧哈希为 `-` 的尚未换单元视为有变 | PASS：`interrupted_commit_recovers_forward`、`recovery_discards_when_target_was_overwritten`（`a.txt` 已换的单元保持新内容不被回滚）、`recovery_discards_when_swapped_unit_was_modified`。删除单元放回与 `-` 旧哈希两条由 `classify` 的分支直接给出，无单独用例 |
| 恢复时机单测：`run_install` 在 metadata 之后、哈希扫描之前调用恢复，`run_mirrorc` 在拿到 API 应答之后调用；改过安装路径时查的是新路径 | 代码路径审阅：`run_dfs_install` 在 `pick_metadata` / `prepare_process` 之后、`dfs_staged` 的哈希扫描之前调用 `open_staging` + `recover_or_discard`；`run_mirrorc` 在 `get_mirrorc_status` 与 `prepare_process` 之后调用；`install_path` 取自 `Settings`，即 `Start` 时的路径。会话函数依赖网络与 IPC，无单测 |
| `STAGING_IN_USE`：另一进程持有 `lock` 且 pid 存活时以该码失败、暂存目录不被删除；pid 已死时视为残留、整目录删除后继续 | PASS：`fs::staging::tests::open_refuses_live_lock`（以 `cmd /C ping` 子进程的 pid 写 `lock`）、`open_keeps_journal_dir_and_deletes_residue` |
| 取消单测：`Running` 中收到 `Intent::Cancel` 后 token 被触发；阶段二进行中收到 `Cancel` 无效果；`cancel` 在 WebView 与 native 各有一个入口 | 部分 PASS：`render.test.tsx` 的 `renders running progress with a cancel button`（点击后发 `{kind: "cancel"}`）与 `hides cancel during the swap`（`stage == "commit"` 时无按钮）；`handle_intent(Cancel)` → `GuiRuntime::cancel_running` 与 native `ProgressDialog::show_with_cancel` 的回调无单测（依赖宿主 / TaskDialog） |
| 每个产出文件在校验通过后调用 `sync_all` | PASS（代码结构）：`ipc/install_file.rs::finalize_staged` 在 `verify_hash` 之后调 `fs.rs::sync_staged_file`，三种模式与 `install_file_by_reader` 共用；Mirror酱 解压在 `mirrorc.rs` 内逐文件 `sync_all`。无计数器断言 |
| 删除单元单测：提交后该文件位于 `old\` 下、目标不存在；恢复时删除项同样完成；回滚时该文件回到原位 | PASS：`delete_unit_moves_to_old_and_rolls_back`（含回滚）；恢复路径的删除单元由 `classify` + `apply_unit` 共用同一实现 |
| `ipc/manager.rs` 单测：服务端在回 `Ok` 前发送 1000 条 `Progress`，客户端 `run` 返回 `Ok` 而非 `IPC_ERR` | PASS：`progress_flood_does_not_lose_result`（`ManagedElevate::detached` 直连 `Pending` 与进度广播，容量 256）；`pipe_roundtrip_and_disconnect` 改为按 `oneshot` 断言，并新增孤儿等待者在断连时收到错误 |
| 卸载器 / 更新器按会话情形生成或刷新，进同一份 journal；根目录单元存在时并入其文件清单 | PASS：`session::run::tests::self_image_plan_follows_session_kind`（全新安装两者皆生成；更新且清单无 updater 时外来安装器生成 updater、卸载器存在才刷新；清单有 updater 时只刷新卸载器且 `copy_from` 指向 `new\` 或安装目录里的 updater；无卸载器则无事可做）、`self_units_fold_into_root_unit_or_append`。"运行的是 updater 本身"一行依赖 `std::env::current_exe()`，测试进程无法伪装，由代码路径给出 |
| 盘根不是安装目录 | PASS：`installer::tests::drive_root_is_never_an_install_dir`（`C:\`、`d:/`、`E:`、`C:\\` 均被 `probe_dir` 拒绝，`C:\App` 与 UNC 不受影响） |
| `locales/zh-CN.tsv` 含 `STAGING_IN_USE`、`progress.commit`，`DISK_FULL` 文案为"磁盘空间不足，无法完成安装/更新"；`locale_covers_codes_stages_prompts` 通过 | PASS |
| 现有 e2e 十项全绿；`offline-install` 追加断言父目录无 `.kachina-staged` | 未跑：本机未执行 `test:all`，待 CI；`offline-install` 的追加断言归入 [核心流程 e2e 补全](../proposed/2026-09-02-e2e-core-coverage.md) |
| 单测与前端 | PASS：`cargo test`（`src-tauri` 内）13 passed（kachina-builder）+ 113 passed / 1 ignored（kachina-installer）；`pnpm exec vitest run` 20 passed；`pnpm exec tsc --noEmit` 零错误 |
| 体积 | 本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std、`src-tauri` 内构建、含前端 HTML：3,046,912 → 3,142,656 字节（+95,744）。增量来自 `fs/staging.rs`、`fs/commit.rs`、目录枚举与四个新 `IpcOperation` / `IpcResult` 变体的 postcard 单态化 |

## Consequences

- 磁盘峰值从"单个文件的目标 + 暂存"变为"现有安装 + 本次全部变更"：空间不足直接失败，用户须先腾出空间；此前能"边下边换、勉强装完"的用户现在装不了。
- 每个产出文件一次 `sync_all`：大文件是一次顺序刷盘，与下载耗时相比可忽略；数万个小文件时每次毫秒级，累计秒到十秒级。
- 复制单元跨链接边界不原子：只影响用户自行建立 junction 的子树，且子树内单个文件仍原子；不做复制模式则这些用户从"能装"变成"装不了"。
- journal 版本不全等时直接删除暂存目录：若此时阶段二已进行到一半，混合状态会保留到哈希扫描修复完成；格式版本只在安装器自身升级且恰好跨越一次中断的提交时才会不等。
- 阶段二窗口内用户手动启动应用仍会看到混合状态：窗口长度是单元数次本地 rename，目录单元把最常见的全新安装缩为一次；散落变更的更新仍是 N 次，几千个文件为秒级。彻底消除窗口需要应用侧的版本目录布局。
- 以不同用户账户运行下一次更新时 `%TEMP%` 不同，找不到上次的 journal：哈希扫描看到混合状态后走正常修复重新下载，另一用户 `%TEMP%` 下的暂存目录成为孤儿。
- `%TEMP%` 被系统磁盘清理清除后 journal 与 `new\` 一起消失：等同于无暂存目录，阶段二中断留下的混合状态由用户下一次主动更新时的哈希扫描修复。
- 恢复核对以哈希相等判定"未变"，用户把某个文件替换成恰好同哈希的内容不会被识别为改动：这等价于没有改动，无害。
- 同一安装目录被两个安装器并发处理：`lock` 让后来者以 `STAGING_IN_USE` 退出；pid 复用导致误判的概率可忽略。
- 目录 rename 在目标目录内有打开句柄时失败：退化为逐文件提交，只损失优化不引入新失败模式。
- 安全软件持有新文件超过重试窗口（合计约 1.5 秒）时以 `FILE_IN_USE` 报错：用户重试即可，原文件完好。
- 自更新的两次 rename 之间仍有一个原名缺失的瞬间：毫秒级本地操作，第二次失败有复原；进程恰在此刻被强制结束时新旧两份都在暂存目录与 `old\` 中，下次会话前滚。
- Mirror酱 路径无 e2e：API 域名写死在 `get_mirrorc_status`，无法以本地 stub 替代；解压、删除清单与摘要校验由合成 zip 的单测覆盖，提交与恢复与 DFS 路径共用同一实现与测试。
- Mirror酱 增量包装进非空目录时全部是文件单元，散落变更仍是逐文件 rename：与 DFS 路径一致。
- 卷判定以 `GetVolumePathNameW` 的卷根比较，`subst` 与网络映射盘会被判为不同卷而走同级暂存目录：结果仍然正确（同级一定同卷），只是少了 `%TEMP%` 的不可见性；真正跨卷的 rename 从未发生。
- 恢复放在 metadata 已知之后：就绪页显示时目录可能仍是上次中断的半提交状态，模式判定（是否升级）按当前目录内容得出；用户在就绪页停留期间应用不可用的时长不变，只是恢复晚了一个点击。
- 目标内容不等即丢弃暂存目录：上次中断后服务端又发了新内容，哪怕只有一个文件变了，整个暂存目录都白费；正确性优先于流量，部分复用 `new\` 里仍然有效的文件需要把暂存目录当本地源，见 Alternatives。
- 阶段一取消后暂存目录删除、已下载内容丢弃：取消后再点开始要重下；不保留是为了让"取消"语义干净，与失败一致。
- native 进度对话框没有关闭框：取消只能经 Cancel 按钮，按钮按下后置灰等待会话收尾；结束进程仍是用户可用的最后手段，阶段一被结束只留暂存残留。
- 更新时不再补回被用户删掉的卸载器，其快捷方式也只在卸载器存在时创建；此前每次 REG 来源的更新都会重写卸载器与更新器。运行为 updater 本身且清单无 updater 时，卸载器由运行中的 updater 镜像刷新——它是这份安装能拿到的最新镜像。
- 盘根成为硬限制后，历史上装在盘根的安装既不能升级也不能卸载，只能手工处理。
- 提权管道改 `oneshot` 后，`ManagedElevate` 被丢弃时向所有未完成的 `oneshot` 发送断连错误；`pipe_roundtrip_and_disconnect` 的孤儿等待者一条覆盖此路径。
- 二进制增加约 94 KiB（本机口径）：换来的是阶段一零触碰安装目录、阶段二可回滚、中断可前滚。
