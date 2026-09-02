# UI 契约：Rust 只推状态，渲染器只渲染

Status: proposed

## Problem

安装器有两个第一方界面（WebView 前端 `src/`、TaskDialog 原生界面 `src-tauri/src/host/native.rs`）和一个无界面路径（silent），三者共用 `src-tauri/src/session/` 的会话逻辑，但界面层的状态与文案由每个界面各自维护，Rust 在其中扮演"应答命令 + 推送拼好的文本"的角色。具体表现：

1. **两套状态机重复推导同一批事实。** `host/native.rs` 的 `ReadyState { path, source_uri, create_lnk, delete_user_data, mirrorc_cdk, is_update, is_uninstall }` 与 `src/App.vue` 的十九个 `ref`（`step`、`subStep`、`percent`、`current`、`source`、`selectedSource`、`isUpdate`、`needElevate`……）各自从 `InstallerConfig` 推导安装路径、当前源、是否为更新、是否需要提权。`src/bootstrap.ts` 还承担了业务判断：嵌入索引与文件表的一致性校验、卸载元数据是否存在、`autoRun` 决策、`uacNeeded` 提权判定——这些在 `session/types.rs` 的 `settings_from_input` / `settings_from_cli` 里有 Rust 版本。
2. **Rust 推送的是渲染结果而不是状态。** `SessionUi::progress` 携带的 `ProgressEvent.current` 是拼好的中文句子，`session/run.rs` 里 16 处常量文本、约 8 处 `format!`（文件名、运行库名、经 `format_size` 格式化的字节数），其中两处含 HTML（`<br>`、`<div class="d-single-stat">`）；`host/native.rs` 用 `plain_progress` 把 `<br>` 替换回换行，前端用 `v-html` 渲染。`SessionUi::confirm(kind, title, message)` 已有 `PromptKind` 枚举却仍传拼好的 `title`/`message`，占用文件列表、进程列表、版本号被埋进字符串。`SessionUi::alert(title, message)` 三处是非致命错误的文本版。`SessionUi::reopen_source()` 是 Rust 向前端下发的流程指令。
3. **错误是中文字符串。** `session/error.rs` 有 12 个中文常量（`PKG_BROKEN`、`META_FAILED`、`HASH_INVALID`、`UNINSTALL_META_MISSING`、`PATH_INVALID`、`TEMP_DIR`、`NO_DOWNLOAD_NODE`、`DFS2_SESSION`、`FILE_MISSING`、`PLUGIN_NO_UI`、`PLUGIN_NEED_WEBVIEW2`、`PLUGIN_HOST_FAILED`），`hide(user_msg, err)` 把原始错误丢进日志、用中文替换；`friendly()` 内含四句网络错误文案；`run.rs` 的 `mirrorc_error` 是 Mirror酱 数字码到中文的映射表并附带 `reopen` 位；[遥测通道职责收敛](../implemented/2026-08-28-telemetry-channels.md) 的 `classify()` 靠 `Expected` 标记加文本匹配（`kind_from_text` 对中文常量与 `HASH_MISMATCH_ERR` 等短码做 `contains`）决定上报与 `fail` 维度。`dfs.rs` 的 `SHORT_INSIGHT_CODES` / `short_insight_code` 是另一套文本匹配。前端 `src/mirrorc-errors.ts` 维护着同一张 Mirror酱 映射表的 TS 版本。bridge 的 `on_message` 在 `Err` 分支发 `{ message: format!("{:#}", err), insight }`，前端只能拿到字符串。
4. **计数遥测经前端中转。** `SilentUi` 与 `NativeUi` 直接调用 `session/ui.rs` 的 `send_ev_insight`，而 `GuiUi` 把 `session-insight` 事件发给前端、前端再经 HTTP 发出；`bootstrap.ts` 与 `App.vue` 的 `uninstall()` 还自行发起会话开始与卸载事件，Rust 侧不知道它们的存在。遥测不是界面职责，跨一次 IPC 只带来两条实现。
5. **界面文案没有单一来源。** 中文散布在 `session/run.rs`（49 行）、`host/native.rs`（30 行）、`session/error.rs`（21 行）、`module/wv2.rs`、`main.rs`、`App.vue` 及各组件。`locales/` 目录存在但为空。

