# Native 重构 CR 第二轮遗留

Status: proposed

## Problem

[Native refactor CR leftovers](../implemented/2026-08-28-native-refactor-cr.md) 收口后再次审阅 `host/`、`session/ui.rs`、`session/plan.rs`、`utils/sentry.rs` 与 IPC 管道，发现一处遥测回归、一处将升级为编译错误的 future-incompat 警告，以及若干 CR 声明与代码不符的边角。

1. **WebView 路径的会话错误不上报。** `TACommandError` 的 `Serialize` 实现是 [遥测通道职责收敛](../implemented/2026-08-28-telemetry-channels.md) 指定的 GUI bridge 上报咽喉，但 `host/bridge.rs` 的 `on_message` 在 `Err` 分支手拼 `{ message, insight }` JSON，不经过 `Serialize`。全库已无任何调用触发 `Serialize`（`IpcError::from_ta` 里那行只在 pipe 模式执行、被 `serialize` 跳过的空转 `to_value` 已随 [提权管道帧编码](../implemented/2026-09-02-ipc-postcard-frames.md) 删除）。结果是 native 路径（`host/native.rs` 会话失败分支）与 silent 路径（`main.rs`）都显式 `capture_anyhow`，唯独 WebView 路径自 Tauri 换 Win32 宿主起就没有错误事件。
2. **`recursion_depth_exceeding_limit` 警告。** release 构建报 `overflow evaluating the requirement {async block@src\host\bridge.rs:26}: Send`，推导链为 `on_message` 的 `tokio::spawn` → `dispatch` → `start_install` → `run_install` → `run_dfs_install` → `Transaction::timed`。该 lint 属 `future_incompatible`，将变为硬错误；CI 的 toolchain 是浮动 nightly，升级即编不过。
3. **主题切换时的 WebView 背景刷新是死代码。** `host/mod.rs` 主循环与 `plugin_runtime_loop` 在 `GetMessageW` 返回值上判断 `msg.message == WM_SETTINGCHANGE` 再 `SetBackground`。`WM_SETTINGCHANGE` 由系统以 SendMessage 广播，`GetMessageW` 在内部直接派发给 wndproc，不会作为队列消息返回，两处分支永不执行。实际生效的只有 wndproc 中的 `apply_mica`，且它没有像 `window::create` 那样按 `is_win11` 门控，非 Win11 上主题切换也会调 `DwmExtendFrameIntoClientArea(-1)`。
4. **`strip_install_prefix` 把相对路径整体转成小写。** `session/plan.rs` 用 `normalize_full`（含 `to_lowercase`）剥前缀后直接返回剩余部分，单测固化了 `Sub\a.dll → "sub/a.dll"`。下游 `find_local`/`is_under` 均为大小写不敏感比较，功能不受影响，但 `02-meta-scan.json` 与日志中的本地文件名与 `hashed` 原始大小写不一致。
5. **Prompt / Plugin 应答注册晚于事件发出。** `GuiPluginHost::call` 与 `GuiUi::confirm` 先 `emit` 再 `hub.wait(id)`，pending 表在事件发出之后才插入；应答若先到，`answer` 返回 false，`wait` 只能等到超时。WebView 往返远慢于一次 `Mutex` 插入，实际难以触发，但顺序反了。
6. **GUI 退出路径无 flush。** [手写最小 Sentry 协议客户端](../implemented/2026-08-28-sentry-minimal-client.md) 的 `flush(timeout)` 只在 silent 失败分支调用；WebView 与 native 路径关窗后 `main` 直接返回，`Transaction::finish` 等在途 envelope 随进程退出丢弃。
7. **`handle_pipe` 服务端读循环在对端关闭时空转。** 已随 [提权管道帧编码](../implemented/2026-09-02-ipc-postcard-frames.md) 解决：`read_line` 路径整体移除，对端关闭即广播 `Disconnect` 并退出，有本进程内的命名管道测试覆盖。下文第 7 条保留作为记录。

## Proposal

