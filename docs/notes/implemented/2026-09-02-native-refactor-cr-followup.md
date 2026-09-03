# Native 重构 CR 第二轮遗留

Status: implemented

## Problem

[Native refactor CR leftovers](./2026-08-28-native-refactor-cr.md) 收口后再次审阅 `host/`、`session/ui.rs`、`session/plan.rs`、`utils/sentry.rs` 与 IPC 管道，发现一处遥测回归、一处将升级为编译错误的 future-incompat 警告，以及若干 CR 声明与代码不符的边角。

1. **WebView 路径的会话错误不上报。** `TACommandError` 的 `Serialize` 实现是 [遥测通道职责收敛](./2026-08-28-telemetry-channels.md) 指定的 GUI bridge 上报咽喉，但 `host/bridge.rs` 的 `on_message` 在 `Err` 分支手拼 `{ message, insight }` JSON，不经过 `Serialize`。全库无任何调用触发 `Serialize`。结果是 native 路径与 silent 路径都显式 `capture_anyhow`，唯独 WebView 路径自 Tauri 换 Win32 宿主起就没有错误事件。
2. **`recursion_depth_exceeding_limit` 警告。** release 构建报 `overflow evaluating the requirement {async block@src\host\bridge.rs}: Send`，推导链为 `on_message` 的 `tokio::spawn` → `dispatch` → `run_install` → `run_dfs_install` → `Transaction::timed`。该 lint 属 `future_incompatible`，将变为硬错误；CI 的 toolchain 是浮动 nightly，升级即编不过。
3. **主题切换时的 WebView 背景刷新是死代码。** `host/mod.rs` 主循环与 `plugin_runtime_loop` 在 `GetMessageW` 返回值上判断 `msg.message == WM_SETTINGCHANGE` 再 `SetBackground`。`WM_SETTINGCHANGE` 由系统以 SendMessage 广播，`GetMessageW` 在内部直接派发给 wndproc，不会作为队列消息返回，两处分支永不执行。实际生效的只有 wndproc 中的 `apply_mica`，且它没有像 `window::create` 那样按 `is_win11` 门控，非 Win11 上主题切换也会调 `DwmExtendFrameIntoClientArea(-1)`。
4. **`strip_install_prefix` 把相对路径整体转成小写。** `session/plan.rs` 用 `normalize_full`（含 `to_lowercase`）剥前缀后直接返回剩余部分，单测固化了 `Sub\a.dll → "sub/a.dll"`。下游 `find_local`/`is_under` 均为大小写不敏感比较，功能不受影响，但 `02-meta-scan.json` 与日志中的本地文件名与 `hashed` 原始大小写不一致。
5. **Prompt / Plugin 应答注册晚于事件发出。** `GuiPluginHost::call` 与 `GuiUi::confirm` 先 `emit` 再 `hub.wait(id)`，pending 表在事件发出之后才插入；应答若先到，`answer` 返回 false，`wait` 只能等到超时。
6. **GUI 退出路径无 flush。** [手写最小 Sentry 协议客户端](./2026-08-28-sentry-minimal-client.md) 的 `flush(timeout)` 只在 silent 失败分支调用；WebView 与 native 路径关窗后 `main` 直接返回，`Transaction::finish` 等在途 envelope 随进程退出丢弃。
7. **`handle_pipe` 服务端读循环在对端关闭时空转。** `read_line` 返回 `Ok(0)` 落入"空行跳过"分支持续 `continue`，既不退出也不广播 `Disconnect`。

## Decision

1. 上报咽喉是显式调用：`TACommandError::report_if_needed` 承载过滤（非 pipe、无 insight、码的类要求上报），`host/bridge.rs` 的 `on_message` 在 `Err` 分支调用它并把返回的事件 id 放进错误载荷。`TACommandError` 没有 `Serialize` 实现，上报不藏在序列化副作用里。上报判定按 [UI 契约](./2026-09-02-ui-contract.md) 的码类表。
2. `main.rs` 有 `#![recursion_limit = "256"]`。UI 契约落地后 Send 推导链经 `on_message → handle_intent → run_install → run_dfs_install → Transaction::timed`，默认深度不够。
3. wndproc 在 `ImmersiveColorSet` 时 `apply_mica` 并 `PostMessageW(hwnd, WM_THEME_BACKGROUND)`（`WM_APP+1`）；主循环与插件循环匹配该消息刷 `SetBackground`，不读 `GetMessage` 的 `WM_SETTINGCHANGE`。`apply_mica` 里 mica / `DwmExtendFrameIntoClientArea(-1)` 仅 `is_win11()`，`DWMWA_USE_IMMERSIVE_DARK_MODE` 始终跟标题栏。
4. `strip_install_prefix` 只用小写副本做比较，返回值从 slash 规范化后的原始路径按同一字节偏移切片；`normalize_full` 用 `to_ascii_lowercase`。
5. `PromptHub` / `PluginHub` 有 `register(id) -> Receiver`；`GuiPluginHost::call` 与 `GuiUi::confirm` 先 `register` 再 `emit`。
6. `flush(3s)` 在 GUI 相关 `block_on` 的末尾（Runtime 尚未 drop，在途 envelope 的 `spawn` 才能跑完）；`native_entry` / `host_main` / silent 失败的 `process::exit(1)` 在退出前同样 flush。`INFLIGHT == 0` 时立即返回。
7. 随 [提权管道帧编码](./2026-09-02-ipc-postcard-frames.md) 解决：`read_line` 路径整体移除，对端关闭即广播 `Disconnect` 并退出。