这些让第三方定制界面（打包时以自定义 HTML 替换 `\0IMAGE` 槽位）几乎不可能：它们要重新实现 `bootstrap.ts` 的推导、解析 Rust 拼出的句子、并接受 Rust 决定的错误文案与流程。

## Proposal

### 原则

Rust 是推状态的后端：推**键**与**原始数据**（枚举、数字、文件名、原始错误文本），不推句子、不推格式化后的数字、不推标记。查表、插值、`format_size`、HTML 全部属于渲染器——WebView 路径在前端做，native 路径在 `host/native.rs` 做，两者读同一张文案表。渲染器对 Rust 只发**意图**（用户动作），不发决策结果。

### 状态结构体与意图

Rust 持有唯一一份界面状态，每次变化整体推送；渲染器全量套用。结构体不足 1 KiB，不做 diff / patch 协议。

```rust
// src-tauri/src/session/state.rs（新建）
#[derive(Serialize, Clone)]
pub struct UiState {
    pub phase: Phase,
    pub mode: Mode,                  // Install | Update | Uninstall，由路径探测与参数推导
    pub project: ProjectView,        // window_title, title, description, borderless
    pub options: Options,            // install_path, source_uri, create_lnk, delete_user_data, mirrorc_cdk
    pub sources: Vec<SourceItem>,    // 可见源：id, name, uri, icon(可选 SVG 文本), requires_webview
    pub path: PathState,             // writable: Writable | Unwritable | Private, exists, upgrade
    pub needs_elevate: bool,         // 推导值，渲染器不再自行计算
    pub cdk: CdkStatus,              // Idle | Checking | Ok | Invalid(Coded)
    pub theme: Theme,                // None | Image | Css | Html，见"主题槽位"
    pub pending: Option<Prompt>,     // 有值即显示模态确认
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase {
    Ready,
    Running(Progress),
    Done(SessionResult),
    Failed(Coded),
}

#[derive(Serialize, Clone)]
pub struct Progress {
    pub sub_step: u32,               // 步骤指示器索引（现有语义不变）
    pub percent: f64,
    pub stage: &'static str,         // 文案键，见"进度阶段键"
    pub subject: Option<String>,     // 当前对象：文件名、运行库名
    pub done: Option<u64>,           // 字节或计数
    pub total: Option<u64>,
}

#[derive(Serialize, Clone)]
pub struct Prompt {
    pub id: String,
    pub kind: &'static str,          // process_running | occupied_files | version_mismatch
    pub items: Vec<String>,          // 进程名 / 文件名列表
    pub params: BTreeMap<&'static str, String>, // version_mismatch 的 local / remote
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    SetPath { path: String },
    SetSource { uri: String },
    SetCreateLnk { value: bool },
    SetDeleteUserData { value: bool },
    SetCdk { cdk: String },          // Rust 负责校验与凭据读写，结果反映在 UiState.cdk
    Start,
    Cancel,
    Answer { id: String, ok: bool },
    Dismiss,                         // Failed -> Ready
    Launch,
    Advanced,                        // native 专用：切换到 WebView 界面
    Close,
}
```

`UiState` 只放两个渲染器都必须一致表达的会话状态。哪个面板展开、动画、hover 属于渲染器自己的视图状态，不进结构体。`Phase` 用带数据的枚举，进度只存在于 `Running`、结果只存在于 `Done`、错误只存在于 `Failed`，消灭"step=3 但 percent=40"一类不可能状态。主题图片 / CSS 等二进制不进状态，见"主题槽位"。

### 传输

