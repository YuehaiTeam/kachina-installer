# UI 契约：Rust 只推状态，渲染器只渲染

Status: implemented

## Problem

安装器有两个第一方界面（WebView 前端 `src/`、TaskDialog 原生界面 `src-tauri/src/host/native.rs`）和一个无界面路径（silent），三者共用 `src-tauri/src/session/` 的会话逻辑，但界面层的状态与文案由每个界面各自维护，Rust 在其中扮演"应答命令 + 推送拼好的文本"的角色。具体表现：

1. **两套状态机重复推导同一批事实。** `host/native.rs` 的 `ReadyState { path, source_uri, create_lnk, delete_user_data, mirrorc_cdk, is_update, is_uninstall }` 与 `src/App.vue` 的十九个 `ref`（`step`、`subStep`、`percent`、`current`、`source`、`selectedSource`、`isUpdate`、`needElevate`……）各自从 `InstallerConfig` 推导安装路径、当前源、是否为更新、是否需要提权。`src/bootstrap.ts` 还承担了业务判断：嵌入索引与文件表的一致性校验、卸载元数据是否存在、`autoRun` 决策、`uacNeeded` 提权判定——这些在 `session/types.rs` 的 `settings_from_input` / `settings_from_cli` 里有 Rust 版本。
2. **Rust 推送的是渲染结果而不是状态。** `SessionUi::progress` 携带的 `ProgressEvent.current` 是拼好的中文句子，`session/run.rs` 里 16 处常量文本、约 8 处 `format!`（文件名、运行库名、经 `format_size` 格式化的字节数），其中两处含 HTML（`<br>`、`<div class="d-single-stat">`）；`host/native.rs` 用 `plain_progress` 把 `<br>` 替换回换行，前端用 `v-html` 渲染。`SessionUi::confirm(kind, title, message)` 已有 `PromptKind` 枚举却仍传拼好的 `title`/`message`，占用文件列表、进程列表、版本号被埋进字符串。`SessionUi::alert(title, message)` 三处是非致命错误的文本版。`SessionUi::reopen_source()` 是 Rust 向前端下发的流程指令。
3. **错误是中文字符串。** `session/error.rs` 有 12 个中文常量（`PKG_BROKEN`、`META_FAILED`、`HASH_INVALID`、`UNINSTALL_META_MISSING`、`PATH_INVALID`、`TEMP_DIR`、`NO_DOWNLOAD_NODE`、`DFS2_SESSION`、`FILE_MISSING`、`PLUGIN_NO_UI`、`PLUGIN_NEED_WEBVIEW2`、`PLUGIN_HOST_FAILED`），`hide(user_msg, err)` 把原始错误丢进日志、用中文替换；`friendly()` 内含四句网络错误文案；`run.rs` 的 `mirrorc_error` 是 Mirror酱 数字码到中文的映射表并附带 `reopen` 位；[遥测通道职责收敛](./2026-08-28-telemetry-channels.md) 的 `classify()` 靠 `Expected` 标记加文本匹配（`kind_from_text` 对中文常量与 `HASH_MISMATCH_ERR` 等短码做 `contains`）决定上报与 `fail` 维度。`dfs.rs` 的 `SHORT_INSIGHT_CODES` / `short_insight_code` 是另一套文本匹配。前端 `src/mirrorc-errors.ts` 维护着同一张 Mirror酱 映射表的 TS 版本。bridge 的 `on_message` 在 `Err` 分支发 `{ message: format!("{:#}", err), insight }`，前端只能拿到字符串。
4. **计数遥测经前端中转。** `SilentUi` 与 `NativeUi` 直接调用 `session/ui.rs` 的 `send_ev_insight`，而 `GuiUi` 把 `session-insight` 事件发给前端、前端再经 HTTP 发出；`bootstrap.ts` 与 `App.vue` 的 `uninstall()` 还自行发起会话开始与卸载事件，Rust 侧不知道它们的存在。遥测不是界面职责，跨一次 IPC 只带来两条实现。
5. **界面文案没有单一来源。** 中文散布在 `session/run.rs`（49 行）、`host/native.rs`（30 行）、`session/error.rs`（21 行）、`module/wv2.rs`、`main.rs`、`App.vue` 及各组件。`locales/` 目录存在但为空。

这些让第三方定制界面（打包时以自定义 HTML 替换 `\0IMAGE` 槽位）几乎不可能：它们要重新实现 `bootstrap.ts` 的推导、解析 Rust 拼出的句子、并接受 Rust 决定的错误文案与流程。