1. `host/bridge.rs` `on_message` 的 `Err` 分支显式调用 `crate::utils::sentry::capture_anyhow`，过滤条件与 `TACommandError::serialize` 一致：非 pipe 模式、`insight.is_none()`、`classify(&err.error).report`。把过滤逻辑抽成 `TACommandError::report_if_needed(&self)`，`serialize` 与 bridge 共用。
2. `main.rs` 增加 `#![recursion_limit = "256"]`。
3. `host/window.rs` 的两个 wndproc 在 `WM_SETTINGCHANGE` 且 `is_color_theme_change` 时 `PostMessageW(hwnd, WM_APP + 1, 0, 0)`；主循环与 `plugin_runtime_loop` 改为匹配 `WM_APP + 1` 触发 `SetBackground`，删除对 `WM_SETTINGCHANGE` 的判断。wndproc 中的 `apply_mica` 按窗口创建时的 `is_win11` 门控，标记方式为 `RegisterClassExW` 时分两个类名或以 `GWLP_USERDATA` 存布尔，取实现时更简的一种。
4. 已落地。`strip_install_prefix` 只用小写副本做比较，返回值从 slash 规范化后的原始路径按同一字节偏移切片；`normalize_full` 改用 `to_ascii_lowercase`。`strip_install_prefix_ignores_case_and_slashes` 期望为 `"Sub/a.dll"`。
5. 已落地。`PromptHub` / `PluginHub` 拆出 `register(id) -> Receiver`；`GuiPluginHost::call` 与 `GuiUi::confirm` 先 `register` 再 `emit`。单测覆盖应答在 await 之前同步到达。
6. 已落地。`flush(3s)` 放在 GUI 相关 `block_on` 的末尾（Runtime 尚未 drop，在途 envelope 的 `spawn` 才能跑完）。`native_entry` / `host_main` / silent 失败的 `process::exit(1)` 在退出前同样 flush，避免跳过 `block_on` 尾部。`INFLIGHT == 0` 时立即返回。
7. 已实施，见上。

## Alternatives considered

- 第 1 项改为 bridge 走 `serde_json::to_value(&err)` 以复用 `Serialize` 副作用：可行，但把上报藏在序列化副作用里正是 [遥测通道职责收敛](../implemented/2026-08-28-telemetry-channels.md) 指出的隐式咽喉，显式调用更可审计。
- 第 2 项改为 `Box::pin` `dispatch` 的 future 以切断 Send 推导链：能消警告，但会话状态机每加一层 `.await` 嵌套就可能再次触顶；提高上限是编译器建议的做法，代价是 crate 级设置。
- 第 3 项改为 wndproc 直接持有 `HostHandle` 指针（`GWLP_USERDATA`）并 `send(UiAction::SetBackground)`：需要在 wndproc 中解裸指针并处理窗口销毁后的悬垂，投递自定义消息更简单且两个循环已在处理 `WM_APP`。
- 第 4 项保留小写返回值、只改 dump 与日志显示：治标；相对路径是数据不是展示，应保持原样。
- 第 6 项在 `UiAction::Close` 处理处 flush：关窗与进程退出之间还有 native 对话框等路径；放在 `block_on` 末尾覆盖 GUI 正常返回，并在 `process::exit` 前补一次，因为临时 Runtime 在 `block_on` 返回后立刻 drop，会中止已 spawn 的上报任务。
- 第 7 项不修（`main` 上即如此）：提权进程崩溃或被杀是崩溃采集要覆盖的场景，主进程在此期间空转会干扰复现与排查。

## Acceptance criteria

- WebView 路径构造一个 `classify(&err).report == true` 的会话失败（例如打包配置指向不可达的 hashed 源并关闭 `META_FAILED` 的 Expected 标记做临时验证），错误上报后端收到且仅收到一个事件；native 与 silent 路径事件数不变。
- `cargo build --release`（src-tauri）输出中不再出现 `recursion_depth_exceeding_limit`。
- 非 Win11 环境切换系统主题，WebView 默认背景色跟随；Win11 保持 mica 透明，标题栏深浅色跟随。
- `strip_install_prefix(r"C:\App\Sub\a.dll", r"c:/app") == "Sub/a.dll"`；`02-meta-scan.json` 中 `local[].file_name` 与 `hashed[].file_name` 大小写一致。
- 单测：`answer` 在 `emit` 回调内同步到达时 `wait` 立即返回对应结果，不等待超时。
- GUI 正常关窗，`INFLIGHT > 0` 时进程等待在途 envelope 至多 3 秒；`INFLIGHT == 0` 时退出耗时不变。
- 提权进程被任务管理器结束后，主进程 `handle_pipe` 任务退出，`ManagedElevate::run` 的等待方收到 `Disconnect`，主进程 CPU 占用回落。

## Risks

- `recursion_limit` 是 crate 级开关，掩盖会话状态机继续膨胀；接受，`run_dfs_install` 已是全库最大单函数，膨胀应由拆分而非上限管理。
- 第 3 项的自定义消息在隐藏插件窗口上同样触发 `SetBackground`，对不可见窗口无副作用。
- 第 1 项落地后 WebView 路径的错误事件量会从零回到与 native/silent 同一水位，观察一段时间确认 Expected 过滤覆盖充分，否则补 `session/error.rs` 的分类。
- 第 6 项在错误上报后端不可达时最多延长退出 3 秒；`read_timeout` 30 秒被 `flush` 上限截断，不会出现更长的挂起。