- Rust → WebView：事件 `ui-state`，payload 为完整 `UiState`。经现有 `HostHandle::emit`。
- WebView → Rust：命令 `intent`，参数为 `Intent`。以下 bridge 命令随之删除：`get_installer_config`、`select_dir`、`start_install`、`start_uninstall`、`answer_session_prompt`、`read_uninstall_metadata`、`wincred_read` / `wincred_write` / `wincred_delete`、`get_mirrorc_status`。`select_dir` 的目录选择对话框由 `SetPath` 之前的一个新命令 `pick_path` 触发（弹系统目录选择框并返回路径；空目录 / 非空目录追加应用名子目录的判断留在 Rust，与 `host/native.rs` 的 `apply_path_choice` 共用）。
- 保留：`launch`、`launch_and_exit`、`log` / `warn` / `error`、`window_*`、插件宿主的 `plugin_host_ready` / `answer_session_plugin` / `http_get_request`，以及新增的 `error_dialog`、`task_dialog`（见"错误出口"）。
- native：`host/native.rs` 的 `loop { show_ready_page → finish_action }` 结构不变，`ReadyState` 换成 `UiState`，`ReadyAction` 换成 `Intent`，`show_finish` 是 `Phase::Done` 的渲染，`ProgressDialog` 是 `Phase::Running` 的渲染，`NativeOutcome::Web` 对应 `Intent::Advanced` 由宿主层处理。
- silent：不渲染，`Phase::Failed` 时日志一行 `code: detail`（无 detail 时只有 code），退出码 1。

### 会话状态机

新增 `session/state.rs` 的 `UiSession::apply(&mut self, intent: Intent)（commands.rs 已占用 SessionState）`，负责：更新 `options`；`SetPath` / `SetSource` 之后重算 `path`、`mode`、`needs_elevate`、`sources` 可见性；`SetCdk` 触发 Mirror酱 状态查询并读写凭据（现由 `MirrorcDialog.vue` 与 `mirrorc_target` 分别实现）；`Start` 进入 `Running` 并调用 `run_install` / `run_uninstall`；`Answer` 转给 `PromptHub`；`Dismiss` 回 `Ready`。`bootstrap.ts` 中的嵌入索引一致性校验、卸载元数据存在性检查、`autoRun` 决策迁入 Rust 的会话初始化，结果以 `Phase::Failed(PKG_BROKEN)` / `Failed(UNINSTALL_INFO_MISSING)` 或直接 `Start` 表达。`SessionUi` trait 精简为：

```rust
pub trait SessionUi: Send + Sync {
    fn state(&self, state: &UiState);                      // 整体推送
    async fn confirm(&self, prompt: Prompt) -> bool;       // 经 UiState.pending 呈现，等待 Answer
    fn notify(&self, coded: Coded);                        // 非致命错误，事件 ui-notice
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>>;
}
```

`progress`、`alert`、`insight`、`reopen_source` 从 trait 移除。 Step 3 落地时 `run.rs` 尚未持有 `UiSession`，因此 `SessionUi::progress` 仍作为默认方法保留（构造仅含 `Phase::Running` 的 `UiState` 再 `state()`）；`GuiUi` 覆盖该方法，把进度写进现有会话并 emit `ui-state`，不再发 `session-progress`。
进度经 `state()` 推送；`alert` 改为 `notify(Coded)`（`RUNTIME_INSTALL_FAILED` 带 `subject = 运行库名`、`detail = 原始错误`），前端与 native 用与 `Failed` 相同的默认错误对话框呈现；`reopen_source` 由前端根据 `MIRRORC_CDK_*` 码自行决定。

### 错误

错误由三部分组成，来源不同，代码里只出现第一部分：

| 部分 | 来源 | 例子 |
|---|---|---|
| 码 | 挂码点，`&'static str` 常量 | `PERMISSION_DENIED` |
| 原始信息 `detail` | 源错误本身：OS / 库 / 对端 API 返回的文本，Rust 只转录 | `拒绝访问。(os error 5)`、`503: <body 前 512 字节>`、`expected value at line 1 column 1`、Mirror酱 返回的 `msg` |
| 文案 | 文案表按码查得 | 不在 Rust 源码中 |

```rust
// src-tauri/src/utils/code.rs（新建）
#[derive(Debug, Serialize, Clone)]
pub struct Coded {
    pub code: &'static str,
    pub detail: Option<String>,   // 源错误链 {:#}，剥除 URL；无源错误时 None
    pub subject: Option<String>,  // 操作对象：文件名、注册表键、host
    pub sid: Option<String>,      // DFS 会话 id，仅下载类
}
```