## Decision

### 原则

Rust 是推状态的后端：推**键**与**原始数据**（枚举、数字、文件名、原始错误文本），不推句子、不推格式化后的数字、不推标记。查表、插值、`format_size`、HTML 全部属于渲染器——WebView 路径在前端做，native 路径在 `host/native.rs` 做，两者读同一张文案表。渲染器对 Rust 只发**意图**（用户动作），不发决策结果。

### 状态结构体与意图

Rust 持有唯一一份界面状态（`session/state.rs`），每次变化整体推送；渲染器全量套用。结构体不足 1 KiB，不做 diff / patch 协议。

```rust
pub struct UiState {
    pub phase: Phase,                // Ready | Running(Progress) | Done(SessionResult) | Failed(Coded)，serde tag = "kind"
    pub mode: Mode,                  // Install | Update | Uninstall，由路径探测与参数推导
    pub project: ProjectView,        // window_title, title, description, borderless, lang
    pub options: Options,            // install_path, source_uri, create_lnk, delete_user_data, mirrorc_cdk
    pub sources: Vec<SourceItem>,    // 可见源：id, name, uri, icon(可选 SVG 文本), requires_webview
    pub path: PathState,             // writable: Writable | Unwritable | Private, exists, upgrade
    pub needs_elevate: bool,
    pub cdk: CdkStatus,              // Idle | Checking | Ok | Invalid(Coded)，serde tag = "kind"
    pub theme: Theme,                // None | Image | Css | Html
    pub pending: Option<Prompt>,     // 有值即显示模态确认
}

pub struct Progress { sub_step: u32, percent: f64, stage: &'static str, subject: Option<String>, done: Option<u64>, total: Option<u64> }
pub struct Prompt { id: String, kind: &'static str, items: Vec<String>, params: BTreeMap<&'static str, String> }

pub enum Intent {
    SetPath { path }, SetSource { uri }, SetCreateLnk { value }, SetDeleteUserData { value }, SetCdk { cdk },
    Start, Cancel, Answer { id, ok }, Dismiss, Launch, Advanced, Close,
}
```

`Intent` 的 wire 形状是 `{ "kind": "<snake_case 变体名>", …字段 }`，与 `Phase` / `CdkStatus` 下行的 `kind` 约定对称。它不走 `#[derive(Deserialize)]`：内部标签派生会拉进 serde 的 `Content` 缓冲机制（本枚举单独约 14 KiB `.text`，机制见 [IPC 枚举改 serde 外部标签](./2026-08-28-ipc-external-tagging.md)），改为 `Intent::from_value(&Value)` 对 `kind` 做 `match` 后直接取字段，`session/state.rs` 的 `intent_from_value_covers_every_variant` 逐变体守形状。

`UiState` 只放两个渲染器都必须一致表达的会话状态。哪个面板展开、EULA 勾选、动画、hover 属于渲染器自己的视图状态，不进结构体。`Phase` 用带数据的枚举，进度只存在于 `Running`、结果只存在于 `Done`、错误只存在于 `Failed`。主题图片 / CSS 等二进制不进状态，见"主题槽位"。

### 传输

- Rust → WebView：事件 `ui-state`，payload 为完整 `UiState`，经 `HostHandle::emit`。
- WebView → Rust：命令 `intent`，参数为 `Intent`。bridge 命令面为：`intent`、`pick_path`（弹系统目录选择框并返回路径；空目录 / 非空目录追加应用名子目录的判断在 Rust 的 `installer::apply_path_choice`，与 native 共用）、`error_dialog`、`task_dialog`、`launch`、`launch_and_exit`、`log` / `warn` / `error`、`window_show` / `window_close` / `window_minimize` / `window_set_title` / `window_set_decorations`，以及插件宿主的 `plugin_host_ready` / `answer_session_plugin` / `http_get_request`。`get_installer_config`、`select_dir`、`start_install`、`start_uninstall`、`answer_session_prompt`、`read_uninstall_metadata`、`wincred_*`、`get_mirrorc_status` 不存在。
- bridge `on_message` 的 `Err` 分支发 `{ code, detail, subject, sid, event_id, insight }`：`code` 对取消与未挂码错误为 `null`，取消时 `detail` 为 `"cancelled"`；`event_id` 由 `TACommandError::report_if_needed` 在上报后回填。
- native：`host/native.rs` 的 `loop { show_ready_page → finish_action }` 以 `UiState` 为输入、以 `Intent` 为输出；`Phase::Running` 由 `ProgressDialog` 渲染，`Phase::Done` 由 `show_finish` 渲染；`Intent::Advanced` 由宿主层切到 WebView。
- silent：不渲染。`SilentUi::state` 对 `Phase::Failed` 写一行日志 `code: detail`（无 detail 时只有 code），`notify` 同形；退出码 1。

