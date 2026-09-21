# 安装过程信息展示对齐

Status: proposed

## Problem

对照重构前的 `ae461aa9ddd5a8f5e445459e14f5d611921f9938` 与 `a52a4c66a9e75d86c130be2e029345a761829c24`，安装过程丢失了速度、活动文件列表和逐文件进度。这些信息有助于判断程序是否仍在工作、哪个任务耗时较长，以及剩余工作是否变化。问题同时存在于进度数据生成与前端展示，单改样式无法恢复。

基准版本的 `src/App.vue` 在下载期间生成总体完成量、总体速度和可滚动的活动文件列表，各文件显示完成量与总量；Mirror酱 下载也显示速度。当前[进度汇总](../../../native/session/run.rs)只输出总体计数与第一个未完成活动文件的名称，[Progress](../../../native/session/state.rs)不包含速度或活动文件列表。[运行界面](../../../web/screens/Running.tsx)据此渲染一行状态和总体计数。

| 差异 | 用户可见结果 | 依据 |
|---|---|---|
| DFS 和 Mirror酱 速度缺失 | 无法直接观察当前吞吐及停顿。 | [进度数据与采样](../../../native/session/run.rs)、[前端类型](../../../web/state.ts) |
| 活动文件及逐文件进度缺失 | 一个文件名可长期保持不变，其他并发任务完成也看不出来。 | [DownloadProg](../../../native/session/run.rs)、[Running](../../../web/screens/Running.tsx) |
| 总体计数放在文件名后，整行省略 | 长文件名可能把总体完成量挤出可见区域。此项为静态布局判断，尚未运行界面验证。 | [进度布局](../../../web/layout.css) |
| 创建 DFS2 会话及预取地址未切换阶段 | 已进入网络等待，界面仍显示校验本地文件。 | [创建会话、预取与进度更新的顺序](../../../native/session/run.rs) |
| Mirror酱 下载始终显示准备文案 | 字节数已增长，文字仍称“准备从Mirror酱下载”。 | [中文文案](../../../locales/zh-CN.tsv)、[Mirror酱 下载回调](../../../native/session/run.rs) |
| 差分更新的总量与上报量口径不同 | 最终文件大小作为总量，补丁读取量作为完成量，成功时直接补齐。以 10 MB 补丁生成 100 MB 文件为示例，会表现为较小完成量后突然跳满；这是计算示例，不是测量结果。 | [任务总量和完成处理](../../../native/session/run.rs)、[补丁进度来源](../../../native/ipc/install_file.rs) |

[UI 契约](../implemented/2026-09-02-ui-contract.md)和[Preact 渲染器](../implemented/2026-09-02-frontend-preact-renderer.md)说明了新的结构化进度和状态行，但未明确记录移除速度、活动列表及逐文件进度的产品决定。速度计算残留的时间与字节字段也未产生展示数据。

本地校验数量、运行库名称和正常下载计数仍然存在；Mirror酱 解压保留文件名并增加数量，提交阶段新增应用更新及提交计数。旧版卸载没有逐文件详情。删除操作已并入提交，展示也应反映这一流程。

## Proposal

### 恢复总体统计与活动文件列表

总体统计独立占一行，显示完成量、可确定的总量及速度；活动文件使用可滚动的列表，显示名称和各自进度。长文件名可截断并显示省略号，总体计数和逐文件计数应完整可见。列表只显示活动任务，并保持顺序稳定，避免每次刷新重新排列。

后端持有任务状态，前端负责格式化和布局。`Progress` 携带统计与活动项的原始数据。合并下载也按文件提供活动项，分别展示已开始处理的文件及其进度。

### 进度接口

拟采用以下 Rust 类型作为 `Phase::Running(Progress)` 的载荷。它由会话生成，native 直接读取，WebView 经 `ui-state` 接收完整快照。`Phase` 的 JSON 仍以 `kind` 区分相位，running 的字段位于同一对象内。

