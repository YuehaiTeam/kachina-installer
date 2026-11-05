# 共享包读取与边写边校验的写出管线

Status: implemented

## Problem

安装包格式的读取有四份实现：安装器、builder、sfx 各自的 TLV 扫描（`native/local.rs`、`native/builder/local.rs`、`native/sfx/local.rs`），以及远端 packed 源取回头部段后的逐字节扫描（`session/source.rs` 的 `refresh_packed_index`）。安装器的读取器为格式化两条错误引用了 `utils::error`，后者依赖网络模块，builder 与 sfx 因此不能经 `#[path]` 复用它。归档相对路径校验 `relative_under_root` 在 builder 解包与 sfx 中也各有一份。

三份本地扫描都从文件开头逐字节查找 `!IN\0` 直到文件末尾，再按 TLV 长度排除落在数据块内容里的命中，启动时要读完整个包：一个 477 MiB 生产离线包的 `kachina-builder extract --list` 用时 957 ms（条件见 Verification）。某一项的内容长度损坏偏大时，扫描会跳过其后完好的项。安装器解析 `\0INDEX` 时不检查剩余长度，索引截断即 panic。

`replace-bin --base` 换上的 base 不带包内应用的名称、窗口标题与图标。三个二进制共用的资源脚本 `resources/app.rc` 没有 `VERSIONINFO`，而 rcedit 只改写已有的 StringFileInfo，pack 写入的 `windowTitle`、`appName` 同样进不了产物。

安装器每写出一个暂存文件要打开三次：写完关闭，重新打开把全文件读一遍算 hash，再以写权限打开一次做 `FlushFileBuffers`；需要抹布局头时，算 hash 之前还要打开一次改写文件开头 256 字节。重读拿到的是刚写入页缓存的同一份数据。zstd 解码与写文件用异步的 `ZstdDecoder` 与 `tokio::fs::File`：16 路本地解压的 CPU 解码跑在异步工作线程上，每次写入都经 `spawn_blocking` 调度。合并请求在同一个异步任务里依次读一个成员、写盘、重读、刷盘，期间网络流停读。

hpatch 的集成层三路缓冲各 4 KiB；输出流容量写死为 1 GiB，产物超过 1 GiB 的补丁必然失败；读 old 文件每次都先 seek；回调里的 io 错误只打印到 stdout，网络取消与连接错误到调用方只剩一个失败码。

本地扫描为每个待 hash 文件建一个异步任务、调一次 `spawn_blocking`、新分配一块 1 MiB 缓冲，并把全部文件按大小降序重排，打乱目录内的读取顺序。计划删除的文件不在扫描清单里，journal 的 `del` 行旧 hash 恒为 `-`；提交在删除单元执行之前中断时，恢复把仍在原处的待删文件判为"有变"，整次恢复放弃。

合并请求中某个成员因取消而失败时，会话把它当普通失败处理，重试合并请求或退回逐文件请求。分片读取（`download::sliced`）每读一次都新分配并清零 64 KiB 缓冲。

## Decision

### 共享包读取

[native/pack_format.rs](../../../native/pack_format.rs) 是包格式读取侧的唯一实现，只依赖外部 crate：安装器直接 `mod`，builder 与 sfx 经 `#[path]` 引入。它包含布局头、TLV 与 `\0INDEX` 的解析、`scan_embedded`、布局头抹除 `clear_layout`、数据块命名规则 `preferred_file_hash` 与归档相对路径校验 `relative_under_root`；pack 写端的魔数与布局标记也取自这里的常量。[native/local.rs](../../../native/local.rs) 只保留安装器自身的 mmap、缓存与配置解析，[native/builder/local.rs](../../../native/builder/local.rs) 只保留 bundle 内的 PE 映像定位。[远端 packed 源](../../../native/session/source.rs)取回头部段后用同一组函数解析，索引截断时报 `METADATA_INVALID`。

`scan_embedded` 按以下规则列出内嵌项：

