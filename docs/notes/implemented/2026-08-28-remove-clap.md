# 手写参数前端替换 clap

Status: implemented

## Problem

clap 是发布 stub 中最大的单一可移除依赖：`cargo bloat --release --crates`（本机 x86_64-pc-windows-msvc、非 build-std、opt-level=s + LTO）计 `clap_builder` 132KiB。CI 产物（x86_64-win7-windows-msvc + build-std + optimize_for_size）绝对数字有出入，但 crate 相对占比一致。

体积之外，clap 时期的解析行为有两处与设计意图不符。设计意图是"不认识的调用一律降级为默认交互安装"，但 `external_subcommand` 兜底只覆盖子命令位置的 token：未知 flag（`--foo`、`-Z`、`-D` 缺值）触发 clap 报错 `exit(2)`，而本程序是 GUI 子系统，双击场景下 stderr 无处输出，表现为静默退出。

同时不支持 DOS/NSIS 风格 slash 开关。Chocolatey、企业部署脚本对未知安装器习惯盲发 `/S` 或 `/VERYSILENT`；`/S` 不以 `-` 开头，落入 `external_subcommand` 兜底，表现为打开交互界面而非静默安装。

## Decision

installer bin 的参数解析由 `src/cli/mod.rs` 的手写前端承担，处理链依次为：

1. 原始命令行尾部 `/D=` 探测：`GetCommandLineW` 取原始字符串，token 起始处的 `/D=`（大小写不敏感）之后全部内容（含空格、不要求引号）作为安装目录，与 NSIS 语义一致。
2. slash 别名（大小写不敏感）：`/S`、`/VERYSILENT` → silent，`/SILENT` → non_interactive，`/DIR=<path>` → target；未知 slash token 丢弃。target 优先级：显式 `-D` > 原始命令行 `/D=` 尾部 > `/DIR=` token。
3. 子命令预分发：`argv[1]` 命中 `install`、`install-webview2`、`native-ui`、`headless-uac` 之一才进入对应解析，否则整个 argv 按安装参数宽松解析。`Command::Other` 变体删除。
4. `InstallArgs` 解析基于单一 `OptSpec` 声明表（short、long、takes_value、hidden、help、setter），解析循环与 `--help` 文本生成同源。解析永不失败：未知 token（flag、位置参数、slash 开关）逐个跳过，已识别选项照常生效——比"整体降级默认安装"更进一步，静默部署脚本带未知旗标时保留 `-S` 语义，不会退化成弹交互界面。
5. `--help`（含 `/?`）：有控制台时打印，无控制台时弹 rfd 对话框。

兼容矩阵（`-D value`/`-Dvalue`/`-D=value`、`--source value`/`--source=value`、bool 短旗标合并、`args_os` 非 UTF-8 路径）由对拍测试冻结：`cli::tests::oracle` 模块镜像原 clap 定义，21 组语法断言两边解析一致。

clap 保留在 `[dependencies]` 供 `kachina-builder` bin 使用（同 package 的 bin 共享依赖表，无法按 bin 拆分）；installer bin 不引用其符号，链接器不将其带入发布产物，出图以 `cargo bloat --filter clap` 为准。对拍测试直接使用该依赖，无需 dev-dependency。

## Alternatives considered

- bpaf（约 40–70KiB）/ argh（约 20–40KiB）：无法覆盖 slash 别名、原始命令行 `/D=` 尾部解析、永不硬失败兜底与对话框 help，这些仍需自写；库承担的仅剩 9 个选项的解析与 help 生成，且其"验证 + 报错退出"核心价值与兜底哲学冲突。
- gumdrop：体积合适但基本停止维护。
- 保留 clap 仅加 slash 预处理：放弃体积收益，未知 flag 静默退出仍需在 clap 外围绕过，绕过代码与手写解析规模相当。
- 未知输入整体丢弃降级默认安装（本提案初稿）：改为逐 token 宽松解析，因脚本化静默安装带未知旗标时丢弃 `-S` 会把无人值守流程退化成交互窗口。
- clap 降为 dev-dependency（本提案初稿）：不可行，`[[bin]]` 共享 `[dependencies]`，builder 需要 clap。

## Verification

| 判据 | 结果 |
|---|---|
| 兼容矩阵与 clap 解析一致 | PASS：`cli::tests::oracle_parity_on_supported_grammar`，21 组语法对拍全绿 |
| slash 开关解析 | PASS：`slash_aliases`、`d_tail_parses_unquoted_spaces`、`target_precedence_dash_wins_over_slash`（`/D=` 无引号含空格、`/DIR=` 带引号与否、`-D` 优先于 `/D=`） |
| 未知输入不退出、已知选项保留 | PASS：`lenient_unknown_input`（`--foo`/`-Z`/裸路径/`/NCRC` 得默认参数；`--typo -S`、`-ZS` 保留 silent） |
| clap 不进发布产物 | PASS：`cargo bloat --filter clap` 计 0B；`.text` 3.9MiB → 3.7MiB（约 -205KiB，含 clap 拉动的 std 部分）、文件 5.0MiB → 4.8MiB（`cargo bloat --release`，本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std） |
| `--help` 可见性 | PASS：`help_lists_visible_hides_hidden`（可见项 `-D`/`-I`/`-S`/`-O`/`-U`，隐藏长选项不出现）；无控制台分支为 rfd 对话框 |
| 单测与 e2e | 本地 44 项单测全过；e2e 矩阵中 offline-install 用例改用 `/S` 写法（`tests/utils.mjs` 的 `FLAGS_SLASH`），随 push 在 CI 执行 |

## Consequences

- slash 开关（`/S`、`/VERYSILENT`、`/SILENT`、`/D=`、`/DIR=`）成为对外兼容接口的一部分，包管理器盲发 NSIS/Inno 开关即可静默安装。
- 未知输入不再有任何报错出口，传参问题只能靠日志诊断；换来的是任何调用方式都不会静默退出或意外弹交互界面。
- 兼容矩阵的权威定义从 clap 行为变为对拍测试用例集；clap 仍在依赖表中，怀疑回归时可随时扩充对拍。
- 新增选项必须进 `OptSpec` 表（解析与 help 同源），就地 if-else 会绕过 help 生成，靠评审约束。
- builder bin 继续使用 clap，其 CLI 不受影响。