```rust
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub step: Option<u8>,
    pub stage: ProgressStage,
    pub subject: Option<String>,
    pub percent: Option<f64>,
    pub cancel: CancelState,
    pub summary: Option<ProgressCounter>,
    pub processing_bps: Option<u64>,
    pub network_bps: Option<u64>,
    pub network_pending: bool,
    pub files: Vec<FileProgress>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStage {
    Prepare,
    FetchMetadata,
    ScanFiles,
    PrepareDownload,
    CreateDownloadSession,
    ProcessFiles,
    DownloadArchive,
    VerifyArchive,
    ExtractArchive,
    Commit,
    DownloadRuntime,
    InstallRuntime,
    CreateShortcuts,
    WriteRegistry,
    Finalize,
    UninstallScan,
    UninstallDelete,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    Available,
    Requested,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressCounter {
    pub unit: ProgressUnit,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressUnit {
    Bytes,
    Files,
    Operations,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileProgress {
    pub id: u32,
    pub name: String,
    pub action: FileAction,
    pub bytes: Option<ByteProgress>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Download,
    Extract,
    Patch,
    Verify,
    Flush,
    Retry,
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteProgress {
    pub done: u64,
    pub total: Option<u64>,
}
```

对应的 TypeScript 类型如下。所有字段必定存在，Rust 的 `None` 序列化为 `null`，空列表为 `[]`；不使用 `skip_serializing_if` 或前端可选属性表达这些状态。

```typescript
export type ProgressStage =
  | 'prepare' | 'fetch_metadata' | 'scan_files'
  | 'prepare_download' | 'create_download_session' | 'process_files'
  | 'download_archive' | 'verify_archive' | 'extract_archive' | 'commit'
  | 'download_runtime' | 'install_runtime' | 'create_shortcuts'
  | 'write_registry' | 'finalize' | 'uninstall_scan' | 'uninstall_delete';

export type CancelState = 'available' | 'requested' | 'unavailable';
export type ProgressUnit = 'bytes' | 'files' | 'operations';
export type FileAction = 'download' | 'extract' | 'patch' | 'verify' | 'flush' | 'retry';

export type ProgressCounter = {
  unit: ProgressUnit;
  done: number;
  total: number | null;
};

export type ByteProgress = {
  done: number;
  total: number | null;
};

export type FileProgress = {
  id: number;
  name: string;
  action: FileAction;
  bytes: ByteProgress | null;
};

export type Progress = {
  step: number | null;
  stage: ProgressStage;
  subject: string | null;
  percent: number | null;
  cancel: CancelState;
  summary: ProgressCounter | null;
  processing_bps: number | null;
  network_bps: number | null;
  network_pending: boolean;
  files: FileProgress[];
};

export type RunningPhase = { kind: 'running' } & Progress;
```

native 与前端尚未正式发布，将共同替换 `Progress` 定义及其使用处，无需兼容旧接口。自定义 HTML 示例同步更新，生产打包格式保持不变。

### 字段含义与展示规则

| 字段 | 约定 |
|---|---|
| `step` | 安装四步列表中的当前位置，取 `0..=3`；`null` 表示不展示步骤列表，卸载使用此值。步骤标题仍由来源类型选择默认或 Mirror酱 文案。该字段控制分组，`stage` 描述组内具体工作。 |
| `stage` | 有限的阶段枚举，渲染器查 `progress.<stage>`。安装成功、已最新和卸载成功只由 `Phase::Done` 表达。 |
| `subject` | 阶段文案需要的单个对象，例如运行库名称；没有对象时为 `null`。并发文件放入 `files`，不把列表首项复制到此字段。 |
| `percent` | 后端计算的整次操作进度，已知时为有限的 `0..=100`，未知时为 `null`，渲染器显示不定进度。它独立于字节统计，不由前端用 `summary` 再计算。取消收尾可以保持最后的进度值，但不能补到 100。 |
| `cancel` | 后端决定的取消状态，具体受理和展示规则见下文。 |
| `summary` | 当前阶段总体计数，包括该阶段已完成、活动及尚未开始的工作；不是活动列表之和。`null` 表示没有计数信息。 |
| `processing_bps` | 后端采样得到的处理字节数每秒，单位为 B/s。`null` 表示当前阶段不提供速率或样本不足；`0` 表示有有效采样但没有处理进展。渲染器只格式化，不自行差分。 |
| `network_bps`、`network_pending` | 网络接收速率与当前是否仍有下载工作，计量和速度选择遵守[网络速度与离线展示提案](./2026-09-21-network-speed-and-offline-progress.md)。 |
| `files` | 当前已经启动、尚未结束的文件任务，按 `id` 稳定排序；完成后移除，排队任务不进入列表。没有活动文件时为 `[]`，不能据此推断整个阶段完成。 |

`ProgressCounter.unit` 显式表达计量单位，取代 Rust 和前端各自维护的 `BYTE_STAGES`。`bytes` 格式化为字节量，`files` 显示文件数，`operations` 显示操作数。哈希扫描和归档解压的总体数量使用 `files`，提交单元使用 `operations`，字节处理阶段使用 `bytes`。计数可以为零；`total: null` 表示未知，`total: 0` 表示已知没有工作，不以 1 代替零分母。有总量时 `done <= total`，实现发现预估失效时更新总量或改为未知，不由渲染器静默截断。