- **构造只有两种方式**：对已有错误 `.attach(code)` / `.attach_with(code, subject)`（`Result` 与 `anyhow::Error` 上的扩展 trait），或无源错误的 `Coded::bare(code)` / `Coded::bare_with(code, subject)`。不提供接收消息字符串的构造器；`anyhow!("中文").attach(code)` 与 `anyhow!("developer note").attach(code)` 都不合规——前者是文案，后者不是原始信息。
- **`attach` 的语义是"若尚未挂码，则为此码"**：链上已有 `Coded` 时原样返回。内层操作知道自己的语义先挂码，外层 `attach` 是兜底；外层想覆盖说明内层挂错了，改内层。
- **挂码点在知道操作语义的那一层**，不在传输层。`utils/url.rs` 的 `HttpContextExt` 只保留上下文，不挂码；`fetch_metadata` 挂 `METADATA_*`，下载流挂 `DOWNLOAD_*` 并带 `sid` / `subject = host`，`get_mirrorc_status` 的调用方挂 `MIRRORC_*`。网络错误细分用 `reqwest::Error::is_timeout()` / `is_connect()` 与 `io::ErrorKind` 判定，不做文本匹配。
- `Coded` 作为链节点入 anyhow 链（`anyhow::Error::new(Coded{..})` 包住源错误并实现 `source()`），`Display` 只输出码，`{:#}` 渲染因此以码开头。
- **边界提取** `extract(&anyhow::Error) -> Extracted { Cancelled | Coded(&Coded) | Uncoded { detail } }`，遍历链、先查 `Cancelled` 标记再查 `Coded`。`Uncoded` 是缺陷，`detail` 为整条链的 `{:#}`。用户取消是独立标记类型 `Cancelled`，不是码。
- **上报由码的类决定**：N（下载本身）、E（用户机器环境）、U（用户输入）不上报；C（打包方配置）、S（服务端 / 第三方服务）、M（第一方元数据 API）上报；`Uncoded` 上报。Sentry 标题与 fingerprint 只用 `code`（`Uncoded` 用 `INTERNAL_ERROR`），`detail` / `subject` 作 extra，不参与分组。`fail` counter 的 `kind` 维度报类（`n` / `e` / `u` / `c` / `s` / `m` / `uncoded` / `cancelled`），[遥测通道职责收敛](../implemented/2026-08-28-telemetry-channels.md) 的"维度取值 ≤ 10"判据保持。`utils/error.rs` 的 `TACommandError::report_if_needed` 与 `host/native.rs`、`main.rs` 的上报门改为 `extract` + 类表；`classify()`、`Expected`、`FailKind`、`kind_from_text`、`user()`、`expected()`、`hide()`、`friendly()`、`file_release()` 及 12 个中文常量删除；`dfs.rs` 的 `SHORT_INSIGHT_CODES` / `short_insight_code` 删除，`InsightItem.error` 直接取 `Coded.code`。
- bridge `on_message` 的 `Err` 分支发 `{ code: string | null, detail: string | null, subject: string | null, insight }`。会话失败时 `Phase::Failed(Coded)` 同时进 `ui-state`。
- `dfs.rs` 的 API 函数（`get_dfs`、`get_dfs2_metadata`、`create_dfs2_session`、`get_dfs2_chunk_url`、`get_dfs2_batch_chunk_urls`、`end_dfs2_session`、`http_get_request`）返回值从 `Result<T, String>` 改为 `anyhow::Result<T>`。
- `run.rs` 的 `mirrorc_error` 改为 `fn(status) -> Option<Coded>`：数字码映射到 `MIRRORC_*`，`detail` 放 API 原始 `msg`，删除 `reopen` 位。

错误码初版（类、是否上报、`subject` 约定）：