### 会话状态机

`session/state.rs::UiSession::apply(Intent)` 是同步的：更新 `options`；`SetPath` / `SetSource` 之后经 `installer::probe_dir` 得到 `DirProbe { exists, empty, upgrade, writable, private }` 重算 `path`、`mode`、`needs_elevate`、`sources` 可见性（native 隐藏 `requires_webview` 的源）。`probe_dir` 同时是 `installer::inspect_dir` 的实现，可写性靠在目标目录（或最近的已存在祖先）创建并删除探针文件判定；`prefer-user` 策略下可写目录不提权。`SetCdk` 触发 Mirror酱 状态查询并读写凭据，结果反映在 `cdk`。`Start` 只做 CDK 门：源为 `mirrorc://` 且 `cdk != Ok` 时进入 `Failed(MIRRORC_CDK_MISSING)`。`Answer` 只清掉 id 匹配的 `pending`，不改 `phase`；`phase` 的 `Running → Done / Failed` 由 `run_install` / `run_uninstall` 的返回值决定。`Dismiss` 从 `Failed` 回 `Ready` 并保留 `options`。`Cancel` / `Launch` / `Advanced` / `Close` 在 `apply` 中为空操作，由宿主与 `session/commands.rs::handle_intent` 处理。

`run_install` / `run_uninstall` 接收 `base: &UiState`，进度以此为底只替换 `phase`；`GuiUi::state` 合并进 `GuiRuntime` 持有的会话时也只取 `phase`，`Ready` 期间的 `options` 不被进度推送覆盖。

`session/commands.rs::prepare_gui` 承担原 `bootstrap.ts` 的启动判断：嵌入索引与文件表不一致或嵌入配置缺失 → `Phase::Failed(PKG_BROKEN)`；卸载模式下卸载元数据缺失 → `Failed(UNINSTALL_INFO_MISSING)`。这两类是宿主级致命错误，`GuiRuntime.fatal` 为真，`Dismiss` 直接关窗。

`SessionUi` trait：

```rust
pub trait SessionUi: Send + Sync {
    fn state(&self, state: &UiState);
    async fn confirm(&self, prompt: Prompt) -> bool;      // 经 UiState.pending 呈现，等待 Answer
    fn notify(&self, coded: &Coded);                      // 非致命错误，事件 ui-notice
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>>;
}
```

`progress`、`alert`、`insight`、`reopen_source` 不存在。`alert` 的三处调用改为 `notify(Coded)`（`RUNTIME_INSTALL_FAILED` 带 `subject = 运行库名`、`detail = 原始错误`）；Mirror酱 CDK 错误后是否重开源面板由前端按 `MIRRORC_CDK_*` 码自行决定。

### 错误

错误由三部分组成，代码里只出现第一部分：

| 部分 | 来源 | 例子 |
|---|---|---|
| 码 | 挂码点，`&'static str` 常量 | `PERMISSION_DENIED` |
| 原始信息 `detail` | 源错误本身：OS / 库 / 对端 API 返回的文本，Rust 只转录 | `拒绝访问。(os error 5)`、`503: <body 前 512 字节>`、Mirror酱 返回的 `msg` |
| 文案 | 文案表按码查得 | 不在 Rust 源码中 |

```rust
// src-tauri/src/utils/code.rs
pub struct Coded {
    pub code: &'static str,
    pub detail: Option<String>,    // 源错误链 {:#}，剥除 URL；无源错误时 None
    pub subject: Option<String>,   // 操作对象：文件名、注册表键、host、运行库名
    pub sid: Option<String>,       // DFS 会话 id，仅下载类
    pub event_id: Option<String>,  // 错误上报事件 id，会话边界上报后回填
}
```