## Alternatives considered

- 第 1 项改为 bridge 走 `serde_json::to_value(&err)` 以复用 `Serialize` 副作用：可行，但把上报藏在序列化副作用里正是 [遥测通道职责收敛](./2026-08-28-telemetry-channels.md) 指出的隐式咽喉，显式调用更可审计。
- 第 2 项改为 `Box::pin` `dispatch` 的 future 以切断 Send 推导链：能消警告，但会话状态机每加一层 `.await` 嵌套就可能再次触顶；提高上限是编译器建议的做法，代价是 crate 级设置。
- 第 3 项改为 wndproc 直接持有 `HostHandle` 指针（`GWLP_USERDATA`）并 `send(UiAction::SetBackground)`：需要在 wndproc 中解裸指针并处理窗口销毁后的悬垂，投递自定义消息更简单且两个循环已在处理 `WM_APP`。
- 第 4 项保留小写返回值、只改 dump 与日志显示：治标；相对路径是数据不是展示，应保持原样。
- 第 6 项在 `UiAction::Close` 处理处 flush：关窗与进程退出之间还有 native 对话框等路径；放在 `block_on` 末尾覆盖 GUI 正常返回，并在 `process::exit` 前补一次，因为临时 Runtime 在 `block_on` 返回后立刻 drop，会中止已 spawn 的上报任务。
- 第 7 项不修（`main` 上即如此）：提权进程崩溃或被杀是崩溃采集要覆盖的场景，主进程在此期间空转会干扰复现与排查。

## Verification

| 判据 | 结果 |
|---|---|
| WebView 路径的会话失败经 `report_if_needed` 上报一次，native 与 silent 路径事件数不变 | 代码路径审阅：`on_message` 的 `Err` 分支是 WebView 路径唯一出口，`report_if_needed` 在此调用一次；未对错误上报后端做计数验证（DSN 尚未进配置） |
| `cargo build --release`（src-tauri）输出中不再出现 `recursion_depth_exceeding_limit` | PASS：本机 nightly 2026-08-31（rustc 1.100.0-nightly），加 `recursion_limit` 前该警告在 `on_message` 上出现，加后消失；产物字节数不变（3,046,912，本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std、含前端） |
| 非 Win11 环境切换系统主题，WebView 默认背景色跟随；Win11 保持 mica 透明，标题栏深浅色跟随 | 代码审阅落地；未做手工 GUI 回归 |
| `strip_install_prefix(r"C:\App\Sub\a.dll", r"c:/app") == "Sub/a.dll"` | PASS：`strip_install_prefix_ignores_case_and_slashes` |
| `answer` 在 `emit` 回调内同步到达时 `wait` 立即返回对应结果 | PASS：`prompt_answer_before_await_is_not_lost`、`plugin_answer_before_await_is_not_lost` |
| GUI 正常关窗，`INFLIGHT > 0` 时进程等待在途 envelope 至多 3 秒；`INFLIGHT == 0` 时退出耗时不变 | 代码审阅落地 |
| 提权进程被结束后 `handle_pipe` 任务退出，`ManagedElevate::run` 的等待方收到 `Disconnect` | PASS：`ipc/manager.rs` 本进程内命名管道测试；见 [提权管道帧编码](./2026-09-02-ipc-postcard-frames.md) |

## Consequences

- `recursion_limit` 是 crate 级开关，掩盖会话状态机继续膨胀；`run_dfs_install` 已是全库最大单函数之一，膨胀应由拆分而非上限管理。
- 第 3 项的自定义消息在隐藏插件窗口上同样触发 `SetBackground`，对不可见窗口无副作用。
- WebView 路径的错误事件量从零回到与 native / silent 同一水位，过滤覆盖是否充分以上报后端的分组观察，缺口按 [UI 契约](./2026-09-02-ui-contract.md) 的码类表补。
- 错误上报后端不可达时退出最多延长 3 秒；`read_timeout` 30 秒被 `flush` 上限截断。