`FileProgress.id` 是本次会话内分配的文件序号，同一文件重试、fallback 和混合补丁切换动作时保持不变。`name` 为原始大小写的相对路径或文件名，长名称截断后提供完整名称提示；列表以 `id` 标识各项，避免不同目录下的同名文件冲突。`bytes` 只描述当前动作，未知动作进度时为 `null`；有已处理量但未知总量时使用 `{ done, total: null }`。校验和刷盘阶段可以没有字节进度，任务仍留在列表并显示相应动作。重试等待使用 `retry` 和 `bytes: null`，开始下一次尝试后重置当前动作计数。

| 阶段组 | 默认安装 | Mirror酱 安装 |
|---|---|---|
| `step: 0` | 准备、获取 metadata | 准备、获取 metadata |
| `step: 1` | 扫描本地文件 | 准备、下载归档 |
| `step: 2` | 准备下载、创建会话、处理文件、提交 | 校验归档、解压归档、提交 |
| `step: 3` | 运行库、快捷方式、注册表和收尾 | 运行库、快捷方式、注册表和收尾 |

恢复未完成提交也使用 `stage: commit`；步骤列表表达当前阶段位置，不作为逐组执行记录。字节处理期间活动列表可以同时出现 `download`、`extract`、`patch`、`verify` 和 `flush`，无需为每个文件切换整个会话的 `stage`。

### 取消状态与快照更新

`available` 显示可点击的取消按钮；用户确认后发送现有 `Intent::Cancel`。`requested` 表示会话已接受取消，按钮禁用，界面提示取消已受理，正在等待任务结束及清理。该提示优先于普通进度，`stage` 和 `files` 仍可说明尚在收尾的工作。`unavailable` 表示当前工作不接受取消，安装界面保留禁用按钮；卸载界面仍不显示取消按钮。

取消权限与受理结果统一来自会话快照，替代渲染器中的 `NO_CANCEL` 阶段名单和本地 `cancelling` 状态。命令发送期间可以暂时禁止重复点击。会话在接受取消或进入不可取消阶段时立即发布状态，不等待下一次采样。具体先后关系与清理前提遵守[取消修复提案](./2026-09-20-refactor-review-fixes.md)。

`ui-state` 沿用现有事件通道，每次携带完整的 running 快照，前端整体替换。阶段切换即清除不属于新阶段的计数、速率和文件列表。会话退出 running 后停止发布其进度；任务结束后迟到的回调不能重新加入活动列表。

### 数据生成与传递

`DownloadProg` 或同等会话汇总器保存每个文件的当前动作、当前尝试的有效计数，以及计算速度所需的累计实际处理量。失败尝试的计数可以从完成量中撤回，实际处理过的字节仍计入当时的速度。汇总结果由会话一次性写入 `Progress`，渲染器直接展示快照中的统计值。

文件操作必须上报足以区分传输、解压、补丁、校验和刷盘的事件，提权执行路径也需要传递这些变化。字节处理完成后仍有校验和刷盘，任务完成应以操作结束事件为准。活动项的 `id` 由会话关联到操作及合并组内文件，执行侧事件提供该操作的当前动作和计数，文案由渲染器选择。合并组中等待处理的文件仍计入总体计划。

字节汇总取传输、解压和补丁的有效完成量，速率取这些动作的实际处理量；校验重读和刷盘只更新动作，不增加处理量。已完成任务的计数留在当前阶段汇总中，完成的列表项移除。实现复用现有计数和进度设施，在执行侧补充所需的动作事件，由会话转换为 UI 结构体。

### JSON 示例

以下快照表示一个文件完成了 4 MiB 数据处理、正在校验，另一个文件的 10 MiB 本地补丁已处理 2 MiB，当前没有网络下载。总体 6 MiB 是处理量，不是磁盘上最终文件的大小；速度为示例值。

```json
{
  "kind": "running",
  "step": 2,
  "stage": "process_files",
  "subject": null,
  "percent": 52.1,
  "cancel": "available",
  "summary": { "unit": "bytes", "done": 6291456, "total": 14680064 },
  "processing_bps": 1048576,
  "network_bps": null,
  "network_pending": false,
  "files": [
    { "id": 0, "name": "bin/app.exe", "action": "verify", "bytes": null },
    { "id": 1, "name": "Assets/data.bin", "action": "patch", "bytes": { "done": 2097152, "total": 10485760 } }
  ]
}
```