- 首个 TLV：DOS stub 布局头的 `base_end` 处是合法 TLV 时取它。否则依次试 PE 节数据末尾、紧随节数据的签名表末尾（先签名再打包的 base）；纯在线包与卸载器、更新器的布局头为零，落在这一步。节数据末尾已到 payload 末尾时是未打包的裸程序，返回空。都不命中时，从节数据末尾（非 PE 从文件开头）向后找第一个内容末尾能接上下一项的 TLV 头。
- payload 末尾：PE 安全目录恰好占据文件尾部且位于节数据之后时，payload 止于签名表起点，打包后追加的 Authenticode 签名不算内嵌项；末项内容与签名表之间允许不足 8 字节的补零。
- 跳步：沿 TLV 链按内容长度前进，不读数据块内容。每项内容末尾须落在 payload 末尾、补零或下一个合法 TLV 头上；落不上时丢弃该项，从其内容起点向后找下一个能接上的 TLV 头；找不到时该项是最后一项，其后是不属于包的数据，保留该项。
- `\0INDEX`：索引里位于 `\0INDEX` 之后的条目首尾相接、从 `\0INDEX` 末尾开始、末尾接上 payload 末尾或下一个 TLV 头，且首末两项的 TLV 头与索引一致时，直接采用索引，其余数据块的 TLV 头不读；`append` 追加在其后的项继续逐项读取。截断的包不满足这些条件，回到逐项读取，缺块由[启动校验](../../../native/session/commands.rs)比对索引与实际内嵌项后报 `PKG_BROKEN`。
- `parse_index` 逐项检查边界，任一项截断、名字不合法或偏移小于 TLV 头长度时返回 `None`，安装器启动报 `PKG_BROKEN`。

### replace-bin 与版本信息

[replace-bin](../../../native/builder/replace_bin.rs) 以 `scan_embedded` 的首项作 payload 起点。换头前按输入包 `\0CONFIG` 的 `windowTitle`、`appName` 用 rcedit 写入新 base 的 `FileDescription`、`ProductName`，与 pack 写入的字段相同；带 `--icon` 时同时写图标，图标路径不存在直接报错。

[resources/app.rc](../../../resources/app.rc) 带 `VERSIONINFO`（StringFileInfo `040904B0`），字段取值与 tauri-build 生成的一致：`ProductName`、`FileDescription` 为 "Kachina Installer"，`CompanyName` 为 "kachina"，`LegalCopyright` 为 "Built by Kachina Installer"；`FILEVERSION`、`PRODUCTVERSION` 与对应的版本字符串取 Cargo 包版本，由 [build.rs](../../../build.rs) 生成 `OUT_DIR/app_version.h`。pack 与 replace-bin 写入的版本字符串依赖这一段存在。app.rc 只用 ASCII，rc.exe 按构建机的 ANSI 代码页读取它。

### 写出管线

[native/verified_write.rs](../../../native/verified_write.rs) 是安装器与 sfx 共用的写出校验。`HashingWriter` 写入目标的同时计算 md5 或 xxh（md5 优先，与数据块命名一致）并累计字节数，每次 `write` 先检查取消；需要抹布局头时先攒下文件开头 512 字节，经 `clear_layout` 抹掉布局头字段后再 hash 与写出。`finish` 在有期望 hash 时核对字节数与 hash；没有期望 hash 时两者都不核对。

[native/ipc/install_file.rs](../../../native/ipc/install_file.rs) 在异步侧把来源打开为原始字节：本地块是 mmap 切片，网络与分片来源是未解压的流，经 `SyncIoBridge` 桥接成同步读取。zstd 解码（同步 `Decoder`，每块一帧）、补丁、hash 与大小核对、`sync_all` 都在一个 `spawn_blocking` 任务里用创建文件的那个句柄完成，随后关闭。`InstallFile` 与合并请求写出的暂存文件只打开一次，不回读；任一步失败时在阻塞任务内删除该文件。

- 进度：直写按写出字节回报（分母 `output_size`），补丁按 diff 读取字节回报（分母 `diff_size`），均 20 ms 节流，与 `transfer_total` 一致。
- HybridPatch：`.hybrid-base` 以读写方式打开一次，解压写入后 `rewind`，作为补丁的旧文件；diff 请求在 base 写完之后才发起，不占着请求名额等 base 解压。
- 取消：下载任务的取消令牌是 task-local，不跨 `spawn_blocking` 与 `tokio::spawn`，由 `spawn_job` 在异步侧取出后交给阻塞任务；写入端每次 `write` 检查，网络读端沿用 `network::Reader` 的取消。
- 合并请求：每个成员从共享响应里切出 `Vec<u8>` 交给阻塞任务，至多 4 个同时在写，网络侧继续读下一个成员；按成员顺序等待最早的任务，结果保持成员顺序。会话侧（[session/run.rs](../../../native/session/run.rs) 的 `failed_chunks`）遇到因取消失败的成员时整体按取消返回，不重试合并请求，也不退回逐文件请求。
- 错误码：读源的错误原样传播，由调用方按类型推断码；zstd 解不开的数据为 `DECOMPRESS_ERR` 并挂 `HASH_MISMATCH`（解码器包着源，源自己的读错不算解不开）；写错为 `WRITE_TARGET_ERR`；大小或 hash 不符挂 `HASH_MISMATCH`；`sync_all` 失败挂 `FILE_IO_FAILED`；补丁失败为 `PATCH_FAILED_ERR`，diff 解不开时同样挂 `HASH_MISMATCH`，由取消引起的按取消处理。