- **构造只有两种方式**：对已有错误 `.attach(code)` / `.attach_with(code, subject)`（`Result` 与 `anyhow::Error` 上的 `Attach` trait），或无源错误的 `Coded::bare(code)` / `Coded::bare_with(code, subject)`。没有接收消息字符串的构造器。
- **`attach` 的语义是"若尚未挂码，则为此码"**：链上已有 `Coded` 时原样返回。内层操作知道自己的语义先挂码，外层 `attach` 是兜底。
- **挂码点在知道操作语义的那一层**：`utils/url.rs` 的 `HttpContextExt` 只保留上下文，不挂码；`fetch_metadata` 挂 `METADATA_*`，下载流挂 `DOWNLOAD_*`，`get_mirrorc_status` 的调用方挂 `MIRRORC_*`。网络错误细分只看类型：`fs.rs` 的 `ClassifiedNetworkError`（经 `io::Error::get_ref()` 取回）、`io::ErrorKind`、`reqwest::Error::is_timeout()` / `is_connect()`、`dfs.rs::HttpStatus`（类型化的非 2xx 响应），不做文本匹配。`attach_download_or(err, fallback)` 在无法细分时挂调用方给的兜底码。
- `Coded` 作为链节点入 anyhow 链，`Display` 只输出码。DFS 会话 id 是另一个链节点 `DfsSession`：`run_dfs_install` 建立会话后用 `tag_session` 给会话内的失败打标，`extract` 时折进 `Coded.sid`。
- **边界提取** `extract(&anyhow::Error) -> Extracted { Cancelled | Coded(Coded) | Uncoded { detail } }`，遍历链、先查 `Cancelled` 标记再查 `Coded`。`Uncoded` 是缺陷，`detail` 为整条链的 `{:#}`。
- **上报由码的类决定**（`class_of` / `should_report_error`）：N（下载本身）、E（用户机器环境）、U（用户输入）不上报；C（打包方配置）、S（服务端 / 第三方服务）、M（第一方元数据 API）上报；`Uncoded` 上报。`fail` counter 的 `kind` 维度取 `fail_kind`：`n` / `e` / `u` / `c` / `s` / `m` / `uncoded` / `cancelled`。上报咽喉是 `TACommandError::report_if_needed`（非 pipe、无 insight、类要求上报时调用 `capture_anyhow`，返回事件 id）与 `main.rs::fatal_error`。`classify()`、`Expected`、`FailKind`、`kind_from_text`、`hide()`、`friendly()` 及中文常量不存在；`dfs.rs` 的 `InsightItem.error` 直接取码（`insight_code` / `insight_code_for_io`）。
- 提权管道：`ipc/mod.rs::IpcError` 携带 `code` / `subject` / `sid` / `cancelled`，`into_ta` 经 `code_from_str` 还原 `Coded`，提权侧的码在主进程原样呈现。
- `dfs.rs` 的 API 函数返回 `anyhow::Result<T>`；Mirror酱 数字码经 `code_for_mirrorc_status` 映射：`1001 / 8001 / 8002 / 8003 / 8004 → MIRRORC_CONFIG_INVALID`，`7001 → MIRRORC_CDK_EXPIRED`，`7002 → MIRRORC_CDK_INVALID`，`7003 → MIRRORC_CDK_QUOTA_EXCEEDED`，`7004 → MIRRORC_CDK_MISMATCH`，`7005 → MIRRORC_CDK_BANNED`，其它非零 → `MIRRORC_FAILED`；`detail` 放 API 原始 `msg`。

错误码（类、是否上报、`subject` 约定）：

| 类 | 码 | subject |
|---|---|---|
| N 不上报 | `DOWNLOAD_TIMEOUT` `DOWNLOAD_REFUSED` `DOWNLOAD_FAILED` `DOWNLOAD_STALLED` `SERVER_HTTP_ERROR` `HASH_MISMATCH` `SOURCE_NEEDS_VERIFICATION` | host / 文件名 |
| E 不上报 | `PERMISSION_DENIED` `DISK_FULL` `FILE_IN_USE` `FILE_IO_FAILED` `TEMP_DIR_UNAVAILABLE` `PROCESS_KILL_FAILED` `REGISTRY_WRITE_FAILED` `SHORTCUT_FAILED` `ELEVATE_FAILED` `RUNTIME_INSTALL_FAILED` `WEBVIEW2_REQUIRED` `WEBVIEW2_FAILED` `SELF_UPDATE_FAILED` | 文件 / 键 / 运行库名 |
| U 不上报 | `MIRRORC_CDK_MISSING` `MIRRORC_CDK_EXPIRED` `MIRRORC_CDK_INVALID` `MIRRORC_CDK_MISMATCH` `MIRRORC_CDK_QUOTA_EXCEEDED` `MIRRORC_CDK_BANNED` `INSTALL_PATH_INVALID` `PLUGIN_FAILED` | — |
| C 上报 | `PKG_BROKEN` `SOURCE_INVALID` `VERSION_REGEX_INVALID` `MIRRORC_CONFIG_INVALID` `PLUGIN_NO_UI` `PLUGIN_NOT_FOUND` `RUNTIME_UNSUPPORTED` `UNINSTALL_INFO_MISSING` `HASH_ALGORITHM_UNSUPPORTED` | 源 uri / 插件名 |
| S 上报 | `SOURCE_METADATA_INVALID` `REMOTE_FILE_MISSING` `NO_DOWNLOAD_NODE` `EXTRACT_FAILED` `MIRRORC_FAILED` `MIRRORC_UNREACHABLE` | 文件名 |
| M 上报 | `METADATA_UNREACHABLE` `METADATA_HTTP_ERROR` `METADATA_INVALID` | — |
| 缺陷 上报 | `INTERNAL_ERROR`（仅文案键，不作为挂码目标） | — |