### 明确速度和完成量的含义

`processing_bps` 表示每秒处理的字节数，包含本地解压和补丁处理。界面按[网络速度与离线展示规则](./2026-09-21-network-speed-and-offline-progress.md)选择速率，将其格式化为 `1.2 MB/s` 这样的数值，与总体完成量并列显示。

每个任务的完成量和总量必须来自同一个计量位置。普通直写若使用解压后的写入量，总量也使用输出大小；补丁若使用补丁输入量，总量就使用对应输入大小。混合补丁的本地基文件准备与补丁处理需区分动作，不能把两段从零开始的计数当作同一段连续进度。不能可靠获得总量时，只显示已处理量和当前动作。

速度使用实际发生的字节变化采样，排除任务结束时的进度补齐和阶段切换造成的计数变化。文件完成进度按本次有效尝试表达；改变处理方式后重新确定对应总量，避免重复累计成功结果。总体进度条继续表示安装阶段进度。

速率每秒用累计实际处理量的差值与单调时钟的实际间隔计算一次，并在两次采样之间保持该值；首个完整采样前为 `null`，没有字节变化的采样为 `0`。计时器独立于字节回调，即使下载停顿也会更新零速率。切换到不统计速率的阶段时为 `null`。

### 阶段文案与实际工作同步

建立 DFS2 会话前发布 `create_download_session`，预取地址时发布 `prepare_download`，启动文件任务后发布 `process_files`。Mirror酱 准备连接时使用 `prepare_download`，开始收到数据后使用 `download_archive`，校验和解压分别使用 `verify_archive`、`extract_archive`。运行库使用 `download_runtime`、`install_runtime`，通过 `subject` 携带名称。阶段键直接对应文案表中的准备、下载、校验等动作。

提交阶段保持现有原子文件操作流程，界面展示提交状态和操作计数。该阶段不可取消，按 `cancel: unavailable` 展示。

### 保持两个渲染器的信息含义一致

WebView 使用列表呈现，native 按对话框能力使用文本行或摘要；两者展示的总体量、速度、当前动作及取消状态应表达相同含义。错误对话框保留现有详细信息和复制能力。

继续使用 Preact、现有文案表及原生样式。字节变化按约 100 ms 汇总发布，阶段及取消状态变化立即发布。活动列表使用稳定 `id` 更新，不引入虚拟列表库或全局状态库。若需要优化汇总，采用[可选性能优化](./2026-09-21-optional-performance-optimizations.md)中的会话内增量计算。

## Alternatives considered

- 只在前端加速度计算和列表：当前状态没有活动项，完成量又混有阶段变化和结束补齐，前端无法可靠还原。应补齐后端原始数据，再做展示。
- 由后端拼接 HTML 并高频刷新：耦合会话与渲染，并增加无必要的更新频率。使用结构化状态和现有框架。
- 仅保留一个文件名，其他详情要求用户查日志：用户需要在安装界面直接看到各活动任务的进度，以判断程序是否仍在工作。
- 把所有处理字节都称为下载量或网络速度：离线解压、补丁、重试的计量位置不同，会误导用户。处理计数保留独立含义，网络统计采用[专门的计量规则](./2026-09-21-network-speed-and-offline-progress.md)。
- 为了维持总体进度单调，隐藏重试或在完成时补出速度：会掩盖重试和停顿。阶段百分比和实际处理统计分开表达。
- 一并增加 ETA、完整任务历史或逐文件卸载详情：这些不是本次对齐所缺失的信息，暂不扩展。

## Acceptance criteria

### 需要保留的回归测试

长期测试保护计数、任务状态和界面行为，复用现有会话单测与[组件测试](../../../web/__tests__/render.test.tsx)。下表按行为组织，可以共用输入表和夹具，无需每行新建一个测试文件。

