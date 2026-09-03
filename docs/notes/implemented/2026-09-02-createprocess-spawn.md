# 子进程拉起直调 CreateProcessW，.NET 运行时检测改为目录与注册表

Status: implemented

## Problem

安装器拉起子进程只有两种形态：等退出码（.NET / VC++ 运行时安装器、WebView2 引导器）与拉起即走（崩溃提示进程、退出后自删的 `cmd`）。这些调用点此前分别经 `std::process::Command` 与 `tokio::process::Command`，二者为通用性携带 stdio 管道装配、环境块 `BTreeMap` 合并、PATH 解析与 `.exe` 补全等代码。[编译配置级体积裁剪](./2026-09-02-profile-size-trim.md) 后重测，`cargo bloat --filter "process::|Command"` 合计 48.5KiB `.text`，其中 `std::sys::process::windows::Command::spawn_with_attributes` 单函数 22.6KiB、环境块 `BTreeMap::insert` 5.4KiB、`Stdio::to_handle` 3.1KiB（本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std）。

`open::that` 是第三个隐藏的 `Command` 使用者：`open` 5.x 在 Windows 上的默认路径是 `cmd /c start "" "<path>"`，`shellexecute-on-windows` feature 只影响 `that_detached`。只要它还在，std 的 `Command` 实现就不会被 LTO 丢弃。

.NET 运行时检测原本 spawn `dotnet --list-runtimes` 并读 stdout，是唯一需要管道接 stdout 的调用点。它还依赖 PATH 上先命中哪个 `dotnet.exe`：装过 x86 SDK 的机器上 `C:\Program Files (x86)\dotnet` 可能排在前面，列出的是 x86 运行时，x64 应用照样起不来；`contains("Microsoft.WindowsDesktop.App 8")` 又是子串匹配。

## Decision

`src/utils/process.rs` 直接调用 `CreateProcessW`，对外只有 `spawn(program, args, hide_window) -> io::Result<Child>` 与 `shell_open(target)`：

- 命令行拼接复刻 `std::process::Command` 的 Windows 规则：argv[0] 总是加引号，其余参数为空或含空格 / 制表符时加引号，引号前的反斜杠成倍转义。
- `hide_window` 为 true 时以 `CREATE_NO_WINDOW` 拉起，是当前唯一需要的创建标志；再有新需求时扩成选项结构体。
- 子进程不继承句柄、不进 Job，父进程退出后继续存活；线程句柄拉起后立即关闭，`Child` 在 Drop 时关闭进程句柄。拉起即走的调用方直接丢弃 `Child`。
- `Child::wait` 在 `spawn_blocking` 里 `WaitForSingleObject(INFINITE)` + `GetExitCodeProcess`，返回原始退出码；调用方以 `0` 判成功，与 `ExitStatus::success()` 等价。
- `shell_open` 以 `ShellExecuteExW` 默认动词打开目标，`SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`：可执行文件直接运行，URL 交默认浏览器，失败不弹系统对话框。`installer::launch` 在 `spawn_blocking` 中调用它并把失败写日志。

调用点全部迁移：`runtimes.rs` 两处安装器与 `module/wv2.rs` 引导器走 `spawn` + `wait`；`utils/sentry.rs` 崩溃提示进程与 `installer/uninstall.rs` 的 `cmd /C ping … & del …`（`hide_window = true`）拉起即走；`installer::launch` 走 `shell_open`。`tokio` 去掉 `process` feature，`open` 从依赖中移除（连带 `dunce`、`pathdiff`、`is-wsl`、`is-docker`），仓库内不再有任何 `std::process::Command` 引用。

.NET 运行时检测改为 `runtimes::dotnet_runtime_installed(framework, major)`，不拉进程：