[native/fs.rs](../../../native/fs.rs) 的 HTTP、已解析来源与本地流辅助函数只返回原始字节。运行库安装器与 Mirror酱 归档的下载走异步写出（`create_staged_file` 与 `progressed_copy`）。卸载器、更新器镜像抹布局头用 `clear_layout`：读文件开头 512 字节，找到完整布局头才写回。

### hpatch

[hpatch-sys](../../../libs/hpatch-sys/src/lib.rs) 的工作区为补丁的 `stepMemSize` 加三路各 64 KiB（`hpatch_kFileIOBufBetterSize`）。输出流容量为 `hpatch_kNullStreamPos`（`u64::MAX`；bindgen 不展开这个宏，在 lib.rs 里定义），产物大小只受补丁头声明的大小约束。old 流记录游标，读取起点等于游标时不 seek。三个回调 wrapper 各自保存首个 `io::Error`，`safe_patch_single_stream` 返回 `io::Result<()>`：有回调错误时原样返回，网络取消与连接错误因此仍可识别；否则把 HPatch 的失败码转成 `ErrorKind::Other`。

### 本地扫描

[check_local_files](../../../native/fs.rs) 起每个 CPU 一个阻塞线程，按原子下标领取文件，各自复用一块 1 MiB 读缓冲；≥1 MiB 的文件按大小降序排在前面，其余保持目录枚举顺序；工作线程提前退出（panic）时报 `HASH_THREAD_ERR`。`CheckLocalFiles.delete_list` 带入计划删除的文件：存在的待删文件被 hash，journal 的 `del` 行记下其旧 hash；它们不算受管文件，目录是否干净的判定不受影响。`probe_writable` 在一个阻塞任务里同步逐个打开。

[download::sliced](../../../native/ipc/download.rs) 用一块 `BytesMut` 读取：每次 `reserve(64 KiB)` 后 `read_buf` 读进空闲容量，`split().freeze()` 切走发送；小读共享一块分配，接收方释放先前的块后 `reserve` 可以回收原分配。

## Alternatives considered

- 抽单一 `[lib]` 供三个 bin 链接：单 package 下编译任一 bin 都要先编译整个 lib，包括只有安装器用到的 webview2、reqwest、h3、russh 相关模块，且要改全仓的 `crate::` 路径。切断读取器对网络模块的依赖后，`#[path]` 已能共享。
- 共享读取放进 `native/local.rs`：该文件还承载安装器专用的 mmap、缓存与 `RepoMetadata` 解析，builder 与 sfx 引入后要么带上这些依赖，要么加条件编译分叉。
- 只用 DOS stub 布局头定位首块：纯在线包、卸载器与更新器（布局头为零）和测试用的假 base 都定位不到。
- 只用 `\0INDEX`、不走 TLV 链：漏掉 `append` 追加在索引之后的项，也读不了布局头被清零的映像。
- `\0INDEX` 快路径逐个核对全部数据块的 TLV 头：每个数据块头都要触碰一页，大包启动的读盘量随文件数增长。只核对首末两项的代价见 Consequences。
- 块长度失步时整个包解析报错：一个坏块会让 `\0CONFIG` 与其后的完好文件都无法经 `extract` 取出；重同步只在坏块区间内扫描，完好的包没有额外开销。
- 计划删除的文件并入 `file_list`：待删文件会算作受管文件，只含受管文件与待删文件的目录会被判为干净并作为目录单元整体换入，而目录单元的恢复要求目录内容恰好等于 journal 列出的文件，待删文件会让恢复判为"有变"。
- `sliced` 把 64 KiB 缓冲移出循环复用：缓冲以 `Bytes` 交给接收方，所有权随消息离开，无法复用。
- 写出沿用异步解码与 `tokio::fs::File`：每次写入要跨线程调度，CPU 解码占用异步工作线程。
- 写完重读校验：重读拿到的是刚写入页缓存的同一份数据，发现不了介质错误；边写边算的 hash 覆盖解码、补丁与写出，加上字节数核对，能发现的问题与重读相同。
- 写出前由一个线程预建全部目标文件句柄：同一组 1787 个空文件（取自一个 476 MiB 生产包的目录结构，93 个目录已预建），单线程逐个创建并关闭 1094 ms，16 线程并发 318 ms（C# `Parallel.ForEach` 经 PowerShell `Add-Type`，`%TEMP%` 在 C 盘，32 逻辑处理器，Windows 11 Insider 26H1 build 28020）。本地写出已是 16 路并发，各任务在自己的阻塞线程里建文件，单个预建线程赶不到写出任务前面；联网安装时写出任务在等网络，建文件不在关键路径上。预建还要按路径登记写者，避免与重试、回退时的 `File::create` 互相截断。