| 要测试的场景 | 必须核对的结果 | 验证方式 |
|---|---|---|
| 普通写入、补丁、本地解压；混合补丁切换动作；重试及 fallback | 完成量与总量来自相同计量位置；混合补丁两段计数不误接成连续进度；重试不重复累计有效完成量；结束补齐不进入速率 | 使用可控进度事件的汇总单测，保留补丁输入大小明显小于最终文件大小的样例 |
| 多个活动文件，其中两个路径不同但文件名相同；合并组中还有未开始的文件 | 活动项身份和顺序稳定，重试不换 `id`，排队文件不提前显示；一个文件停顿不影响其他文件进度；已完成项移除但计数留在汇总中 | 一组活动项生命周期单测，组件测试验证多个文件及各自计数可见 |
| 字节处理完成后仍在校验或刷盘；切换阶段；会话结束后收到迟到回调 | 文件操作结束前活动项保留并显示对应动作；阶段切换清除无关计数、速率和列表；完成项与已结束会话不会被迟到事件恢复 | 可控动作事件与完整快照序列，前端确认新快照替换旧内容 |
| 计数单位分别为 bytes、files、operations；未知总量、已知零总量和无计数；未知百分比 | 按显式单位格式化；三个计数状态可区分，不伪造分母；未知百分比显示不定进度，已知百分比采用后端值 | 一个共享进度样例及少量输入变体，检查 Rust 序列化字段、TypeScript 消费和组件输出 |
| 首次采样、连续处理、无字节变化、重试重置和阶段切换 | 首个完整样本前为 `null`；按实际时间差计算速率；停顿归零；处理进度回退不产生负速率，阶段变化不产生虚假速度 | 传入可控时间和字节样本的速率单测；网络速度复用此采样测试 |
| DFS2 创建会话或预取地址处于等待；Mirror酱 从连接准备进入接收 | 等待开始前发布对应阶段，Mirror酱 接收数据后进入下载阶段；阶段变化及时发布，不依赖下一次字节采样 | 会话测试用可控 future 检查状态发布时机，不访问外部服务 |
| 取消状态由 available 变为 requested 或 unavailable，随后仍有普通进度 | 按会话状态决定按钮权限；已受理取消提示持续可见，进度刷新不覆盖它；卸载保持无取消按钮 | 复用组件状态序列；提交竞态和清理顺序由[取消修复验收](./2026-09-20-refactor-review-fixes.md)覆盖 |

JSON 样例在后端序列化检查与前端消费检查之间共用，避免两边各自维护一份看似一致的数据。断言聚焦字段含义、单位和空值，不锁定 JSON 属性顺序、完整 DOM 或整段文案。网络计数及在线、离线速度选择的回归测试由[网络速度提案](./2026-09-21-network-speed-and-offline-progress.md)负责，两份提案共用测试入口。

### 一次性验收

以下项目在功能完成时检查并记录结果；相关布局或实现改变时再复核，无需为此次改动新增长期快照库、性能门槛或 e2e 矩阵。

| 要检查的场景 | 验收结果 |
|---|---|
| 默认窗口下的长路径、多个活动文件和列表滚动 | 名称截断后能查看完整内容，总体及逐文件计数、速度完整可见，总体统计不随列表滚走；用真实界面或已有视觉工具检查，jsdom 文本断言不替代布局检查 |
| native 与 WebView 展示同一组代表性进度 | 两者的完成量、单位、动作、速度含义和取消状态一致；native 摘要能表达仍在进行的工作，错误详情和复制能力保留 |
| DFS、Mirror酱、运行库和提交的实际展示 | 阶段与实际工作相符，运行库名称、Mirror酱 解压文件名及数量、提交操作数保留；文案可读且没有缺失的翻译键 |
| 活动列表连续更新，已有大量完成任务 | 列表和快照只保留活动项，完成量仍正确；检查序列化和渲染开销，确认历史任务没有随快照长期累积 |
| 功能实现前后的构建产物 | 在同一工具链、target 和 release profile 下记录压缩前端资产与 installer exe 的体积差异，说明新增依赖或明显增长的原因 |

已有组件与文案检查继续复用。具体措辞、像素位置、某次二进制体积和机器相关耗时不增加固定断言；这些项目的验收结果与必要的截图或测量条件一并保留。

### 验证记录

实施时记录上述关键回归测试的用例名和执行结果，一次性验收记录环境及观察结果。未执行的场景明确保留缺口；两项功能共用的检查只记录一份证据并相互引用。

## Risks

总量定义改变会影响进度曲线。实施前应核对各模式产生进度数据的位置，确保完成量和总量的口径一致；无法确定总量时使用 `null`。

活动项增加状态序列化和渲染成本；只传活动项并沿用现有展示频率，避免为每个字节或完整历史推送状态。重试和混合补丁容易使累计值重复，必要单测集中保护这些计数规则。

实施涉及[UI 契约](../implemented/2026-09-02-ui-contract.md)、Rust、TypeScript、两个渲染器、自定义 HTML 示例、文案与夹具，必须共同更新。native 对话框展示空间有限，摘要仍需让用户看出哪些工作尚在进行。
