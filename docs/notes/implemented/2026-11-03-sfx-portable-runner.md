# 自解压免安装运行器（kachina-sfx）

Status: implemented

## Problem

本项目的主程序是安装器：把应用文件在线下载或离线解压到目标位置后结束，并写入注册表与快捷方式。分发侧还需要一种自解压程序：纯离线把应用数据解到临时目录下不重名的新目录、直接运行应用主体、等应用退出后删除临时目录；包的结构与安装包一致（程序+配置+数据），配置与安装器兼容，使得现有离线安装包仅更换头部程序即可变成自解压程序。现有安装器没有"解到临时目录直接运行"的入口，[包格式读取](../../../native/pack_format.rs)和[带路径防护的解包](../../../native/builder/extract.rs)具备读取这种包所需的全部能力，缺的是一个只做解包、起进程与清理的程序。

## Decision

### 新 bin 与共享代码

第三个 bin `kachina-sfx` 的入口是 [native/sfx/main.rs](../../../native/sfx/main.rs)，在 [Cargo.toml](../../../Cargo.toml) 中登记为 `[[bin]]`，[build.rs](../../../build.rs) 的图标资源目标列表包含它。sfx 经 `#[path]` 与安装器共享[包格式读取](../../../native/pack_format.rs)、[边写边校验的写出](../../../native/verified_write.rs)、[进程拉起](../../../native/utils/process.rs)、[提权启动](../../../native/utils/uac.rs)、[快捷方式写出](../../../native/utils/shell_link.rs)与[元数据](../../../native/utils/metadata.rs)；前两个模块的设计见[共享包读取与边写边校验的写出管线](./2026-11-04-pack-reader-and-write-pipeline.md)。sfx 不接日志，也不往 stdout 写任何东西：被包装的应用继承同一个控制台。sfx 不链接 reqwest、h3、russh、webview2 与安装器的提权 IPC。

安装器侧的 taskdialog.rs、staging.rs 不能整体 `#[path]` 引入：它们分别经 utils/code.rs 拖入 fs/dfs、经 session::plan 拖入会话模块。sfx 因此自带 [dialog.rs](../../../native/sfx/dialog.rs)（TaskDialogIndirect 的错误框与进度条最小实现）与 [reaper.rs](../../../native/sfx/reaper.rs)。

### 包格式与换头

包格式不变：sfx.exe 作 base，其后依次是 `\0CONFIG`、`\0INDEX`、`\0META` 与 hash 命名的 zstd 数据块。[replace-bin](../../../native/builder/replace_bin.rs) 的 `--base <path>` 显式从文件读取新 base；缺省取 bundle 末位 PE，因此 [merge-release-bundle](../../../scripts/merge-release-bundle.mjs) 与 `pack` 流程不变。INDEX 内 offset 相对 payload 起点，换头后保持有效。换头时按包内 `\0CONFIG` 把应用名与窗口标题写进新 base 的版本信息，`--icon` 写入图标：

```bat
kachina-builder.exe replace-bin App.Install.exe --base kachina-sfx.exe --icon App.ico -o App.Portable.exe
```

### 运行时流程

