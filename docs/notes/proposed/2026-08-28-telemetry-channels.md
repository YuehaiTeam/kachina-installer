# 遥测通道职责收敛

Status: proposed

## Problem

仓库中并存三套遥测，职责重叠且各有噪音。错误上报后端无差别接收 `capture_anyhow`（`utils/error.rs` 的 `TACommandError::serialize` 对每个跨 IPC/bridge 边界的错误无条件上报），弱网、文件占用、用户取消等环境错误淹没真实缺陷。计数遥测端点（`session/ui.rs` 的 `send_ev_insight`）承载 `format!("{err:#}")` 整段错误文本，属高基数自由文本，无法聚合。下载网络错误已有 DFS insight 结构化短码通道（随下载会话回传调度器），与前两者内容重复。

## Proposal

三通道各归其位：

| 通道 | 职责 | 禁止内容 |
|---|---|---|
| DFS insight | 网络侧全部：节点性能、下载错误短码 | 非网络错误 |
| 错误上报后端 | 缺陷：过滤后的错误、panic、native crash | 环境/用户错误 |
| 计数遥测端点 | 低基数可枚举 counter（prepare/finish/uninstall 及维度） | 自由文本 |

计数端点的 `error` 事件删除；若需保留安装成功率指标，以 `fail` counter + 粗分类维度（`network`/`disk`/`permission`/`hash`/`cancelled`/`other`，基数控制在十个左右）替代。

错误上报过滤以类型标记实现：`session/error.rs` 增加零大小标记类型 `Expected`，`user()` 构造的错误在 anyhow context 链中携带该标记；捕获层（现阶段 SDK 的 `before_send`，后续最小客户端沿用同一判断函数）以 `err.chain()` downcast 检查，携带标记者不上报。`hide()` 不自动豁免——其中部分是协议异常（如 dfs2 会话响应格式错误），属缺陷信号。`fail` counter 的分类维度与该过滤判断共用同一函数，避免两处漂移。

## Alternatives considered

- 字符串黑名单匹配错误码：与 DFS insight 短码体系耦合，错误文本措辞变化即失效。
- 全量上报、依赖服务端 inbound filter：浪费流量与配额，过滤规则在服务端漂移、仓库内不可见。
- 按 `hide()` 一刀切豁免：会吞掉协议异常类缺陷信号。

## Acceptance criteria

- 触发弱网下载失败、文件占用、用户取消场景，错误上报后端不产生新 event；触发内部不变量类错误（如 IPC 协议解析失败）仍产生 event。
- 计数遥测端点全部事件 payload 无自由文本字段，`fail` 维度取值集合 ≤ 10。
- 过滤判断与 `fail` 分类由同一函数实现，有单测覆盖每个分类。

## Risks

- 过滤过度：真缺陷被误标 `Expected`。缓解：只有 `user()` 自动携带标记，评审时对新增 `user()` 调用点保持敏感。
- 成功率指标在 `error` 事件删除与 `fail` counter 上线之间断档。缓解：同一变更内完成替换。
