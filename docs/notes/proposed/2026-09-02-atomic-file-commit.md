# 文件提交协议：暂存目录、两阶段提交与目录单元

Status: proposed

## Problem

安装器向安装目录写文件有三条路径，安全性不一致，自更新在失败时会删掉更新器的最后一份拷贝，中断会留下混合版本，而计划阶段对目标目录里不属于本应用的文件一无所知。

**三条写入路径。**

- 直写：`ipc/install_file.rs` 的 `InstallFileMode::Direct` 与 `install_file_by_reader` 调 `fs.rs::create_target_file`，即 `File::create(target)`——先截断目标再写入。下载中断、进程被结束、断电都会留下半截目标文件，旧版本已不存在；随后的哈希校验在同一个被截断的文件上失败。
- patch：`fs.rs::progressed_hpatch` 写 `<target>.patching`，成功后 `rename(target → .old)`、`rename(.patching → target)`、`remove(.old)`。三步之间没有回滚：第二个 rename 因共享冲突失败时目标缺失、只剩 `.old`。
- Mirror酱：`thirdparty/mirrorc.rs` 的 zip 解压对每个文件 `File::create(out_path)` 直写；自更新另有一套 `.instbak` 改名。zip 下载到安装目录内（`session/run.rs::run_mirrorc` 的 `join_install(install_path, "KachinaInstaller_Mirrorc_<sha256>.zip")`），失败时留在用户目录；API 返回的 `sha256` 只用于拼文件名，下载内容从未与之校验。
- 删除：DFS 路径的 `plan.deletes` 经 `IpcOperation::RmList` 直接 `remove_file`，结果被丢弃；Mirror酱 路径在解压后按 `changes.json` / `.metadata.json` 的清单 `remove_file`。两处删除都不可撤销。

**自更新失败即丢更新器。** 目标是自身时 `fs.rs::prepare_target` 先 `rename(updater.exe → updater.instbak)`，并在同一处把 `installer/uninstall.rs` 的 `DELETE_SELF_ON_EXIT_PATH` 设为 `.instbak`；此刻磁盘上没有 `updater.exe`。随后 `progressed_hpatch` 以 `.instbak` 为旧文件、`updater.patching` 为输出。任何失败（网络中断、hpatch 返回非 1、`RUN_HPATCH_ERR`）只删 `.patching` 并返回错误，不把 `.instbak` 改回原名。用户关窗时 `host/mod.rs` 主循环对 `UiAction::Close` / `WM_QUIT` 调 `delete_self_on_exit()`，按第一步设下的路径 `del /f /q updater.instbak`。更新器与备份同时消失。同一根因的变体：patch 成功但哈希不匹配，留下坏的 `updater.exe` 而 `.instbak` 照删；`Direct` 模式自更新留下半截 `updater.exe` 而 `.instbak` 照删。曾有线上事故表现为更新失败后更新器消失且无备份文件，与此路径一致。

**中断留下混合版本。** 安装模型是"任意本地状态 → 服务器上的目标版本"，每个文件装完即生效。更新进行到一半被关闭、断电或结束进程，安装目录就处于一部分文件是新版、一部分是旧版的状态；应用没有自检能力，这种状态下启动大概率崩溃。用户重跑安装器可以修复，但在此之前应用不可用，而用户并不知道要重跑。

**计划看不见不受管文件。** `session/run.rs::scan_local` → `IpcOperation::CheckLocalFiles` → `fs.rs::check_local_files` 只按 metadata 的文件清单逐个 `metadata()`，从不 `read_dir`；目标目录里不在 metadata 中的文件（用户数据、日志、用户自行放入的文件）计划完全不知情。仓库中仅有的 `read_dir` 都不是扫描：`run.rs` 对 `ignoreFolderPath` 只取首个条目判非空，`fs.rs::is_dir_empty` 供路径选择器，`uninstall.rs` 供删除。

**临时目录逻辑分散且以失败反推。** `installer/uninstall.rs::run_uninstall` 自卸载时 `rename(exe → %TEMP%\kachina.uninst.<unix 秒>.exe)`，失败即假定跨卷改挪到安装目录父目录——安全软件锁文件、权限不足都会被误判为跨卷；文件名以秒为粒度可撞名；它是 `DELETE_SELF_ON_EXIT_PATH` 的第二个写入点。`host/mod.rs`、`host/native.rs`、`session/run.rs::silent_main` 各有一份 `set_current_dir(%TEMP%)` 与各自的错误呈现——进程 cwd 会锁住所在目录，这一步是安装目录能被 rename 与删除的前提，不只是探测。`installer/runtimes.rs` 与 `module/wv2.rs` 以固定文件名直接下载到 `%TEMP%` 根下，失败时不清理。