## Verification

| 判据 | 结果 |
|---|---|
| 包格式读取只有一份实现；`relative_under_root` 由 builder 与 sfx 共用 | PASS：`native/sfx/local.rs` 不存在，`native/builder/local.rs` 只剩 bundle 映像定位，远端 packed 源用 `pack_format` 的解析函数；路径校验单测为 `pack_format` 的 `relative_path_guard` |
| 新旧读取对有效包得到相同条目、配置与偏移 | PASS：本机三个生产包（下游应用甲的两个相邻版本与应用乙的一个版本），改动前（`9fd6ce3` 构建的 builder）与改动后 `extract --list` 的输出逐行相同：甲旧版 1671 项、甲新版 1782 项、乙 146 项 |
| 布局头为零、签名在打包前或后、追加条目、伪魔数、长度损坏、截断包、索引截断 | PASS：`pack_format` 单测 `online_pack_located_at_pe_end`、`cleared_layout_header_located_at_pe_end`、`fake_base_scans_to_first_chained_block`、`bare_exe_has_no_entries`、`signature_appended_after_pack_is_not_an_entry`、`base_signed_before_packing`、`blocks_appended_after_index`、`magic_inside_content_is_not_an_entry`、`broken_length_resyncs_to_next_block`（长度 ±3）、`last_block_followed_by_unknown_data_is_kept`、`truncated_pack_drops_out_of_bounds_entries`、`index_parse_rejects_truncated_entries`、`valid_index_is_used_without_reading_data_headers` |
| 真实签名文件的安全目录布局与 payload 末尾的判定一致 | PASS：`node.exe`、`pwsh.exe` 的安全目录起点等于节数据末尾、终点等于文件长度 |
| 大离线包首次解析耗时 | `extract --list` 墙钟时间中位数（含进程启动与 `\0META` 解析）：甲旧版（477 MiB）957 → 38 ms，甲新版（476 MiB）1041 → 39 ms，乙（406 MiB）801 → 24 ms。改动前为 `9fd6ce3` 的 `cargo build --release --bin kachina-builder`（x86_64-pc-windows-msvc），改动后为 `pnpm build` 产出的 `kachina-builder-standalone.exe`（x86_64-win7-windows-msvc，build-std），两侧 target 不同；PowerShell `Measure-Command` 各 5 次，页缓存已热，无其他负载，Windows 11 Insider 26H1 build 28020 |
| 小在线安装器首次解析不退步；读取量 | 未测量 |
| replace-bin 写入版本字符串与图标 | PASS：`replace-bin --base kachina-sfx.exe --icon red.ico`（纯红 32×32）产物的关联图标中心像素 ARGB 为 (255,255,0,0)，不带 `--icon` 的产物与裸 `kachina-sfx.exe` 同为 (255,129,93,83)，产物 `extract --list` 与原包一致；release e2e 中 pack 产物、replace-bin 产物与 sfx 便携包的 `FileDescription`、`ProductName` 等于包内的 `windowTitle`、`appName` |
| 三个二进制带版本信息 | PASS：debug 构建的 installer、builder、sfx 的 `FileDescription`、`ProductName` 为 "Kachina Installer"，`FileVersion`、`ProductVersion` 为 "0.1.0"，`CompanyName` 为 "kachina"，`LegalCopyright` 为 "Built by Kachina Installer" |
| 写出校验：抹布局头、大小与 hash 核对、取消、读错回传 | PASS：`verified_write` 单测 `layout_header_is_cleared_before_hashing_across_split_writes`（布局头跨 7 字节分块写入，仍在 hash 前抹掉）、`short_file_with_clearing_is_still_written`、`size_and_hash_mismatches_are_reported`、`cancellation_stops_the_copy`；`install_file` 单测 `patch_returns_the_diff_reader_error`（diff 读到一半报 `ConnectionReset`，错误链里是同一 kind 的 `io::Error`）、`cancelled_patch_reports_cancel`、`compressed_direct_write_clears_the_layout_header`；`pack_format` 单测 `clear_layout_zeroes_only_the_layout_fields` |
| hpatch 往返与进度；产物超过 1 GiB | PASS（往返）：`patch_output_is_verified_and_progress_reaches_the_diff_size`（1 MiB 伪随机旧文件，新文件前后两半对调并插入内容，hdiff-sys 生成补丁，输出逐字节一致，最终进度等于 diff 长度，期望 hash 不符时挂 `HASH_MISMATCH`）。超过 1 GiB 的产物未测，依据是 HPatch `patch.c` 只在 `newDataSize > streamSize` 时拒绝 |
| 本地扫描：hash 结果不变、在途数量有界、计划删除被 hash 且不算受管 | PASS：`check_local_files_hashes_every_file_once`（41 个文件含一个 2 MiB 文件，逐个对照 md5）、`planned_deletes_are_hashed_but_stay_unmanaged`；工作线程数为 CPU 数 |
| 合并请求遇取消不重试、不回退 | PASS：`cancelled_chunk_cancels_the_merged_request` |
| 暂存文件句柄不跨到提交阶段 | 代码审查：`InstallFile` 与合并请求的输出只在 `create_staged_file_blocking` 打开一次，`commit_output` 在同一句柄 `sync_all` 后随 `File` 析构关闭；阻塞任务返回前全部句柄已释放 |
| 单测、格式与 lint | PASS：`cargo test --bins` builder 39、installer 208（1 ignored）、sfx 40，改动前为 25 / 179 / 21；`cargo fmt --all -- --check` 无差异；`cargo clippy --all-targets` 0 warning |
| e2e | PASS（release，`pnpm build`，运行 e2e 的 shell 没有 `DEV` 环境变量）：prepare、offline-install、online-install、offline-update、online-update、dfs2、updater-survival、already-latest、uninstall、userdata-ignore、occupied-process、builder-extract-replace、plugin-stub、sfx-run。v2 fixture 含 2 个补丁（app.exe、updater.exe）与 1 个删除（readme.txt），online-update 的日志含 4 个文件的合并请求、两个补丁与 `local_scanned=7`（含待删文件）。machine-acl、registry 未运行：需要提权会话 |
| 扫描、补丁与写出耗时；二进制体积 | 未测量。写出耗时要用生产包做离线安装，会在本机写入第三方应用的注册信息与运行库 |

