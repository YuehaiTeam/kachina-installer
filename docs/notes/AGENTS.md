# note 规范

note 是仓库文档体系中唯一的活动体裁，决策、任务与调研都以 note 的形式存在。本文定义 note 的生命周期、格式与存放规则；note 在文档分层中的位置见 [文档标准](../AGENTS.md)。

## 生命周期

note 的状态由所在目录编码，分类共四个，状态变化就是目录迁移，正文头部的 `Status:` 行随迁移保持与目录一致。

| 状态 | 目录 | 含义 |
|---|---|---|
| proposed | `proposed/` | 提议与任务：已规划、尚未实施；验证计划与判据记录于此 |
| researched | `researched/` | 纯调研：只做了分析，未规划、未实施任何修改 |
| implemented | `implemented/` | 已实施：正文用现在时描述现状与验收结果 |
| rejected | `rejected/` | 被否决：`Status:` 行注明原因；仅当仍有防重议价值时保留 |

```text
proposed ──实施──> implemented
   ├─调研──> researched
   └─否决──> rejected ──无防重议价值──> 删除
```

已完结且不再指导未来工作的 note 可归档冻结；归档判据与操作流程由 [archive-notes](../../.agents/skills/archive-notes/SKILL.md) 定义，归档目录 `archived/` 只存冻结历史，不属于活动分类。

任务也是 note。任务派发就是写一篇 proposed 状态的 note，其中包含背景、目标、验证计划与判据；完成后把结果并入同一篇，迁入 implemented。仓库中不存在单独的任务文件体裁。

## 命名与引用

note 文件名取 `yyyy-mm-dd-<slug>.md`，日期为该主题首次提出之日。note 之间的引用一律使用库内相对路径的 Markdown 链接（方括号内为显示名，圆括号内为相对路径）；裸标题、裸文件名、任务编号以及任何数字代称都禁止使用。notes 目录不设集中式索引，浏览按生命周期目录进行。

## 格式

每篇 note 的开头固定为三行。

```text
# <标题>

Status: <proposed|researched|implemented|rejected[ — <原因>]>
```

状态行不写日期也不带括注，日期只出现在文件名中。rejected 是唯一在状态行带内容的生命周期，因为"为什么被否"正是读者来找的事实。

正文小节随状态而异。所有状态都以 `## Problem` 小节开头，陈述动机，动机必须独立于方案成立。

proposed 状态依次包含 `## Problem`、`## Proposal`、`## Alternatives considered`、`## Acceptance criteria`、`## Risks`。其中 Proposal 描述拟进行的修改，允许使用将来时；Acceptance criteria 说明什么样的可观察状态才算完成；Risks 记录可能出错的地方以及已知的代价。Alternatives considered 必填，若当时未记录，以占位注解替代。

researched 状态包含 `## Problem`、`## Findings`、`## No action`。Findings 陈述事实结论及其来源，测量数据须同行注明条件；No action 明确写明本调研只做了分析，未规划也未实施任何修改。

implemented 状态包含 `## Problem`、`## Decision`、`## Alternatives considered`、`## Verification`、`## Consequences`。Decision 用现在时描述现状；Verification 承接 proposed 时期的验收条件，逐条转化为验收结果，而不是删除。例如：

| 判据 | 结果 |
|---|---|
| 冷启动 ≤50ms | PASS：…ms（<测量条件>） |

Consequences 记录取舍付出的代价与换来的收益。implemented 状态中禁止出现 Plan、Migration plan 等将来时标题。

rejected 状态冻结原 proposed 正文，只修改 `Status:` 行补充原因。

## 迁移

- proposed 迁 implemented 时，Proposal 改写为现在时的 Decision；Acceptance criteria 逐条转为 Verification 结果；Risks 折入 Consequences 或改写为现在时的 Testing。
- proposed 迁 researched 时，正文改写为 Findings 与 No action。
- proposed 迁 rejected 时，仅修改 `Status:` 行，正文冻结。
- 反向迁移一律禁止。

## 何时写 note

凡是非平凡的变更——改变行为、契约、跨文件语义或流程——都应产生或更新一篇 note。已被某篇 note 覆盖的决策，后续改名、改路径、改结构属于事实更新，直接修改该 note；决策本身被推翻则另立新 note 取代，旧 note 与新 note 交叉链接。纯机械、局部且语义无变化的编辑可以豁免。
