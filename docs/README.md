# 文档

本目录是全库文档的唯一入口。规则见[文档标准](./AGENTS.md)，note 体裁见 [note 规范](./notes/AGENTS.md)。

## 结构

- `design/` — 设计目标：这个项目提供了什么，按层面各成一篇。
- [notes/](./notes/AGENTS.md) — 决策、任务与调研，按生命周期分四个目录：`proposed/`（提案）、`implemented/`（已实施）、`rejected/`（被否决）、`researched/`（纯调研）。
- `.agents/skills/`（仓库根）— 文档与代码审计用 skill：[trim-cot-leakage](../.agents/skills/trim-cot-leakage/SKILL.md)、[prose-standard](../.agents/skills/prose-standard/SKILL.md)、[archive-notes](../.agents/skills/archive-notes/SKILL.md)、[find-simplifications](../.agents/skills/find-simplifications/SKILL.md)。

## 查找

找决策与其落选方案：在 `docs/notes/` 下用 `rg` 搜主题词，文件名排序即时间排序。找当前行为的权威描述：读 `design/` 对应层面的文档。