## Consequences

- 有效 `\0INDEX` 下只核对首末两个数据块的 TLV 头，中间数据块的 TLV 头损坏不在启动时报 `PKG_BROKEN`。内容按索引偏移读取，只坏了 TLV 头的块照常安装；损坏延伸到内容时，安装时的 hash 校验报 `HASH_MISMATCH`。换来的是启动不触碰数据块所在的页。
- 写出校验覆盖写出流：hash 与字节数核对能发现解码、补丁与写出的错误，发现不了写入后介质上的位翻转，重读页缓存同样发现不了。没有期望 hash 的写出不核对大小；HybridPatch 的 base 中间文件属于这种情况，其内容由补丁产物的 hash 兜底。
- 合并请求至多 4 个成员同时在写，这些成员的内容同时在内存里；成员是合并接纳的小文件。
- 本地扫描每个 CPU 一个阻塞线程、各持 1 MiB 缓冲，32 逻辑处理器的机器上是 32 个线程与 32 MiB。
- 每个补丁的工作区为 `stepMemSize` 加 192 KiB。
- pack 与 replace-bin 只改写 `FileDescription`、`ProductName`；产物的 `CompanyName`、`LegalCopyright` 与版本号是 base 自带的值，不随应用变化。
- `pack_format.rs` 只依赖外部 crate，`verified_write.rs` 另外只依赖 `pack_format`；两者引用其他 `crate::` 路径时，builder 或 sfx 就无法经 `#[path]` 引入它们。