| 类 | 码 | subject |
|---|---|---|
| N 不上报 | `DOWNLOAD_TIMEOUT` `DOWNLOAD_REFUSED` `DOWNLOAD_FAILED` `DOWNLOAD_STALLED` `SERVER_HTTP_ERROR` `HASH_MISMATCH` `SOURCE_NEEDS_VERIFICATION` | host / 文件名 |
| E 不上报 | `PERMISSION_DENIED` `DISK_FULL` `FILE_IN_USE` `FILE_IO_FAILED` `TEMP_DIR_UNAVAILABLE` `PROCESS_KILL_FAILED` `REGISTRY_WRITE_FAILED` `SHORTCUT_FAILED` `ELEVATE_FAILED` `RUNTIME_INSTALL_FAILED` `WEBVIEW2_REQUIRED` `WEBVIEW2_FAILED` `SELF_UPDATE_FAILED` | 文件 / 键 / 运行库名 |
| U 不上报 | `MIRRORC_CDK_MISSING` `MIRRORC_CDK_EXPIRED` `MIRRORC_CDK_INVALID` `MIRRORC_CDK_MISMATCH` `MIRRORC_CDK_QUOTA_EXCEEDED` `MIRRORC_CDK_BANNED` `INSTALL_PATH_INVALID` `PLUGIN_FAILED` | — |
| C 上报 | `PKG_BROKEN` `SOURCE_INVALID` `VERSION_REGEX_INVALID` `MIRRORC_CONFIG_INVALID` `PLUGIN_NO_UI` `PLUGIN_NOT_FOUND` `RUNTIME_UNSUPPORTED` `UNINSTALL_INFO_MISSING` `HASH_ALGORITHM_UNSUPPORTED` | 源 uri / 插件名 |
| S 上报 | `SOURCE_METADATA_INVALID` `REMOTE_FILE_MISSING` `NO_DOWNLOAD_NODE` `EXTRACT_FAILED` `MIRRORC_FAILED` `MIRRORC_UNREACHABLE` | 文件名 |
| M 上报 | `METADATA_UNREACHABLE` `METADATA_HTTP_ERROR` `METADATA_INVALID` | — |
| 缺陷 上报 | `INTERNAL_ERROR`（仅文案键，不作为挂码目标） | — |

Mirror酱 数字码映射：`1001 / 8001 / 8002 / 8003 / 8004 → MIRRORC_CONFIG_INVALID`，`7001 → MIRRORC_CDK_EXPIRED`，`7002 → MIRRORC_CDK_INVALID`，`7003 → MIRRORC_CDK_QUOTA_EXCEEDED`，`7004 → MIRRORC_CDK_MISMATCH`，`7005 → MIRRORC_CDK_BANNED`，其它非零 → `MIRRORC_FAILED`。

### 错误出口

Rust 里只有一个默认错误处理实现 `utils/taskdialog.rs::show_error(coded: &Coded, parent)`：TaskDialog，标题 = 文案表[code]，主文 = `subject`（有则显示），展开区 = `detail`，脚注 = `code`，一个"复制"按钮把 `code`、`subject`、`detail` 写入剪贴板并保持对话框打开（回调返回 `S_FALSE`）。`Uncoded` 用 `INTERNAL_ERROR` 的文案。

bridge 暴露两层：`error_dialog({ code, detail, subject })` 直通 `show_error`；`task_dialog({ title, content, expanded, footer, buttons })` 是底层原语，给自定义 HTML 自己组文案和按钮用。

边界规则：WebView 存活期间的错误全部进 `ui-state`（`Failed`）或 `ui-notice`，由前端决定调 `error_dialog` 还是自己渲染；WebView 尚不存在或已销毁时（WebView2 缺失、插件宿主启动失败、宿主初始化失败）Rust 直接调 `show_error`。遥测（上报判定、`fail` counter、insight）在会话边界完成，不依赖任何渲染器调用，自定义 HTML 无法跳过。

`main.rs`、`host/mod.rs`、`host/native.rs`、`session/ui.rs`、`ipc/manager.rs`、`installer/mod.rs`（`error_dialog`、`confirm_dialog`）、`module/wv2.rs` 中的 `rfd::MessageDialog` 全部收敛到 `show_error` / `task_dialog`；`rfd` 仅保留 `AsyncFileDialog`（目录选择）一个用途。

### 进度阶段键