### 错误出口

Rust 里只有一个默认错误处理实现 `utils/taskdialog.rs::show_error(ErrorDialog { code, detail, subject, sid, event_id }, parent)`：标题 = `dialog.error`；主指令 = 文案表[code]，文案含 `{subject}` 占位符时在此插值；正文 = `subject`（文案已含 `{subject}` 时不重复）与 `detail`，以空行分隔；脚注 = `dialog.session_id` / `dialog.event_id`（有值才显示）。**码本身不显示给用户。**"复制"按钮只在 `copy_useful(code)` 为真时出现——N / S / M 类与未挂码的 `INTERNAL_ERROR`，即对端有人能凭 id 查到东西的错误；E / U / C 类只能本地解决，不给按钮。复制内容为逐行 `code` / `subject` / `sid` / `event` / `time`（UTC 秒）/ `detail`，按下后对话框保持打开。`Uncoded` 用 `INTERNAL_ERROR` 的文案。

bridge 暴露两层：`error_dialog({ code, detail, subject, sid, event_id })` 直通 `show_error`；`task_dialog({ title, content, expanded, footer, buttons })` 是底层原语，给自定义 HTML 自己组文案和按钮用。

边界规则：WebView 存活期间的错误全部进 `ui-state`（`Failed`）或 `ui-notice`，由前端决定调 `error_dialog` 还是自己渲染；WebView 尚不存在或已销毁时（WebView2 缺失、插件宿主启动失败、宿主初始化失败）Rust 直接调 `show_error`，宿主级失败统一经 `main.rs::fatal_error`。崩溃提示 `crash_dialog` 走 `task_dialog`，文案 `dialog.crash` 带 `{event_id}`。遥测（上报判定、`fail` counter、insight）在会话边界完成，不依赖任何渲染器调用。

仓库中没有 `rfd`：`MessageDialog` 的全部用途收敛到 `show_error` / `task_dialog`，目录选择由 `utils/folderdialog.rs` 直接调 Common Item Dialog（`IFileOpenDialog` + `FOS_PICKFOLDERS`，在自带 STA 的阻塞线程上运行，`ERROR_CANCELLED` 视为未选择）；`raw-window-handle` 随之移除。

### 进度阶段键

`stage` 取值与文案键 `progress.<stage>` 一一对应，插值参数在括号内：`prepare`、`metadata`、`hash_scan`、`plan`、`download(subject, done, total)`、`patch(subject)`、`extract(subject, done, total)`、`delete(subject)`、`runtime_download(subject, done, total)`、`runtime_install(subject)`、`shortcut`、`registry`、`finalize`、`uninstall_scan`、`uninstall_delete(subject, done, total)`、`mirrorc_metadata`、`mirrorc_download(done, total)`、`mirrorc_verify`、`install_done`、`already_latest`、`uninstall_done`。结局按三种分开是因为三者文案不同、渲染器不能从 `Phase::Done` 之外的信息推出该显示哪句。`done` / `total` 为字节数的阶段列在 `session/state.rs::BYTE_STAGES`（`download`、`runtime_download`、`mirrorc_download`），其余按计数解释；前端 `screens/Running.tsx` 持同一列表。步骤标题为文案键 `step.default.<n>` / `step.mirrorc.<n>`，渲染器按当前源是否为 `mirrorc://` 选择。

### 计数遥测