- 目录枚举：按 apphost 查找 .NET 根目录的顺序收集 `DOTNET_ROOT`、32 位注册表视图 `HKLM\SOFTWARE\WOW6432Node\dotnet\Setup\InstalledVersions\x64` 的 `InstallLocation`、`%ProgramFiles%\dotnet`，在每个根下枚举 `shared\<framework>\` 的子目录名。apphost 只取第一个存在的根，这里全部收集。
- 注册表：同一键下 `sharedfx\<framework>` 的值名即 MSI 安装器登记的版本号。
- 两个来源任一存在以 `{major}.` 开头的版本即视为已安装。本二进制是 x64 进程，直接写 `WOW6432Node` 字面路径，与 `check_vcredist` 对 x86 的写法一致；64 位视图下没有这些值。

## Alternatives considered

- 只换掉 `tokio::process`、保留 `std::process::Command`：std 那 22.6KiB 是大头，`tokio::process` 本身只是包装，单换 tokio 侧收益极小。
- 保留 `open` 改用 `open::that_detached`：能绕开 `Command`，但它对目录路径走 `SHOpenFolderAndSelectItems`（打开父目录并选中），与 `start` 的"打开该目录"语义不同，还带进 `dunce` 与 `CoInitialize`。`ShellExecuteExW` 一处调用即覆盖 `launch` 的全部用法，`uac.rs` 已有同一 API 的用例。
- .NET 检测只用注册表 `sharedfx`：`dotnet-install.ps1` 或解压安装并设 `DOTNET_ROOT` 的机器没有注册表记录但应用能跑；只用目录则漏掉注册表有记录、目录被自定义到别处的情形。本机两个来源本就不同步（目录多出升级残留的 `8.0.12`、`8.0.16`），取并集。
- .NET 检测改为加载 `hostfxr.dll` 调 `hostfxr_get_dotnet_environment_info`：最权威，但要动态加载与回调解析，为一个前置检查不值。
- 拉起即走的子进程加 `CREATE_BREAKAWAY_FROM_JOB`：父进程若被放进禁止 breakaway 的 Job 会直接失败；`std::process::Command` 也不加，保持同一行为。

## Verification

| 判据 | 结果 |
|---|---|
| 命令行引号转义与 std 一致 | PASS：`utils::process::tests::program_is_always_quoted_and_plain_args_are_not`、`args_with_spaces_quotes_and_backslashes_are_escaped`（含空格、空串、尾反斜杠、内嵌引号） |
| 真实拉起并取退出码 | PASS：`spawn_hidden_cmd_and_read_exit_code` 以 `hide_window` 拉起 `cmd /C exit 7`，`wait` 返回 7；`spawn_missing_program_fails` 对不存在的程序返回 `Err` |
| `shell_open` 失败不弹窗 | PASS：`shell_open_missing_target_fails_without_dialog` 对不存在的路径返回 `Err`，测试进程无对话框 |
| std `Command` 代码不再链入 | PASS：`cargo bloat --filter "process::|shell_open|ShellExecute"` 合计 5.5KiB，均为名字含 `process` 的无关函数（`prepare_process`、`kill_process` 等）；`spawn_with_attributes`、`EnvKey` `BTreeMap`、`Stdio::to_handle` 消失 |
| 体积 | PASS：3,048,448 → 2,987,008 字节（−61,440；.text 2,385,920 → 2,334,208，.rdata 476,672 → 468,480；本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std，在 `src-tauri` 目录内构建以载入 `.cargo/config.toml` 的静态 CRT 配置；两次测量时 `dist/index.html` 均未构建，前端 HTML 未嵌入，含前端的绝对值各高约 68 KiB，差值不受影响）。基线为 [Native 重构 CR 第二轮遗留](./2026-09-02-native-refactor-cr-followup.md) 两次修复提交之后的 HEAD |
| .NET 检测两个来源在本机一致 | PASS：`%ProgramFiles%\dotnet\shared\Microsoft.WindowsDesktop.App` 目录与 `WOW6432Node\…\sharedfx\Microsoft.WindowsDesktop.App` 值名对 major 6/7/8/9 结论相同；64 位视图 `HKLM\SOFTWARE\dotnet\…\sharedfx` 下无值，`InstallLocation` 与 `sharedhost\Version` 均不存在 |
| 单测 | PASS：`cargo test` 13 passed（kachina-builder）+ 69 passed, 1 ignored（kachina-installer） |
| CI 产物（x86_64-win7-windows-msvc + build-std + optimize_for_size） | PASS：2,753,536 → 2,720,768 字节（−32,768）。降幅约为本机的一半：CI 的 std 由 build-std 按 `opt-level = "z"` 编译，被移除的 `Command` 实现在该配置下本就小于本机预编译 std 中的版本 |

## Consequences

- 子进程不再有 stdout / stderr 管道能力；新增需要读输出的调用点时须自行扩展 `spawn`，或像 .NET 检测一样改用不拉进程的数据源。
- `hide_window` 只对应 `CREATE_NO_WINDOW`，对控制台子进程有效；GUI 子进程的窗口显示由其自身决定。
- `installer::launch` 失败时不再有 `cmd` 的隐藏窗口吞掉错误，而是写一条 `warn` 日志；成功路径少一层 `cmd` 进程。
- `delete_self_on_exit` 拉起 `cmd` 失败时静默跳过，不再 `unwrap` 触发 panic。
- .NET 检测把 `DOTNET_ROOT` 与注册表 `InstallLocation` 也算作根目录，比 apphost 宽：apphost 只看第一个存在的根，若 `DOTNET_ROOT` 指向一个没有目标框架的目录而 `%ProgramFiles%\dotnet` 有，检测通过但应用启动失败。这是用户环境配置错误，安装器不替 apphost 兜底。
- 检测仍按主版本匹配（`8.` 前缀），`tag` 带具体 patch 版本（如 `8.0.1`）时不比较 minor / patch；版本号已在手，需要时是一个三段整数比较的扩展。