`stage` 取值与文案键 `progress.<stage>` 一一对应，插值参数在括号内：`prepare`、`metadata`、`hash_scan`、`plan`、`download(subject, done, total)`、`patch(subject)`、`extract(subject, done, total)`、`delete(subject)`、`runtime_download(subject, done, total)`、`runtime_install(subject)`、`shortcut`、`registry`、`finalize`、`uninstall_scan`、`uninstall_delete(subject, done, total)`、`mirrorc_metadata`、`mirrorc_download(done, total)`、`mirrorc_verify`、`done`。`session/run.rs` 的 `progress(ui, sub_step, percent, text)` 辅助函数改为 `progress(ui, sub_step, percent, stage, subject, done, total)`。`App.vue` 现有的 `subStepList` / `subStepListMirrorc` 两组步骤标题改为文案键 `step.default.<n>` / `step.mirrorc.<n>`，渲染器按 `sources` 中当前源是否为 `mirrorc://` 选择。

### 计数遥测

`send_ev_insight` 及 `run.rs` 的 `insight_base` 留在 Rust，会话开始事件与卸载事件由 `run_install` / `run_uninstall` 包装层发出（`fail` counter 已在此处）。`GuiUi` 不再 `emit("session-insight")`；前端 `sendInsight` / `insightBase` 及其调用全部删除。

### 文案表

仓库形态：`locales/<lang>.tsv`，每行 `KEY\t文案`，一个语言一个文件，翻译只碰自己的文件。错误码直接作 key；其它字串用带前缀的 key（`progress.*`、`step.*`、`prompt.<kind>.title` / `.message`、`ready.*`、`done.*`、`dialog.*`）。占位符写作 `{subject}`、`{done}`、`{total}`、`{items}`、`{local}`、`{remote}`。

构建形态：`src-tauri/build.rs` 读取 `locales/*.tsv`，按 key 字典序合并为宽表 `i18n.tsv`——首行表头 `KEY\t<lang1>\t<lang2>…`（列名取文件名），缺失翻译留空单元格——zstd 压缩后作为资产条目 `i18n.tsv` 加入 `host/assets.rs` 的 `get()`。`cargo:rerun-if-changed` 指向 `locales/` 目录。

运行时：Rust 与前端都经 `assets::lookup("i18n.tsv")` 拿同一份字节。Rust 侧 `utils/i18n.rs` 解码后按表头找列、按 key 找行，插值只做 `{name}` 直接替换，供 native 渲染器与 `show_error` 使用；语言由宿主决定一次（系统 UI 语言，无匹配列时用第一列）并放入 `UiState.project.lang`。native 与 silent 之外的 Rust 代码不得读文案表。

选型依据（46 条、zh-CN 真实文案 + en-US 近似长度英文、zstd level 22）：每语言单独一个 zstd 帧合计 2,486 字节，两语言并入一帧 2,239 字节，三列宽表 2,165 字节；JSON 与 TSV 在压缩后相差 27 字节，TSV 的 Rust 解析是 `lines()` + `split('\t')`，不引入 `serde_json` 对 `HashMap<String, String>` 的单态化。

### 主题槽位

打包格式不变，`\0IMAGE` 槽位继续一槽多用，识别从 `bootstrap.ts` 的 `processEmbeddedImage` 迁到 Rust 运行时：`RIFF….WEBP` 魔数为图片；`28 B5 2F FD` 为 zstd 帧，解开后首个非空白字符为 `<` 是 HTML、否则是 CSS；其余按现有规则（前 16 字节可打印 ASCII）视为明文 CSS。识别结果进 `UiState.theme: None | Image | Css | Html`；字节本体走资产端点：图片 `theme.webp`、CSS `theme.css`；HTML 直接替换 `index.html` 条目（`host/assets.rs` 的 `lookup` 已为此预留）。`InstallerConfig.embedded_image` 不再以 base64 进入任何命令返回值。空包不携带默认图片，`theme == None` 时渲染器不显示图片区域。

### 插件宿主

插件宿主继续加载与主界面同一份 HTML（`plugin_runtime_setup` 的 `index.html?pluginHost=1`），插件在前端 bundle 内注册；自定义 HTML 替换 `index.html` 后同时成为插件宿主，因此可以只改 HTML 就引入新插件，不必改 Rust。协议不变：宿主监听 `session-plugin` 事件（`PluginEvent { id, method, name, url, range, diffchunks, insights }`），以 `answer_session_plugin({ id, ok, data?, error?, unimplemented? })` 应答，启动完成后调用 `plugin_host_ready`。此协议是自定义 HTML 必须实现的两条契约之一（另一条是 `ui-state` / `intent`）。