**e2e 覆盖不到这些。** `tests/` 十项全是 packed HTTP 源的成功路径；线上主要下载路径 DFS2（`session/source.rs` 的 `ensure_dfs2_session` / `create_dfs2_session_with_challenge` / `resolve_dfs2_location` / `prefetch_batch_urls`，`dfs.rs` 的五个 API）零覆盖；没有中断后重跑的用例，尽管重跑即修复是安装器最重要的恢复属性。

**提权管道进度洪水。** `ipc/manager.rs` 的 `ManagedElevate::run` 用一个容量 100 的 `broadcast` 同时承载进度与结果，`recv()` 返回 `Lagged` 时 `while let Ok` 退出循环、整次操作报 `IPC_ERR`。文件数多、主线程慢时进度消息可以填满通道。

## Proposal

### 暂存目录

每次会话一个暂存目录，位置由安装路径确定性推导，两次运行之间可以重新找到：

- 与目标同卷时：`%TEMP%\kachina-staged\<h>\`，`<h>` 为安装路径经 `session/plan.rs::normalize_full` 规范化后哈希的前 16 个十六进制字符。用户在安装目录旁看不到任何暂存文件。
- 不同卷时（`GetVolumePathNameW` 对两侧取卷根比较）：`<安装目录>.kachina-staged\`，例如 `D:\app\someapp.kachina-staged\`。rename 必须同卷才是原子的，跨卷会退化成复制，所以不能用 `%TEMP%`。父目录不存在时先 `create_dir_all`，创建空父目录是幂等的、不回滚。

目录布局：`new\<相对路径>` 放产出文件，`old\<相对路径>` 放被换下的旧文件，`dl\` 放运行库与 WebView2 引导器等临时下载，`journal` 是提交清单，`lock` 内含本进程 pid。安装目录内不再出现任何安装器自己的文件。

`fs::staging` 模块承载全部临时目录逻辑：`same_volume(a, b)` 以 `GetVolumePathNameW` 的卷根比较；`staging_root(install_dir)` 按上述规则选址，安装目录位于盘根而没有父目录时报安装位置不安全；`enter_neutral_cwd()` 把进程 cwd 切到 `%TEMP%` 并在失败时返回统一的临时目录不可用错误。`host/mod.rs`、`host/native.rs`、`silent_main` 三处 `set_current_dir` 改调 `enter_neutral_cwd`，提权子进程启动时同样调用而不依赖继承。自卸载改为 `rename(exe → <staging>\old\<uninstall_name>)`，同卷由 `staging_root` 保证，不再以 rename 失败反推跨卷；`run_uninstall` 不再写 `DELETE_SELF_ON_EXIT_PATH`，退出清理与自更新共用同一机制。运行库与 WebView2 引导器下载到 `dl\`，随暂存目录一起清理。

启动时按"同级、`%TEMP%`"顺序查找暂存目录：有 `lock` 且 pid 存活则另一个安装器正在处理同一目录，报错退出；有 `journal` 进入恢复核对（见下）；否则是上次死在写入阶段的残留，整目录删除。暂存目录不跨越两次启动存活：恢复要么当场完成要么当场丢弃，本次进程退出时若会话未走到阶段二，也整目录删除。

### 统一写入

`fs.rs` 新增 `commit` 模块。所有产出文件——直写、patch 输出、HybridPatch、`install_file_by_reader`、Mirror酱 zip 解压的每个文件、Mirror酱 自更新下载——一律写到 `new\<相对路径>`。`clear_index_mark` 与 `verify_hash` 在 `new\` 下完成，任一失败删该文件并返回错误，目标从未被触碰。校验通过后对该文件 `File::sync_all()`（`FlushFileBuffers`）：rename 的原子性只在元数据层，NTFS 不记录数据日志，不刷盘则阶段二之后立刻掉电会换上零长度或半截的文件。`progressed_hpatch` 简化为 `(old_path, diff_stream, diff_size, out_path)`：读旧文件、写 `out_path`，不再包含任何改名逻辑；`.patching` / `.patchold` / `.old` / `override_old_path` 删除。`prepare_target` 删除。`create_target_file` 删除。

### 计划阶段：目录单元探测

提交以"单元"为粒度，单元有三种：文件（写入）、目录（整棵子树写入）、删除。目录单元一次 rename 换掉整棵子树，其判定条件是：**子树内受管文件 100% 待写，且不含任何不受管内容**。

候选目录由受管集合与变更集算出，不需要文件系统：其下每个受管文件都是新增或变更。DFS 路径的受管集合是 metadata 的 `hashed` 与删除清单；Mirror酱 路径没有单文件 metadata，受管集合是归档内文件与删除清单，磁盘上不在其中的文件无法区分"上一版保留"与"不受管"，一律视为不受管——全量包装进空目录时根目录成为单元，增量包只有被整体覆盖的子目录成为单元。全新安装（目标目录不存在）时根目录是唯一候选，且无需探测。普通更新（散落几十个文件变更）候选集为空，无需探测。

探测与本地文件的 stat 合并为 `IpcOperation::CheckLocalFiles` 中的**一趟目录枚举**（与哈希扫描同一次往返、同一提权侧），输入增加 `probe_dirs: Vec<String>`（候选目录，相对安装目录），输出增加 `clean_dirs: Vec<String>`。现有实现对每个受管文件单独 `tokio::fs::metadata()`，Windows 上这是 `CreateFileW` + `GetFileInformationByHandle` + `CloseHandle`——每个文件一次打开，每次打开经过文件系统过滤驱动的回调；数万个小文件即数万次带扫描回调的打开。改为对含受管文件的目录 `read_dir`：`NtQueryDirectoryFile` 一次带回一批条目，每个条目自带大小、修改时间与属性，`DirEntry::metadata()` 在 Windows 上直接返回这份数据不再打开文件。受管文件的 stat 从枚举结果取，同一趟里得到每个目录的干净标记。自顶向下：

- 文件条目：受管待写、受管未变、删除清单内、其它。出现"其它"即标本目录脏；受管文件的大小与修改时间记入扫描结果。
- 子目录条目：是 reparse point（junction / 符号链接，`FILE_ATTRIBUTE_REPARSE_POINT`） → 标父目录脏，不进入，并把该子树登记为**复制模式**（见下）——链接目标通常在另一卷，rename 跨卷会失败；位于 `userDataPath` / `ignoreFolderPath` / `extraUninstallPath` 下 → 标父目录脏，不进入，其下的 `skip_hash` 文件保留单独 stat；metadata 中没有任何受管文件 → 视为不受管内容，标父目录脏，不进入（空目录同样算脏，整体 rename 会丢掉它）；否则进入。
- 候选目录的干净标记：本目录无"其它"条目，且所有子目录要么干净要么不存在受管文件之外的内容。父目录脏不代表子目录脏，`node_modules\` 根下一个用户文件不妨碍 `node_modules\foo\` 整体 rename；取最靠上的干净候选为单元。每个目录只枚举一次。
- 路径匹配一律用 `normalize_full` 的小写 slash 形式，`Foo.dll` 与 `foo.dll` 视为同一受管文件。

枚举的成本与哈希扫描不同量级：NTFS 枚举数万个条目是数百次目录查询、数百毫秒，每个条目一次哈希集合查表，不读文件内容。历史上"全部哈希一遍"的慢是读内容，`skip_hash` 已经解决，本提案不改哈希范围；哈希阶段仍是读盘瓶颈，不受本改动影响。

删除清单中的文件在干净单元内无害：旧目录整体换到 `old\`，它们随之消失。

reparse point 子树内的受管文件成为**复制单元**：阶段一仍写到 `new\`，阶段二在链接目标所在卷内完成单文件提交——复制到目标同目录的 `<名>.kachina-tmp`、校验、`rename(目标 → <名>.kachina-old)`、`rename(tmp → 目标)`，回滚与恢复用 `.kachina-old`，收尾删除。这是唯一会在安装目录内出现临时文件的情形，只发生在用户自行建立的链接之下；跨链接边界的原子性无法保证，子树内单个文件仍是原子的。

### 两阶段提交

**阶段一（写入）**：对计划中的全部文件只做写入、后处理、校验，不 rename。此阶段的任何失败或中断（网络、哈希、磁盘满、进程被结束）发生时安装目录未被修改，应用仍是完整旧版；残留只在暂存目录里，下次运行整目录删除。

**阶段二（提交）**：开始前对所有目录单元**重新探测**一次（只枚举目录名），探测与提交之间隔着整个下载，用户可能已在被判干净的目录里放了文件，变脏的单元降级为逐文件提交。然后写 `journal`——首行为格式版本 `kachina-journal 1`，其后每行一个单元：`file <相对路径> <旧哈希|-> <新哈希>`、`dir <相对路径>`（其下文件各占一行 `file` 记录）、`del <相对路径> <旧哈希|->`、`copy <相对路径> <旧哈希|-> <新哈希>`，按提交顺序。新哈希在写 `new\` 校验时已算出；旧哈希来自计划阶段的扫描，Mirror酱 路径没有扫描，记 `-`——再逐单元：

- 删除单元：`rename(目标 → old\<相对路径>)`，目标不存在则跳过。删除因此可回滚、进 journal、失败不再被丢弃；`IpcOperation::RmList` 退役。
- 目录单元：目标目录存在则 `rename(目标 → old\<相对路径>)`，再 `rename(new\<相对路径> → 目标)`。目录 rename 因内部文件被占用而失败时，不放弃该单元，退化为逐文件提交：`new\<相对路径>\` 下每个文件按文件单元规则移入目标目录，这是同卷目录间的 rename，同样原子。
- 文件单元：目标存在则 `rename(目标 → old\<相对路径>)`，再 `rename(new\<相对路径> → 目标)`。
- 复制单元：按上节规则在链接目标所在卷内完成。
- 根目录单元：目标不存在 → `create_dir_all(父目录)`，`rename(new → 安装目录)`，一次操作；目标存在且为空 → `rmdir(安装目录)`，再同一个 rename。
- 每次 rename 遇 os error 32（共享冲突）、33（锁冲突）、5（拒绝访问）时以 50、100、200、400、800 ms 退避重试五次——安全软件扫描新写入文件的窄窗口就在这里。
- 全部单元完成后删除 `journal`；若本次自更新了更新器，`DELETE_SELF_ON_EXIT_PATH` 设为暂存目录（旧 exe 在 `old\` 下，运行中不可删），`delete_self_on_exit` 改为 `rmdir /s /q`；否则同步删除暂存目录。这是 `DELETE_SELF_ON_EXIT_PATH` 全库唯一的写入点，位于所有 rename 成功之后。

阶段二只有本地 rename，是唯一存在混合状态的窗口，长度为单元数次 rename。目录单元把全新安装的窗口缩为一次操作，把整目录替换（node_modules 类）从 3N 次缩为两次。

**阶段二内的失败**：某个 rename 在重试后仍失败（文件被占用），把已换过的单元按逆序复原（`rename(目标 → new\…)`、`rename(old\… → 目标)`），删除 journal 与暂存目录，报文件占用；安装目录回到完整旧版。`ProbeWritable` 与占用提示在阶段一之前已经排除了已知占用，此路径应当罕见。

**磁盘空间**：阶段一结束时峰值为现有安装加本次变更文件总大小。计划阶段以变更总量对比暂存目录所在卷的可用空间，不足时退化为逐文件即时提交（写完一个校验一个立即换入，同一暂存布局），并在会话状态中标记 `staged: false` 供界面告知用户本次更新中断后需重跑修复。

### `old\` 的产生与寿命

`old\` 只在阶段二产生，阶段一不触碰安装目录。每个单元放入新文件之前，目标已存在的才 rename 进 `old\`；新增文件、不存在的根目录、为空的根目录（`rmdir`）都没有备份。删除单元的"删除"就是 rename 进 `old\`。复制单元不进 `old\`，在链接目标所在卷内改名为同目录的 `<名>.kachina-old`。自卸载时运行中的卸载器 rename 进 `old\`，是唯一发生在阶段二之外的写入。

用途只有两个：同一进程内阶段二失败时按逆序复原；停放运行中的 exe（更新器、卸载器）——运行中的镜像只能改名不能删除。它不用于跨运行的回滚：下次启动核对不通过时整个暂存目录连同 `old\` 一起删除，核对通过时前滚也不需要它。

寿命上限是一次进程：阶段二成功结束时随暂存目录同步删除；`old\` 里停着本进程的 exe 时延到退出，由 `delete_self_on_exit` 的 `rmdir /s /q` 一起带走。每个单元为此多付一次同卷 rename，没有数据复制。

### 恢复

任何安装器或更新器启动、就绪页之前，若暂存目录存在 `journal`，先读首行：格式版本与当前不全等则不做任何 rename，整个暂存目录删除。版本相符则**先核对、再决定**，核对是全有或全无的：

- 逐单元哈希当前目标。文件与复制单元：当前哈希等于旧哈希（尚未换）或等于新哈希（已换）为"未变"；目标缺失而旧哈希存在、或哈希与两者都不等，为"有变"。删除单元：目标缺失或哈希等于旧哈希为"未变"。目录单元：目标目录内文件集合与哈希整体等于旧集合或等于新集合为"未变"，多一个文件、少一个文件、任一哈希不符都是"有变"。旧哈希为 `-` 的单元只有"已换"能被证明，尚未换即视为"有变"。
- **任一单元有变**：用户在两次运行之间改动了目录（覆盖安装了便携版、手动替换了文件），上次那次更新的前提不再成立，不做任何 rename，删除 journal 与整个暂存目录，进入正常就绪页，是否更新由用户重新决定。
- **全部未变**：目录还是上次中断时的样子，把上次用户要求的更新做完——对尚未换的单元完成两次 rename，删除单元移入 `old\`；随后删除 journal 与暂存目录，进入正常流程。更新器本身若在中断前已换成新版，接手的就是新更新器，行为相同。

核对需要读取 journal 内单元的当前目标文件，成本与一次只覆盖变更文件的哈希扫描相当。Mirror酱 路径的 journal 没有旧哈希，中断后除已全部换完的情形外一律丢弃、重下 zip。

### Mirror酱 路径

zip 下载到 `<staging>\dl\<sha256>.zip`，完成后以 `sha2` 计算摘要与 API 返回的 `sha256` 比对，不匹配则报错、不解压。`RunMirrorcInstall` 改为只做阶段一：解压到 `new\<相对路径>`，返回 `.metadata.json` 原文与删除清单（`changes.json` 的 `deleted` 或 `.metadata.json` 的 `deletes`），不触碰安装目录、不处理自更新。会话侧据此构造单元清单——归档文件为文件或目录单元、删除清单为删除单元、更新器与其它文件无异——然后走通用的 `Commit`。`mirrorc.rs` 中的 `.instbak` 改名、`remove_file` 删除、`File::create` 直写全部删除。

### 提权侧操作

阶段二与恢复在 Program Files 下需要提权，新增 `IpcOperation::Commit { staged, journal }` 与 `IpcOperation::Recover { staged }`，由提权进程执行全部 rename；`CheckLocalFiles` 增加 `probe_dirs` / `clean_dirs`；`RmList` 删除。写入阶段的 `InstallFile` / `InstallMultichunkStream` / `RunMirrorcDownload` / `RunMirrorcInstall` 输出路径改为暂存目录下的路径，不再直接触碰安装目录。

### 提权管道：进度与结果分离

`ManagedElevate::run` 为每个请求登记一个 `oneshot`（以请求 id 为键），管道读任务收到 `PipeMsg::Ok` / `Err` 时按 id 投递，`Disconnect` 时唤醒全部等待者；`PipeMsg::Progress` 走 `broadcast`，`recv()` 返回 `Lagged` 时跳过继续，不影响结果。进度丢失只影响进度条，结果不会丢。

### 核心流程 e2e

`tests/server.mjs` 增加 DFS2 stub：`GET <api>?with_metadata=1` 返回 `Dfs2Metadata`（`data.index` 指向 fixtures 中 hashed 目录、`data.metadata` 为 `gen` 产出）；`POST <api>` 创建 session，默认直接返回 `sid`，`?challenge=1` 时先返回 `challenge: "md5"` 与 `data: "<hash>/<source>"`（与 `dfs.rs::solve_dfs2_challenge` 的 md5 前缀搜索一致）再验证 `challenge` 字段；`GET <api>/session/<sid>/<res>?range=` 返回 `Dfs2ChunkResponse`；`POST <api>/session/<sid>/<res>` 批量返回 `Dfs2BatchChunkResponse`；`DELETE` 接收 `Dfs2DeleteRequest`。chunk URL 指向同一 express 的静态文件并透传 Range。stub 支持两个故障开关：`?corrupt=<file>` 使指定文件的 chunk 返回篡改后的字节；`?delay=<ms>` 使每个 chunk 响应前等待。

新增用例：

- `online-install-dfs2` / `online-update-dfs2`：源 uri 为 `dfs2+http://localhost:8080/api/TestApp`，断言与现有 `online-install` / `online-update` 相同的文件集与更新器自更新哈希；`online-update-dfs2` 以 `?challenge=1` 运行。
- `update-corrupt-updater`：stub 对 `updater.exe` 的 chunk 返回篡改字节；断言更新以非零退出码结束、`updater.exe` 哈希仍等于 v1、安装目录及其父目录无 `.kachina-staged`、`%TEMP%\kachina-staged\` 下无对应条目、无 `.instbak`；随后以正常 stub 重跑更新成功。
- `update-interrupted`：stub 以 `?delay=300` 拖慢下载，更新启动 2 秒后 `process.kill`（落在阶段一）；断言每个受 metadata 管理的文件哈希都等于 v1、暂存目录内无 `journal`；重跑一次后文件集与 `online-update` 相同且暂存目录已删除。
- `update-interrupted-commit`：以隐藏环境变量让安装器在阶段二完成一半时 `process::exit`；断言暂存目录内 `journal` 存在；重跑一次后文件集与 `online-update` 相同、暂存目录已删除、stub 未收到任何 chunk 请求（前滚零下载）。
- `update-interrupted-then-overwritten`：同上中断后，把 fixtures 中 v1 的全部文件复制覆盖到安装目录（模拟便携版覆盖）；重跑一次后断言安装目录每个文件哈希等于 v1、暂存目录已删除；再以 `--source local-v2` 正常更新成功。
- `install-cross-volume`：安装目录位于 `subst` 出来的盘符下——按卷根比较它是另一个卷，走同级 `<安装目录>.kachina-staged`，而底层同卷保证 rename 仍然成功——断言暂存目录出现在同级并在成功后删除。

现有十项保持不变；`offline-install` 追加断言：成功后安装目录父目录下无 `.kachina-staged`。

## Alternatives considered

- 保留 patch 与直写两条路径、各自加固：两条路径的失败语义仍不一致，自更新的改名顺序问题要在两处分别修；统一写入后 `progressed_hpatch` 与直写只差"产出的方式"。
- 每个文件旁的 `<target>.kachina-tmp` 作为暂存：目录条目翻倍、半成品对应用和用户可见、同前缀文件名在开启 8.3 短名的卷上加剧碰撞探测；集中暂存目录一处解决。
- 只用同级 `<安装目录>.kachina-staged`、不用 `%TEMP%`：省掉卷判定与跨用户 `%TEMP%` 的边角，代价是更新期间用户在应用目录旁看到一个暂存目录；选择 `%TEMP%` 优先是为了尽量不让用户看到不该看到的文件。
- journal 放在安装目录根下：目标目录不存在时无处可放，目标为空目录时会随根目录单元一起被换走；放在确定性路径的暂存目录里两种情形都成立。
- `ReplaceFileW`：语义与 `MoveFileExW(REPLACE_EXISTING)` 同卷下等价，多一个 API 面。
- 只做单文件原子提交、不做两阶段：每个文件不再半截，但中断仍留下混合版本，与"任意状态 → 目标版本"模型结合后是必然状态而非边角。
- MSI 式回滚（复制原文件到备份目录、失败时重放回滚脚本）：需要复制数据；本提案的 `old\` 改名即备份，回滚同样是改名。
- 版本目录加启动器切换：能把中间态窗口缩为零，但改变应用的安装布局并要求经启动器进入，是应用侧决定。
- 自卸载保留"先试 `%TEMP%` 失败再试父目录"的做法：以 rename 失败反推跨卷会把锁文件与权限问题误判为跨卷，且与自更新形成两套退出清理；由 `staging_root` 事先选址后，两者共用一个机制。
- 阶段二中断后下次启动一律回滚到旧版：旧版不是这个安装模型的目标状态，且目录可能已被用户改动，回滚同样会覆盖用户的改动。
- 不核对、直接按 journal 前滚：用户在两次运行之间覆盖安装便携版后，前滚会把上次的新文件盖到用户刚放好的版本上、把用户的文件移进 `old\` 后删除，再次制造混合版本；核对全有或全无后，任何外部改动都让安装器退回"什么都不做、由用户决定"。
- 把 `new\` 当作已校验缓存并入下次会话的本地源、不设 journal：能自然处理外部改动，但用户没有再次要求更新时暂存目录要么一直留着要么白下载，且需要为"本地源 rename 进位"新增一类计划动作；核对式前滚只在目录确实未变时做事，逻辑边界更清楚。
- 下次启动一律删除暂存目录、不做任何恢复：最简单，代价是阶段二中断的用户重新下载全部变更；中断在阶段二的概率低，但核对的成本也只是哈希一遍变更文件，选择核对。
- 暂存目录内保存 metadata 快照以支持离线恢复：只在"阶段二中断、下次离线、用户仍要更新"三者同时成立时有用，不做；离线时没有元数据就不跑。
- 目录单元允许少量不受管文件、提交后逐个搬回：几百个用户数据文件的目录会退化成几百次搬运，收益消失。
- 只枚举候选目录、其余文件仍逐个 stat：省下的是候选之外的枚举，付出的是每个文件一次经过过滤驱动的打开；小文件多的应用里后者是计划阶段仅次于哈希的成本，整体枚举反而更便宜。
- 以本地清单缓存（路径、大小、修改时间、哈希）跳过未变文件的哈希：任何独立于扫描的缓存都可能让更新漏掉文件，风险远大于收益；扫描是唯一真相来源。
- 为目录单元中未变更的受管文件建硬链接以放宽 100% 待写的条件：每文件一次元数据操作，且引入硬链接语义；候选集足够覆盖全新安装与整目录替换两个主要场景。
- SSH / SFTP 路径 e2e：`tests/sshd/` 夹具已存在，但该路径流量占比约 3% 且 `capabilities/ssh.rs` 有 Rust 单测，不纳入。
- UI 自动化：本提案验证的是文件系统结果，silent 路径即可断言。

## Acceptance criteria

- `rg -n "File::create\(" src-tauri/src` 的命中仅限 `fs.rs` commit 模块中写 `new\` 的位置、`#[cfg(test)]` 代码与 `src/builder/`；`create_target_file`、`prepare_target` 不再存在。
- `DELETE_SELF_ON_EXIT_PATH` 的写入点全库唯一，位于 commit 模块所有 rename 成功且 journal 删除之后；`mirrorc.rs` 与 `run_uninstall` 不再写它。
- `rg -n "set_current_dir|env::temp_dir" src-tauri/src --glob '!**/builder/**'` 的命中仅限 `fs::staging` 模块、`utils/log.rs` 的日志文件路径、`host/webview.rs` 的 WebView2 用户数据目录与 `#[cfg(test)]` 代码。
- 自卸载单测：以 `FILE_SHARE_READ | FILE_SHARE_DELETE` 打开的文件模拟运行中的卸载器，`run_uninstall` 后该文件位于暂存目录 `old\` 下、`DELETE_SELF_ON_EXIT_PATH == Some(暂存目录)`；安装目录位于盘根时返回安装位置不安全错误且文件未移动。
- `.instbak`、`.patching`、`.patchold`、`with_extension("old")`、`RmList`、`KachinaInstaller_Mirrorc_` 在 `src-tauri/src` 中不存在；`kachina-tmp` / `kachina-old` 仅出现在 commit 模块的复制单元分支。
- `rg -n "remove_file\(" src-tauri/src --glob '!**/builder/**'` 的命中仅限 `fs::staging` / commit 模块（清理暂存目录）、`installer/uninstall.rs` 的卸载删除与 `#[cfg(test)]` 代码；安装与更新路径不再直接删除安装目录内的文件。
- 暂存路径推导单测：同卷取 `%TEMP%\kachina-staged\<h>`，不同卷取 `<安装目录>.kachina-staged`；`<h>` 对大小写与斜杠不同的同一路径相同。
- `CheckLocalFiles` 的 stat 来自目录枚举：以计数器断言对受管文件不再调用 `metadata()`（`userDataPath` / `ignoreFolderPath` 下的 `skip_hash` 文件除外），返回的大小与修改时间与逐个 stat 一致。
- 目录单元探测单测（`fs.rs`）：候选目录根下有一个不受管文件而某子目录完全受管待写时，输出仅含该子目录；含 `userDataPath` 的候选不干净；含空目录的候选不干净；含未变更受管文件的候选不在输入中；删除清单中的文件不使候选变脏；每个目录只被枚举一次（以计数器断言）。
- 提交单测（commit 模块）：三文件计划，阶段一第二个文件写入失败时三个目标字节均不变且暂存目录无 journal；阶段二第二个单元 rename 被注入失败时三个目标字节均等于旧文件、暂存目录已删除；阶段二在第二个单元之后被注入中断时 journal 存在，调用恢复后三个目标均为新文件、暂存目录已删除；`new\` 被删除后调用恢复时已换过的单元被复原、暂存目录已删除；目录单元的目标目录内有文件被 `share_mode(0)` 锁定时该单元退化为逐文件提交且其余文件成功换入；根目录不存在与根目录为空两种情形各一例；空间不足路径下 `staged == false` 且逐文件即时提交。
- 自更新单测：以 `FILE_SHARE_READ | FILE_SHARE_DELETE` 打开的文件模拟运行中的 exe，第二个 rename 被注入失败时原名文件字节等于原文件、`DELETE_SELF_ON_EXIT_PATH` 为 `None`；成功时 `DELETE_SELF_ON_EXIT_PATH == Some(暂存目录)` 且 `old\` 下的旧 exe 字节等于原文件。
- Mirror酱 单测（`thirdparty/mirrorc.rs`）：合成 zip（含 `changes.json` 的 `deleted` 项、一个与当前 exe 同名的文件、一个子目录）解压后全部产物位于 `new\` 下、安装目录字节不变、返回的删除清单与 `changes.json` 一致；`.metadata.json` 变体同理；zip 摘要与给定 `sha256` 不匹配时返回错误且 `new\` 为空；二者皆无时报归档无效。
- 探测单测补充：候选目录下的 junction 子目录使候选变脏且其内受管文件成为复制单元；提交前重新探测时向已判干净的目录写入一个文件，该目录单元降级为逐文件提交且该文件保留在原位。
- 复制单元单测：以 junction 指向另一临时目录（测试内同卷即可，逻辑与卷无关），提交后目标文件为新内容、`.kachina-tmp` / `.kachina-old` 已清理；注入第二个 rename 失败时目标为旧内容。
- journal 版本单测：首行为 `kachina-journal 0` 或缺失时恢复不执行任何 rename、暂存目录被删除。
- 恢复核对单测：三单元 journal，阶段二在第二个单元后中断——目录未变时恢复完成剩余单元、暂存目录删除；第三个单元的目标被替换为任意内容（模拟便携版覆盖）时不执行任何 rename、暂存目录删除、被替换的内容保留；第一个单元（已换）的目标被改动时同样整体丢弃；删除单元的目标被用户重新放回同哈希文件时视为未变；旧哈希为 `-` 的尚未换单元视为有变。
- 每个产出文件在校验通过后调用 `sync_all`：以注入的写入器计数断言。
- 删除单元单测：三文件计划含一个删除项，提交后该文件位于 `old\` 下、目标不存在；阶段二被注入中断后恢复，删除项同样完成；回滚时该文件回到原位。
- `ipc/manager.rs` 单测：`pipe_roundtrip_and_disconnect` 增加一个变体，服务端在回 `Ok` 前发送 1000 条 `Progress`，客户端 `run` 返回 `Ok` 而非 `IPC_ERR`。
- 新增 e2e 七项（`online-install-dfs2`、`online-update-dfs2`、`update-corrupt-updater`、`update-interrupted`、`update-interrupted-commit`、`update-interrupted-then-overwritten`、`install-cross-volume`）加入 `test:all` 并全绿；现有十项全绿。
- `update-corrupt-updater` 在本提案落地前的构建上运行必须失败（`updater.exe` 消失或哈希不等于 v1），证明用例能抓到原问题。

## Risks

- 磁盘峰值从"单个文件的目标 + 暂存"变为"现有安装 + 本次全部变更"：空间不足时退化为逐文件即时提交，此时中断仍会留下混合版本，界面需据 `staged: false` 提示用户。
- 每个产出文件一次 `sync_all`：大文件是一次顺序刷盘，与下载耗时相比可忽略；数万个小文件时每次毫秒级，累计秒到十秒级，接受。
- 复制单元跨链接边界不原子：只影响用户自行建立 junction 的子树，且子树内单个文件仍原子；不做复制模式则这些用户从"能装"变成"装不了"。
- journal 版本不全等时直接删除暂存目录：若此时阶段二已进行到一半，混合状态会保留到哈希扫描修复完成；格式版本只在安装器自身升级且恰好跨越一次中断的提交时才会不等，接受。
- 阶段二窗口内用户手动启动应用仍会看到混合状态：窗口长度是单元数次本地 rename，目录单元把最常见的全新安装缩为一次；散落变更的更新仍是 N 次，几千个文件为秒级。彻底消除窗口需要应用侧的版本目录布局。
- 以不同用户账户运行下一次更新时 `%TEMP%` 不同，找不到上次的 journal：哈希扫描看到混合状态后走正常修复重新下载，另一用户 `%TEMP%` 下的暂存目录成为孤儿，接受。
- `%TEMP%` 被系统磁盘清理清除后 journal 与 `new\` 一起消失：等同于无暂存目录，阶段二中断留下的混合状态由用户下一次主动更新时的哈希扫描修复。
- 恢复核对以哈希相等判定"未变"，用户把某个文件替换成恰好同哈希的内容不会被识别为改动：这等价于没有改动，无害。
- 同一安装目录被两个安装器并发处理：`lock` 让后来者报错退出；pid 复用导致误判的概率可忽略。
- 目录 rename 在目标目录内有打开句柄时失败：退化为逐文件提交，只损失优化不引入新失败模式。
- 安全软件持有新文件超过重试窗口（合计约 1.5 秒）时以文件占用报错：用户重试即可，原文件完好。
- 自更新的两次 rename 之间仍有一个原名缺失的瞬间：毫秒级本地操作，第二次失败有复原；进程恰在此刻被强制结束时新旧两份都在暂存目录与 `old\` 中，下次启动前滚。
- Mirror酱 路径无 e2e：API 域名写死在 `get_mirrorc_status`，无法以本地 stub 替代；解压、删除单元与自更新由合成 zip 的单测覆盖，提交与恢复与 DFS 路径共用同一实现与测试。
- Mirror酱 增量包只把被整体覆盖的子目录判为目录单元，散落变更仍是逐文件 rename：与 DFS 路径一致，接受。
- DFS2 stub 与线上服务的偏差：stub 只实现 `dfs.rs` 类型定义的字段与 `solve_dfs2_challenge` 支持的 md5 challenge；服务端新增字段或 challenge 类型不会被 stub 覆盖。
- `update-interrupted` 的 kill 时机：以 `?delay` 拖慢每个 chunk 保证 2 秒时仍在下载；若 CI 机器极慢导致 2 秒时尚未开始写入，用例退化为普通重跑，不会误报失败。
- 卷判定以 `GetVolumePathNameW` 的卷根比较，`subst` 与网络映射盘会被判为不同卷而走同级暂存目录：结果仍然正确（同级一定同卷），只是少了 `%TEMP%` 的不可见性；真正跨卷的 rename 从未发生。
- H3 / QUIC 下载路径流量占比约 20–30%，本提案不含其 e2e：需要 QUIC 服务端夹具（Node 无内置实现，需如 `tests/sshd/` 一样用 Go 编写），是否纳入另行决定。
- 提权管道改 `oneshot` 后，`ManagedElevate` 被丢弃时需向所有未完成的 `oneshot` 发送 `Disconnect`，遗漏会让等待方永久挂起；单测中"丢弃 mpsc 发送端后等待方收到 Disconnect"的既有用例覆盖此路径。
