# 遥测通道职责收敛

Status: implemented

## Problem

仓库中并存三套遥测，职责重叠且各有噪音。错误上报后端无差别接收 `capture_anyhow`（`utils/error.rs` 的 `TACommandError::serialize` 对每个跨 IPC/bridge 边界的错误无条件上报），弱网、文件占用、用户取消等环境错误淹没真实缺陷。计数遥测端点（`session/ui.rs` 的 `send_ev_insight`）承载 `format!("{err:#}")` 整段错误文本，属高基数自由文本，无法聚合。下载网络错误已有 DFS insight 结构化短码通道，与前两者内容重复。

## Decision

三通道各归其位：

| 通道 | 职责 | 禁止内容 |
|---|---|---|
| DFS insight | 网络侧全部：节点性能、下载错误短码 | 非网络错误 |
| 错误上报后端 | 缺陷：过滤后的错误、panic、native crash | 环境/用户错误 |
| 计数遥测端点 | 低基数可枚举 counter（prepare/finish/uninstall/fail 及维度） | 自由文本 |

- `session/error.rs` 的 `Expected` 为环境/用户错误标记。与提案的零大小 context 标记不同，实现为携带消息与 `FailKind` 的根错误类型：anyhow context 的 Display 会污染 `{:#}` 用户可见输出，根错误的 Display 即用户消息则无此问题，且 `err.chain()` downcast 在任意层 context 叠加后仍可命中。
- `user()` 构造的错误自动携带标记；`expected(kind, msg)` 供显式指定维度。`file_release`（下载/释放失败的主要出口）也携带标记——不标记则弱网下载失败必然进后端，直接违反验收判据；其维度从原始错误文本的稳定短码推导。`hide()` 不豁免，其中的协议异常属缺陷信号。
- 过滤与分类由同一函数 `classify` 实现，返回 `{ kind, report }`：`report` 即 Expected 标记不存在；`kind` 依次由标记显式维度、`io::Error`/`reqwest::Error` downcast、稳定短码文本（`HASH_MISMATCH_ERR`、`DOWNLOAD_STALLED` 等仓库内常量）判定。
- 上报咽喉点全部过滤：`TACommandError::serialize`（GUI bridge）、`host/native.rs` 会话失败分支、`main.rs` silent 失败分支。`serialize` 在提权进程（pipe 模式）不上报——错误经管道回主进程后由会话边界统一上报，避免一次失败产生两个各持半边面包屑时间线的事件；代价是主进程故意忽略结果的提权操作（现仅卸载残留清理 `RmList`）出错无人上报。`serialize` 额外跳过携带 insight 的错误——它们是下载错误，其家是 DFS insight 通道。
- 直接以 `anyhow!(常量)` 构造的环境错误改经 `user()`/`expected()` 携带标记：`PKG_BROKEN`、`PATH_INVALID`、`TEMP_DIR`、`PLUGIN_NEED_WEBVIEW2`。`HASH_INVALID`（服务端配置错误）保持无标记，属缺陷信号。
- 计数端点的 `error` 事件已删除，替代为 `fail` counter + `kind` 维度（`network`/`disk`/`permission`/`hash`/`cancelled`/`other`）。发射点在 `run_install`/`run_uninstall` 包装层，GUI、native、silent 三条路径统一覆盖（原 `error` 事件只覆盖 silent 路径，成功率指标失真）；web 前端 `App.vue` 原有的两处自由文本 `error` 上报一并删除。

## Alternatives considered

- 字符串黑名单匹配错误码：与 DFS insight 短码体系耦合，错误文本措辞变化即失效。
- 全量上报、依赖服务端 inbound filter：浪费流量与配额，过滤规则在服务端漂移、仓库内不可见。
- 按 `hide()` 一刀切豁免：会吞掉协议异常类缺陷信号。

## Verification

| 判据 | 结果 |
|---|---|
| 弱网下载失败、文件占用、用户取消不产生 event；内部不变量错误仍产生 | 部分：`user()`/`file_release` 标记路径有单测（`user_error_is_expected_and_display_clean`、`file_release_download_error_is_expected_network`、`hide_error_is_reported`）；端到端待后端核对 |
| 计数端点全部事件 payload 无自由文本，`fail` 维度取值 ≤ 10 | PASS：`error` 事件已删除，`fail` 仅携带 `kind`，取值集合固定 6 个 |
| 过滤判断与 `fail` 分类由同一函数实现，单测覆盖每个分类 | PASS：`classify` 单一实现，8 个单测覆盖 6 个分类与两种上报判定 |

## Consequences

- 过滤过度的风险仍在：真缺陷被误标 `Expected` 即静默。只有 `user()`/`expected()`/`file_release` 携带标记，评审时对新增调用点保持敏感；`file_release` 的标记范围比提案更宽（包含潜在的 patch 缺陷路径），换取弱网场景零噪音。
- `fail` counter 上移到包装层后，silent 与 GUI 的成功率口径统一，历史 `error` 事件数据不可比。