### 落地顺序

与 [前端重写为 Preact 渲染器](./2026-09-02-frontend-preact-renderer.md) 是同一连续任务，不经 Vue 适配层。WebView 路径在 bridge 切到 `ui-state` / `intent` 的同一批改动里换成 Preact；旧 `.vue` 与 `bootstrap.ts` 直接删除。

1. `utils/code.rs`、`session/state.rs`、`utils/i18n.rs`、`build.rs` 的 `locales/` 合并、`locales/zh-CN.tsv` 初稿（把现有中文原样搬入）。Rust 单测覆盖 `attach` / `extract` / 类表、`SessionState::apply`。
2. 会话层迁移：`SessionUi` 精简、`run.rs` 进度与提示改为键与数据、错误挂码、`mirrorc_error` 改码、`dfs.rs` 返回 `anyhow::Result`、遥测回到 Rust、旧错误工具与常量删除。
3. bridge 与 Preact 渲染器一起：`ui-state` / `intent` / `error_dialog` / `task_dialog` / `pick_path`，旧命令删除；`on_message` 的 `Err` 载荷改为结构体；第一方前端按渲染器 note 重写，e2e 在这一批结束时全绿。
4. native 渲染器：`ReadyState` → `UiState`，`rfd::MessageDialog` → `show_error` / `task_dialog`，文案从 `i18n.rs` 取。可与第 3 步并行，不依赖 Preact。

## Alternatives considered

- 只做错误码、不动状态机：错误是"Rust 拼文案推前端"的一个特例，单独治理后进度、提示仍是句子，自定义 HTML 仍要解析文本；两者动的是同一批文件（`SessionUi`、`run.rs`、`bridge.rs`、前端事件处理），一起做少一遍回归。
- 错误码与文案一起放在 Rust 常量中、以 `coded(code, msg)` 构造：码与文案同时出现在每个调用点，文案表形同虚设，多语言无从下手，`detail` 与用户文案混为一个字段。
- 在传输层（`HttpContextExt`）按"业务家族"参数挂码：把业务语义压进传输层，每个 HTTP 调用点都要声明自己属于下载还是元数据；挂码点放在知道语义的业务层后传输层无需知情。
- `fail` counter 直接报码：违反已定的维度 ≤ 10 判据；报类即可满足过滤与成功率口径，码在 Sentry 与日志中可查。
- 状态推送做增量 patch：结构体不足 1 KiB、变化频率以进度为上限（每秒数十次），全量推送的成本可忽略，patch 协议增加两端复杂度。
- 文案表用 JSON：压缩后与 TSV 相差 27 字节，但 Rust 侧多一份 `serde_json` 单态化（估 1–3 KiB `.text`）。
- 每语言单独一个 zstd 资产：比宽表多约 320 字节，且加语言要改资产清单；宽表加语言只是加一列，缺翻译一眼可见。
- 文案表并入 `index.html` 的压缩帧：native 路径为读 2 KiB 文案要解码整个前端 HTML，方向与 native 路径不依赖 WebView 的初衷相反。
- 新增打包字段标注 `\0IMAGE` 内容类型：改变包格式；魔数识别在运行时零成本且兼容既有包。
- 用 UI 自动化（CDP 驱动 WebView2、UIA 驱动 TaskDialog）验收：渲染器变薄后行为逻辑全部在 `SessionState::apply` 与 `extract` 中，用 Rust 单测与前端组件测试覆盖；UI 自动化只多覆盖"窗口真的弹出"一层，代价是 CI 交互桌面依赖与 flake。
- 先给现有 Vue 写一层 `UiState` → `ref` 适配、模板不动，再另开 Preact 重写：否。那等于独立重写一份 Vue 渲染器，契约形状会在过渡协议上冻一版，e2e 还要回归两遍。两篇 note 作为同一连续任务，bridge 切换与 Preact 替换同一批落地。