`session/run.rs::emit_insight` 与 `insight_base` 在 Rust，会话开始事件与卸载事件由 `run_install` / `run_uninstall` 包装层发出（`fail` counter 在同一处）。前端没有 `sendInsight` / `insightBase`。

### 文案表

仓库形态：`locales/<lang>.tsv`，每行 `KEY\t文案`，一个语言一个文件。错误码直接作 key；其它字串用带前缀的 key（`progress.*`、`step.*`、`prompt.<kind>.title` / `.message`、`ready.*`、`done.*`、`dialog.*`、`webview2.*`、`shortcut.*`）。占位符写作 `{subject}`、`{done}`、`{total}`、`{items}`、`{local}`、`{remote}`、`{sid}`、`{event_id}`。

构建形态：`src-tauri/build.rs` 读取 `locales/*.tsv`，按 key 字典序合并为宽表 `i18n.tsv`——首行表头 `KEY\t<lang1>\t<lang2>…`（列名取文件名），缺失翻译留空单元格——zstd level 22 压缩后作为资产条目 `i18n.tsv` 加入 `host/assets.rs`。`cargo:rerun-if-changed` 指向 `locales/` 目录与其中每个文件。

运行时：Rust 与前端都经 `assets::lookup("i18n.tsv")` 拿同一份字节。Rust 侧 `utils/i18n.rs::Catalog` 解码后按表头找列、按 key 找行，`t(key, params)` 只做 `{name}` 直接替换，缺键返回键名；语言由 `GetUserDefaultLocaleName` 决定一次（无匹配列时用第一列），放入 `UiState.project.lang`。`format_size` 也在此模块，供 native 渲染器使用。文案表的读者只有渲染器（native、silent、`show_error`）与会话层中少数用户可见的文件系统名（卸载快捷方式名 `shortcut.uninstall`）。

选型依据（46 条、zh-CN 真实文案 + en-US 近似长度英文、zstd level 22）：每语言单独一个 zstd 帧合计 2,486 字节，两语言并入一帧 2,239 字节，三列宽表 2,165 字节；JSON 与 TSV 在压缩后相差 27 字节，TSV 的 Rust 解析是 `lines()` + `split('\t')`，不引入 `serde_json` 对 `HashMap<String, String>` 的单态化。

### 主题槽位

打包格式不变，`\0IMAGE` 槽位继续一槽多用，识别在 `session/commands.rs::apply_theme`：`RIFF….WEBP` 魔数为图片；`28 B5 2F FD` 为 zstd 帧，解开后首个非空白字符为 `<` 是 HTML、否则是 CSS；其余按前 16 字节可打印 ASCII 视为明文 CSS，否则视为图片。识别结果进 `UiState.theme`；字节本体走资产端点：图片 `theme.webp`、CSS `theme.css`；HTML 直接替换 `index.html` 条目。`InstallerConfig.embedded_image` 不以 base64 进入任何命令返回值。空包不携带默认图片，`theme == None` 时渲染器不显示图片区域。

### 插件宿主

插件宿主加载与主界面同一份 HTML（`plugin_runtime_setup` 的 `index.html?pluginHost=1`），插件在前端 bundle 内注册；自定义 HTML 替换 `index.html` 后同时成为插件宿主，只改 HTML 就能引入新插件。协议：宿主监听 `session-plugin` 事件（`PluginEvent { id, method, name, url, range, diffchunks, insights }`），以 `answer_session_plugin({ id, ok, data?, error?, unimplemented? })` 应答，启动完成后调用 `plugin_host_ready`。插件 `ok: false` 时的 `error` 文本作为 `PLUGIN_FAILED` 的 `detail`。这是自定义 HTML 必须实现的两条契约之一（另一条是 `ui-state` / `intent`），最小实现见 [前端重写为 Preact 渲染器](./2026-09-02-frontend-preact-renderer.md)。

## Alternatives considered