1. 启动时先由 reaper 清理历史残留；随后 mmap 自身，以共享的 `scan_embedded` 列出内嵌项。
2. 解析 `\0CONFIG` 的子集：`exeName` 必需，`appName`/`windowTitle` 用于对话框文案，`uacStrategy` 按下面的表格解释；serde 忽略其余字段，配置零新增字段。sfx 不直接读 `\0INDEX`：`scan_embedded` 已给出包括数据块在内的全部内嵌项的绝对偏移。无 `\0META`、`hashed[]` 为空、或 meta 引用的数据块缺失时报错退出，纯在线包因此被拒绝；`patches[]`、`deletes[]` 与 `\0IMAGE` 忽略。
3. 校验：meta 中的 fileName 与 `exeName` 都是包内输入，不可信。每个 `hashed[i]` 的路径经 `relative_under_root` 校验（只允许普通路径组件），并以 `preferred_file_hash`（md5 优先、其次 xxh）在扫描结果中定位数据块；`exeName` 按同一规则校验，且须是解压计划中的文件（不区分大小写）。任一项不合法即报错退出，此时还没有创建临时目录。
4. 创建 `%TEMP%\kachina-sfx-<uuid>\`（`create_dir` 遇已存在即换名重试，uuid 使同包多实例并发互不干扰）。此后的任何失败都先删除临时目录再报错。
5. 解压：先建好全部父目录，再起 min(CPU 数, 16, 文件数) 个线程按解出大小降序领取文件，最大的文件最先开始。每个文件以同步 zstd 解码 mmap 中的数据块，经 `HashingWriter` 边写边算 hash，写完核对大小与 hash，再从写入的句柄读回核对；临时目录里的文件不刷盘。任一文件失败时先记下错误再置停止位，其余线程在下一次写入时停下，报最先记下的错误，失败的文件当场删除。解压前按 Σsize + 10% 预检卷剩余空间，解压期间在非控制台场景显示 TaskDialog 进度条，只在整数百分比变化时更新。
6. 取消：点进度框的取消按钮或关掉进度框都算取消。解压线程在下一次写入时停下，解压完成后再检查一次；取消后删除临时目录、不拉起应用，不弹错误，以退出码 1 结束。
7. 提权遵循 `uacStrategy`，在第 3 步校验之后、创建临时目录之前决定。sfx 的 manifest 恒为 `asInvoker`；需要提权时 sfx 以 `ShellExecuteExW` 的 `runas` 重启自身，命令行是 `--kachina-sfx-elevated` 加原样的用户参数，由提权后的实例解压并拉起应用，原实例等它退出并透传退出码。带该标记的实例不再提权，也不把标记传给应用。不使用安装器的 `headless-uac` 与命名管道：

| `uacStrategy` | sfx 行为 |
|---|---|
| `prefer-user` | 不提权 |
| `prefer-admin`（缺省，未知取值同义） | 未提权时提权重启；用户取消（`ERROR_CANCELLED`）则在原实例内以普通权限继续 |
| `force` | 未提权时提权重启；用户取消即报错退出 |

已经提权的 sfx（如经 gsudo 启动）直接在本实例内解压与拉起。提权重启的 cwd 由 `ShellExecuteExW` 继承；调用者会话里设置的环境变量不跨越提权，与直接经 UAC 启动一个程序的行为一致。

8. 以 `CreateProcessW` 拉起 `<temp>\<exeName>`：cwd 继承 sfx 的调用者，命令行参数原样透传。应用的环境在 sfx 的环境之上加 electron-builder portable 目标的三个变量：`PORTABLE_EXECUTABLE_DIR` 为 sfx 本体所在目录，`PORTABLE_EXECUTABLE_FILE` 为 sfx 本体路径，`PORTABLE_EXECUTABLE_APP_FILENAME` 为 `appName`（为空时取 `exeName` 去掉扩展名）。
9. 任务栏身份：解压完成后、拉起前，由 shell 的 `IShellLinkW` 在临时目录根部写 `<appName>.lnk`，目标是 sfx 本体、不带参数、起始位置是 sfx 所在目录，并以 `STARTF_TITLEISLINKNAME` 把它作为应用的来源快捷方式。任务栏把应用窗口认作 sfx 本体、显示 lnk 的名称，固定到任务栏时拷贝的是这个 lnk，临时目录删除后固定项仍启动 sfx。同名文件已存在（按文件系统的大小写规则，即包内文件）、名称清洗后为空或写入失败时不带来源快捷方式启动，不覆盖已解出的文件。应用自己设置显式 AppID 时它的 AppID 优先，这类应用要固定到 sfx 本体需自行在窗口上设置重启命令（Electron 的 `setAppDetails`），路径取 `PORTABLE_EXECUTABLE_FILE`。
10. 等待直接子进程退出，退出码作为 sfx 的退出码透传。应用经 launcher 派生新进程后自身退出时，孙进程存活期间临时目录删不掉，残留交给 reaper；孙进程不继承来源快捷方式。

### 分层清理

不写注册表持久化键：

- L0：子进程退出后 `remove_dir_all`，失败按 1 秒退避重试至多 20 次；仍失败则放弃，交给 L1。
- L1 reaper：每次 sfx 启动时枚举 `%TEMP%\kachina-sfx-*`，删除条件是目录创建时间超过 24 小时且 ToolHelp 快照中没有进程映像位于该目录之下。年龄阈值同时保护正在解压的新目录：解压中的目录里没有进程映像，ToolHelp 判定不出它。读不到创建时间的目录跳过。占用判定先把目录与每个进程的映像路径解析为规范路径（去掉 `\\?\`、转小写）再做前缀比较，同一目录的 8.3 短名、长名与经链接的形式视为同一个；目录本身解析失败时按占用处理。
- L2：不再运行任何 sfx 时的残留交给操作系统清理临时目录，与安装器暂存目录"尽力删除、下次兜底"的策略一致。

已知限制：应用的孙进程把 cwd 钉在临时目录内时，ToolHelp 拿不到 cwd 占用，删除会失败；此类残留同样按年龄阈值在后续运行中重试或交给操作系统。

### 控制台与输出重定向

sfx 在 `AttachConsole(ATTACH_PARENT_PROCESS)` 成功、存在控制台窗口、或 stdout 被重定向到管道/文件时走非交互路径：不弹对话框，也不显示进度，错误写 stderr；其余情况（双击启动）使用 TaskDialog。提权重启的实例不接原实例的控制台，始终使用 TaskDialog，控制台应用在新窗口中运行，与直接经 UAC 启动时一致。错误框的标题在解析出 `\0CONFIG` 之后取 `windowTitle`，其次 `appName`；之前的错误用 "kachina-sfx"。

### 测试

sfx 自身的单测覆盖 meta 与数据块的 join、创建临时目录之前的路径与 `exeName` 校验、按计划解出全部文件与目录、大小或 hash 不符时报错并删掉失败的文件、reaper 的时钟回拨与路径前缀判定、是否提权重启、来源快捷方式的命名与重名让位；包格式读取、写出校验、提权命令行拼接（`join_args`）与带来源快捷方式拉起子进程的单测在共享模块里，随 sfx 一起运行。e2e 见 [tests/sfx-run.mjs](../../../tests/sfx-run.mjs)：它自建应用目录（`cmd.exe` 副本充当 `app.exe`，现有 fixture 的 `app.exe` 是假 PE 不可运行），经 gen + pack + `replace-bin --base --icon` 产出 sfx 包后验证版本信息、包内数据解出、`PORTABLE_EXECUTABLE_*` 与来源快捷方式、调用者 cwd 继承、参数与退出码透传、sfx 不写 stdout、临时目录清理、无控制台时的进度框路径、提权重启实例只把用户参数交给应用、reaper 对超龄目录与运行中目录的处理，以及纯在线包拒绝。npm 脚本为 `test:sfx-run`。

## Alternatives considered

- 换皮安装器：解出一份完整安装包到临时目录再运行安装器。安装包本身已是可直接运行的 PE，安装器从自身原地读取内嵌数据块，此方案多一次全量拷贝而无收益。
- sfx 内嵌安装器映像（尾部附加 PE 或新增 TLV）：payload 中只有应用数据而没有安装器本体，换头后安装器必须由新头部携带；但自解压语义是运行应用主体而非执行安装，安装器无需存在。
- sfx 不提权、只以 `ShellExecuteExW` 的 `runas` 提权启动应用：`ShellExecuteExW` 不接受环境块与 `STARTUPINFO`，`PORTABLE_EXECUTABLE_*` 与来源快捷方式到不了提权的应用，缺省的 `prefer-admin` 包在用户同意 UAC 后固定到任务栏的仍是临时目录里的 exe。
- 跨进程往应用窗口写 `System.AppUserModel.ID` 与 `RelaunchCommand` 等属性：能覆盖自设 AppID 的应用，但覆盖的正是应用自己接管的任务栏身份；sfx 要在应用整个生命周期里跟踪新窗口；对提权窗口写入返回 `E_ACCESSDENIED`。
- `STARTF_TITLEISAPPID` 指定 AppID：分组生效，但任务栏显示名仍取 exe 文件名，固定项也不指向 sfx 本体。
- 把调用者的环境变量转发给提权实例：系统经 UAC 启动程序时不带调用者会话的环境，要保留的用户用 gsudo 或 `sudo -E` 包住 sfx 即可；sfx 自行转发会在直接 UAC 这条路上比系统更宽松。
- cwd 设为解压目录（electron-builder portable 的做法）：往 cwd 写文件的应用会把数据写进退出即删的临时目录，且覆盖调用者有意指定的 cwd；按 cwd 找包内资源的应用应改为按 exe 所在目录查找。
- 忽略 `uacStrategy`、永远以 `asInvoker` 启动应用：实现最简；代价是无法分发必须管理员运行的工具类应用。当前实现按遵循配置设计，字段取值复用而语义在 sfx 语境中重新解释。
- 看守进程：sfx 以 `--cleanup-watcher` 重拉自身的 detached 实例，等应用退出后清理，覆盖 sfx 被强制结束的场景。它为被杀场景常驻第二个进程，而 reaper 已覆盖全部残留场景，区别只在清理时效；与暂存目录"尽力删除、下次兜底"的策略不一致地引入一套进程机制及其参数与测试，不采用。
- Job Object 进程树等待：把子进程纳入 Job 等待 `ACTIVE_PROCESS_ZERO`。提权子进程无法被非提权父进程纳入 Job，提权路径必须另备降级方案，等于维护两套等待机制；只等直接子进程后，launcher 型应用的孙进程占用导致删除失败并自然流入 reaper，单一机制覆盖两条路径。
- reaper 以 lock 文件与 `pid_alive` 判定活跃目录：与年龄阈值重复，且无法识别解压进行中（目录内尚无进程映像）这一真正需要保护的状态；年龄阈值单独承担保护。
- 写 HKCU 的 RunOnce 实现跨重启清理：这是免提权跨重启的唯一用户态钩子，但 Run/RunOnce 属于杀软重点监控的持久化位置，未签名程序写入有提示与拦截风险，清理类软件也会删除该项；残留由 reaper 与操作系统兜底。若分发侧接受签名与白名单沟通成本，可作为编译期选项回归，不进入配置。
- `MoveFileExW` 的延迟到重启删除：需要管理员写 HKLM 的 PendingFileRenameOperations，sfx 通常非提权运行，不作为默认依赖。
- 内容寻址缓存复用解压结果（如 `%LOCALAPPDATA%\<hash>\`）：第二次启动零解压，但与"退出后删除"的需求直接冲突；高频启动场景不适用本方案。
- 新增 sfx 专属配置字段（如运行时机、等待开关）：违反零新增字段的兼容性约束；非提权下运行库安装不可兑现，缺运行库的便携应用由打包侧 app-local 携带 DLL 解决。
- 解压期间不做进度反馈：大包解压期间无可见反馈会导致用户重复双击，TaskDialog 进度条成本低。
- 单线程顺序解压：实现最简，实测启动到退出的耗时约为并发解压的两倍（见 Verification）。

## Verification

| 判据 | 结果 |
|---|---|
| 现有离线安装包经 `replace-bin --base` 产出的程序：双击后还原到 `%TEMP%\kachina-sfx-<uuid>\`、运行 `exeName`、退出后删除临时目录、退出码透传 | PASS：e2e 以 `cmd.exe` 副本作 `app.exe`，运行期间包内的 `data.txt` 已解出且内容为 `SFX-DATA`；`/c exit 42` 时 sfx 退出码 42，应用记下的 cwd 等于调用者的 cwd；退出后 `%TEMP%\kachina-sfx-*` 集合无新增（x86_64-win7-windows-msvc release，`pnpm build`，Windows 11 Insider 26H1 build 28020） |
| 换头产物带应用的名称与窗口标题 | PASS：e2e 断言便携包的 `FileDescription`、`ProductName` 等于包内的 `windowTitle`、`appName`；`--icon` 的图标写入见[共享包读取与边写边校验的写出管线](./2026-11-04-pack-reader-and-write-pipeline.md)的 Verification |
| 零新增配置字段；纯在线包换头后以明确错误退出 | PASS：e2e 用只含 config 的包换头运行，exit=1，stderr 为"此包不含离线数据，不能作为自解压程序运行" |
| meta 路径与 `exeName` 的逃逸在创建临时目录之前被拦截；任一文件大小或 hash 校验失败时整体失败并清理 | PASS：单测 `relative_path_guard`、`plan_extraction_rejects_unsafe_path_before_writing`、`exe_must_be_a_safe_path_in_the_plan`、`plan_extraction_reports_missing_block`、`extract_reports_size_and_hash_mismatch_and_removes_the_file`（单个文件大小不符、并发解压中 hash 不符，失败的文件都已删除） |
| `uacStrategy` 三档行为 | PARTIAL：`prefer-user` 由 e2e 覆盖；是否提权重启由单测 `only_an_unelevated_first_instance_asks_for_elevation` 覆盖；带 `--kachina-sfx-elevated` 的实例只把其余参数交给应用由 e2e 覆盖；`prefer-admin` 同意 UAC 的路径手工验证（x86_64-win7-windows-msvc release，Windows 11 Insider 26H1 build 28020）：从 cwd 为 `tests\` 的终端带参数启动，提权实例拉起的应用拿到同一 cwd、原样参数、`PORTABLE_EXECUTABLE_FILE` 与来源快捷方式，应用退出后两个 sfx 实例退出、临时目录删除；`prefer-admin` 的取消回退与 `force` 的取消报错未验证 |
| 应用拿到 `PORTABLE_EXECUTABLE_*`，来源快捷方式指向 sfx 本体 | PASS：e2e 中应用记下的三个变量等于 sfx 所在目录、sfx 路径与 `appName`；运行期间临时目录里的 `<appName>.lnk` 目标为 sfx、无参数、起始位置为 sfx 所在目录；子进程收到 `STARTF_TITLEISLINKNAME` 与 lnk 路径由单测 `child_sees_the_link_it_started_from_and_the_extra_env` 覆盖 |
| 固定到任务栏的是 sfx 本体，临时目录删除后仍可启动 | PASS（手工，Windows 11 Insider 26H1 build 28020，任务栏按钮经 UI Automation 读取）：固定操作在模拟 sfx 的 C# 启动器与 WinForms 应用上进行，以来源快捷方式启动时任务栏按钮的 AppID 是启动器路径、名称是 lnk 名；固定后 `User Pinned\TaskBar` 下的 lnk 目标为启动器、无参数；应用退出、临时目录删除后固定项仍在，启动它的新窗口并入固定按钮。应用自设 AppID 时按钮改用应用的 AppID。真实 sfx 包（x86_64-win7-windows-msvc release）经 `prefer-admin` 提权启动、以及从 `\\localhost\V$\…` 的 UNC 路径启动时，任务栏按钮的 AppID 为 sfx 路径、名称为 `appName`，UNC 启动写出的 lnk 目标与起始位置是 UNC 路径；真实包上未做固定操作。固定到开始菜单未验证 |
| 双击路径（无控制台、无重定向）显示进度框后拉起应用 | PASS：e2e 经 WMI 拉起 sfx（父进程没有控制台），应用写下标记文件，临时目录随后删除 |
| sfx 被强制结束时残留由下次启动的 reaper 按"年龄阈值 + 无进程映像占用"删除；运行中实例的目录不被误删 | PASS：e2e 把残留目录创建时间回拨两天后运行 sfx，该目录被删除、当次新建的目录保留；把运行中实例的临时目录创建时间回拨两天后再启动一次 sfx，该目录与其中的 `data.txt` 仍在（进程映像占用判定命中） |
| sfx 不往 stdout 写任何东西 | PASS：e2e 中第二次启动的 sfx stdout 为空 |
| 进度框取消后删除临时目录、不拉起应用 | 未自动化：需要非控制台启动并点击 TaskDialog 的取消按钮 |
| 全程不写 Run/RunOnce 等注册表持久化键 | PASS：实现无注册表写入（代码审查） |
| 便携包启动耗时 | 从一个生产安装包导出的 733 个文件（221.8 MiB）加 `cmd.exe` 作 `app.exe`，gen + pack + 换头后运行 `portable.exe /c exit 0`，从启动到退出（含 reaper、解压、拉起 cmd、删除临时目录）：单线程顺序解压 1641 ms，并发解压 865 ms（两版交替运行 5 轮取中位数；单线程版 1601–1704 ms，并发版 826–946 ms）。条件：x86_64-win7-windows-msvc release（build-std），32 逻辑处理器，`%TEMP%` 在 C 盘，`Start-Process -Wait` + `Measure-Command`，Windows 11 Insider 26H1 build 28020 |
| 单测、格式与 lint | PASS：`cargo test --bin kachina-sfx` 47 passed；`cargo fmt --all -- --check` 无差异；`cargo clippy --all-targets` 0 warning；`x86_64-win7-windows-msvc` release 构建通过 |

builder 侧回归：`cargo test --bin kachina-builder` 39 passed；e2e `builder-extract-replace` 用不带 `--base` 的 `replace-bin`（取 bundle 末位 PE）换头，产物的内嵌项可正常列出，换头后的安装器能完成静默安装。

## Consequences

每次启动都支付全量解压成本，包体越大启动越慢（222 MiB、734 个文件的包从启动到退出约 865 ms，条件见 Verification）；本方案适合偶尔或一次性运行的分发，不适合高频启动。

换头即用只对自包含的绿色应用成立：依赖安装副作用（注册表项、`userDataPath` 目录、运行库）的应用换头后不能正确运行。

launcher 型应用（主进程拉起实际应用后退出）的临时目录在实际应用运行期间持续存在，直到下次 sfx 启动由 reaper 清理；这类应用在两次运行之间会积累一份残留。

包格式无签名，逐文件 hash 只防损坏不防篡改；未签名程序释放文件到临时目录并执行属于启发式扫描的典型画像，缓解在分发侧签名。

继承包格式的 u32 限制：单个数据块与包体小于 4 GiB。

`uacStrategy` 在安装器语境管安装提权、在 sfx 语境管 sfx 自身的提权，提权时解压也在管理员权限下进行。用户输入另一个管理员账户的凭据时，提权实例使用那个账户的 `%TEMP%`。sfx 位于映射网络驱动器上时，提权实例可能看不到该盘符而启动失败（未验证）。

提权启动时调用者会话里设置的环境变量不到达应用；需要它们的用户用 gsudo 或 `sudo -E` 启动 sfx。

自设显式 AppID 的应用（Electron 默认如此）不从来源快捷方式获得任务栏身份，固定项仍指向临时目录里的 exe，除非应用自己设置重启命令。透传的命令行参数不进固定项。

第三个 bin 带来一份 TaskDialog 裁剪版；包格式读取与写出校验和安装器共用一份实现。crate 级依赖不分 bin，构建 sfx 仍要编译完整依赖树。

临时目录里的文件不刷盘：断电或崩溃后的临时目录本来就要丢弃，由 reaper 清理。