## Acceptance criteria

- `rg -n --pcre2 "[\x{4e00}-\x{9fff}]" src-tauri/src --glob '!**/tests/**'` 排除注释后，命中仅限 `utils/i18n.rs` 的测试夹具与 `host/native.rs` 中经 `i18n` 查表的键名常量；`session/`、`utils/error.rs`、`utils/code.rs`、`dfs.rs`、`fs.rs`、`module/wv2.rs`、`main.rs` 零命中。
- `session/ui.rs` 的 `SessionUi` 只有 `state`、`confirm`、`notify`、`plugin_host` 四个方法；`ProgressEvent.current`、`PromptEvent.title` / `.message`、`session-progress` / `session-prompt` / `session-insight` / `session-reopen-source` 事件名在仓库中不存在。
- `utils/code.rs` 单测：`attach` 幂等（首个码存活）、`extract` 三态、类表与上报判定逐类一例、`Cancelled` 优先于码、`detail` 剥 URL、`subject` 与 `sid` 透传。
- `session/state.rs` 单测：`SetPath` 到只读目录后 `needs_elevate == true` 且 `mode` 随 `upgrade` 变化；`SetSource` 切到 `mirrorc://` 后 `cdk == Idle` 且 `Start` 在 `cdk != Ok` 时进入 `Failed(MIRRORC_CDK_MISSING)`；`Answer { ok: false }` 于 `occupied_files` 后 `Phase::Done(SessionResult { cancelled: true })`；`Dismiss` 从 `Failed` 回 `Ready` 且 `options` 保持。
- silent 路径注入一个 `Failed(Coded { code: METADATA_HTTP_ERROR, detail: Some("500 …") })`，日志末行为 `METADATA_HTTP_ERROR: 500 …`，退出码 1。
- `build.rs` 单测（或 `utils/i18n.rs` 单测读取生成产物）：`locales/zh-CN.tsv` 覆盖 `utils/code.rs` 全部码常量与全部 `stage` / `prompt.<kind>` 键；宽表列名等于 `locales/` 下文件名。
- 提权路径的 `Coded` 经 [提权管道帧编码](../implemented/2026-09-02-ipc-postcard-frames.md) 的 postcard 帧往返后 `code` / `detail` / `subject` / `sid` 不变（`Coded` 不含 `serde_json::Value`，字段无 `skip_serializing_if`）。
- 现有 e2e 十项保持全绿（`test:all`），它们保的是主流程行为正确性，不为本提案新增用例。
- `host/native.rs` 与 `main.rs` 不再 `use rfd::MessageDialog`；`rfd` 在 `Cargo.toml` 的 feature 只剩目录选择所需。

## Risks

- 挂码遗漏会让真实用户错误以 `INTERNAL_ERROR` 呈现并上报：可接受，`Uncoded` 上报正是发现漏挂码的机制；上线初期按 Sentry 分组补码。
- 挂码过宽会静默缺陷：`attach` 只在知道操作语义的层调用，`Result` 上的 `attach` 不对整个函数体兜底；评审时对新增 `attach` 调用保持敏感。
- `SessionState::apply` 集中了原本散在两个前端的推导，成为新的复杂点：以单测覆盖每条意图，`run_install` / `run_uninstall` 本体不变。
- `UiState` 携带 `mirrorc_cdk` 明文推给前端：与现状一致（前端已持有 CDK 输入值），自定义 HTML 能读到它；凭据写入仍只在 Rust。
- 文案表键与代码常量分离，改名会漂移：`i18n` 单测校验覆盖率；缺键时 Rust 侧 `t(key)` 返回键名本身，界面可见但不崩。
- `Coded.detail` 含路径、Win32 错误文本、HTTP body 片段，进入 Sentry extra 与用户可复制的对话框：URL 已剥除，body 截断 512 字节；不含凭据（Mirror酱 API 的 `msg` 不回显 CDK）。
- 自定义 HTML 必须同时实现 `ui-state` / `intent` 与插件宿主协议，否则以该 HTML 打包的安装器在插件源上无法工作：文档化为两条契约，前端 note 提供最小实现。