- 只做错误码、不动状态机：错误是"Rust 拼文案推前端"的一个特例，单独治理后进度、提示仍是句子，自定义 HTML 仍要解析文本；两者动的是同一批文件，一起做少一遍回归。
- 错误码与文案一起放在 Rust 常量中、以 `coded(code, msg)` 构造：码与文案同时出现在每个调用点，文案表形同虚设，多语言无从下手，`detail` 与用户文案混为一个字段。
- 在传输层（`HttpContextExt`）按"业务家族"参数挂码：把业务语义压进传输层，每个 HTTP 调用点都要声明自己属于下载还是元数据；挂码点放在知道语义的业务层后传输层无需知情。
- `fail` counter 直接报码：违反已定的维度 ≤ 10 判据；报类即可满足过滤与成功率口径，码在错误上报后端与日志中可查。
- 状态推送做增量 patch：结构体不足 1 KiB、变化频率以进度为上限（每秒数十次），全量推送的成本可忽略，patch 协议增加两端复杂度。
- 文案表用 JSON：压缩后与 TSV 相差 27 字节，但 Rust 侧多一份 `serde_json` 单态化（估 1–3 KiB `.text`）。
- 每语言单独一个 zstd 资产：比宽表多约 320 字节，且加语言要改资产清单；宽表加语言只是加一列，缺翻译一眼可见。
- 文案表并入 `index.html` 的压缩帧：native 路径为读 2 KiB 文案要解码整个前端 HTML，方向与 native 路径不依赖 WebView 的初衷相反。
- 新增打包字段标注 `\0IMAGE` 内容类型：改变包格式；魔数识别在运行时零成本且兼容既有包。
- 用 UI 自动化（CDP 驱动 WebView2、UIA 驱动 TaskDialog）验收：渲染器变薄后行为逻辑全部在 `UiSession::apply` 与 `extract` 中，用 Rust 单测与前端组件测试覆盖；UI 自动化只多覆盖"窗口真的弹出"一层，代价是 CI 交互桌面依赖与 flake。
- 先给现有 Vue 写一层 `UiState` → `ref` 适配、模板不动，再另开 Preact 重写：那等于独立重写一份 Vue 渲染器，契约形状会在过渡协议上冻一版，e2e 还要回归两遍。
- `Intent` 改 serde 外部标签（`{"set_path":{"path":…}}`）以出图 `Content` 机制：体积与手写 `match` 相当，但让上行 `intent` 与下行 `ui-state` 的标签风格不对称，前端、测试与两篇 note 都要跟着改；手写解析形状不变、只碰一个文件。
- 错误对话框把码显示在脚注、复制按钮对所有错误可用：码对用户无意义，E / U / C 类复制出来也无人能用；改为 id 进脚注、复制只给对端可查的类。
- 会话 id 放进 `Coded` 字段而非独立链节点：会话建立与失败挂码不在同一层，独立节点让 `run_dfs_install` 只在会话边界打一次标，任何内层挂码都能在 `extract` 时拿到 sid。

## Verification

| 判据 | 结果 |
|---|---|
| `rg -n --pcre2 "[\x{4e00}-\x{9fff}]" src-tauri/src --glob '!**/tests/**'` 排除注释后，命中仅限 `utils/i18n.rs` 的测试夹具 | PASS：其余命中均为 `//` / `//!` 注释；`session/`、`utils/error.rs`、`utils/code.rs`、`dfs.rs`、`fs.rs`、`module/wv2.rs`、`main.rs`、`host/native.rs` 零命中 |
| `SessionUi` 只有 `state`、`confirm`、`notify`、`plugin_host`；`ProgressEvent.current`、`PromptEvent`、`session-progress` / `session-prompt` / `session-insight` / `session-reopen-source` 在仓库中不存在 | PASS |
| `utils/code.rs` 单测：`attach` 幂等、`extract` 三态、类表与上报判定、`Cancelled` 优先、`detail` 剥 URL、`subject` / `sid` 透传 | PASS：`cargo test --bin kachina-installer` 81 passed / 1 ignored |
| `session/state.rs` 单测：`SetPath` 到只读目录后 `needs_elevate == true` 且 `mode` 随 `upgrade` 变化；`SetSource` 切到 `mirrorc://` 后 `cdk == Idle` 且 `Start` 在 `cdk != Ok` 时进入 `Failed(MIRRORC_CDK_MISSING)`；`Answer { ok: false }` 于 `occupied_files` 后的相位；`Dismiss` 从 `Failed` 回 `Ready` 且 `options` 保持 | PASS，其中 `Answer` 一条按落地语义断言 `pending` 清空、`phase` 不变（相位由 `run_install` 返回值决定，不由 `apply` 决定） |
| `Intent` 逐变体解析与拒绝未知 kind / 缺字段 / 类型错 | PASS：`intent_from_value_covers_every_variant` |
| silent 路径 `Phase::Failed` 日志末行为 `code: detail`，退出码 1 | `SilentUi::state` 的 `Failed` 分支与 `silent_main` 的返回值按此实现；未做注入 `METADATA_HTTP_ERROR` 的单独验证 |
| `locales/zh-CN.tsv` 覆盖全部码常量与全部 `stage` / `prompt.<kind>` 键；宽表列名等于 `locales/` 下文件名 | PASS：`locale_covers_codes_stages_prompts` |
| 提权路径的 `Coded` 经 postcard 帧往返后 `code` / `detail` / `subject` / `sid` 不变 | PASS：`ipc/mod.rs::coded_error_survives_pipe`；e2e `test:online-install` 走真实提权管道通过 |
| e2e 十项（`test:all`）全绿 | PASS：CI（windows-2022，`x86_64-win7-windows-msvc` 产物）十项通过 |
| `host/native.rs` 与 `main.rs` 不再 `use rfd::MessageDialog`；`rfd` 只剩目录选择所需 | PASS，且超出判据：`rfd` 与 `raw-window-handle` 整体移除，`cargo tree` 无 `rfd`；`Cargo.lock` 少 40 个传递 crate |
| `recursion_depth_exceeding_limit` 不出现 | PASS：见 [Native 重构 CR 第二轮遗留](./2026-09-02-native-refactor-cr-followup.md) |
| 体积 | 本机 x86_64-pc-windows-msvc、opt-level=z + LTO、非 build-std、`src-tauri` 内构建、含前端 HTML：契约落地前 3,058,688 → 落地后 3,046,912 字节（−11,776）；其中 `index.html.zst` 68,026 → 17,269，新增 `i18n.tsv.zst` 2,451，Rust `.text` 2,336,768 → 2,366,976。CI 产物（x86_64-win7-windows-msvc + build-std + optimize_for_size）2,720,768 → 2,710,528（−10,240） |

## Consequences

- 收益：三个界面共用一份状态机与一张文案表；错误在代码里只有码，文案、上报、`fail` 维度、insight 码都从码派生，文本匹配消失；自定义 HTML 只需实现 `ui-state` / `intent` 与插件宿主两条契约。
- `Uncoded` 上报是发现漏挂码的机制：真实用户错误若以 `INTERNAL_ERROR` 呈现并上报，按错误上报后端的分组补码。`attach` 只在知道操作语义的层调用，评审时对新增 `attach` 保持敏感。
- `prefer-user` 策略下可写目录不再提权：这是 `probe_dir` 取代旧 `inspect_dir` 后的行为变化，旧实现对已存在目录恒报 `Unwritable`。
- `PLUGIN_FAILED` 一码两用：插件宿主启动失败与插件执行失败共用此码，文案按前者写；若两者需要区分呈现，应拆出 `PLUGIN_HOST_FAILED` 并改码表。`SHORTCUT_FAILED` 目前没有挂码点（卸载快捷方式创建失败挂 `FILE_IO_FAILED` + subject）。`MIRRORC_UNREACHABLE` 的文案是新写的，旧代码在此路径抛原始错误。
- `Intent::Cancel` 在会话层是空操作，`Cancelled` 只由用户拒绝确认的路径构造；进行中取消未实现。
- 错误上报事件仍以整条链逐个 `to_string` 作 `exception.values`、无 fingerprint，分组尚未按 `code` 收敛；`detail` / `subject` 进入 extra 也未做。C 类继续上报但对话框无复制按钮。
- `Coded.detail` 含路径、Win32 错误文本、HTTP body 片段，进入上报 extra 与用户可复制的对话框：URL 已剥除，body 截断 512 字节；Mirror酱 API 的 `msg` 不回显 CDK。
- `UiState` 携带 `mirrorc_cdk` 明文推给前端，自定义 HTML 能读到它；凭据写入仍只在 Rust。
- 文案键与代码常量分离，改名会漂移：`locale_covers_codes_stages_prompts` 守覆盖率；缺键时 `t(key)` 返回键名本身，界面可见但不崩。
- `UiSession::apply` 集中了原本散在两个前端的推导，是新的复杂点，由逐意图单测覆盖；`run_install` / `run_uninstall` 本体只多了 `base` 参数。
- 自定义 HTML 若只实现 `ui-state` / `intent` 而不实现插件宿主协议，以插件源打包的安装器会在 `session-plugin` 上等待超时。
- serde `Content` 机制在二进制中仅剩 `SourceField` 的 untagged 残余（约 2.4 KiB）；新增需要反序列化的公开形状时沿用手写解析或外部标签，不再引入内部标签派生。
