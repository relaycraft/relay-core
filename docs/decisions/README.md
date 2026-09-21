# Decisions / 关键决策记录

本目录存放**对项目外部可见的关键决策**（架构方向、契约兼容性、范围裁定、发布纪律）。

内部性、探索性、未定型的讨论仍放在 `.ai/`（该目录被 gitignore，仅本地可见）。

## 与 `.ai/` 的分工

| 目录 | 可见性 | 放什么 |
|---|---|---|
| `docs/` | 仓库内公开（开源可见） | 已定型的**关键决策**、能力清单、对齐文档 |
| `.ai/` | 仅本地（`.gitignore:63`） | 内部讨论、草稿、历史归档、未定方案 |

**迁移原则：只增不删。** 已进入 `.ai/` 的材料保留原位（归档用移动，不删除），
避免丢失信息；一旦某项讨论定型为对外决策，再在 `docs/decisions/` 新增一份正式记录。

## 约定

- 文件名：`NNNN-<kebab-case-title>.md`，四位递增编号。
- 结构：**背景 → 决策 → 理由 → 影响 → 相关证据**。
- 决策一旦被推翻，**不修改原文件**，而是新增一份并在其开头标注 `Supersedes: NNNN`。
- 每条决策应可回答：这个决定排除了哪些替代方案，代价是什么。

## 索引

| 编号 | 标题 | 状态 |
|---|---|---|
| [0001](./0001-roadmap-authority-and-archive.md) | 路线图权威性与历史文档归档 | Accepted |
| [0002](./0002-body-observation-policy.md) | HTTP body 观察策略 | Accepted |
| [0003](./0003-typed-events-alongside-flow-snapshots.md) | 类型化事件与 Flow 快照并存 | Accepted |
| [0004](./0004-dependency-advisories-owned-by-dependabot.md) | 依赖漏洞由 Dependabot 接管 | Accepted |
| [0005](./0005-circuit-breaker-defaults.md) | 熔断器默认值与可配置化 | Accepted |
| [0006](./0006-redaction-on-by-default.md) | 敏感信息默认脱敏 | Accepted |
| [0007](./0007-daemon-control-plane.md) | 守护进程控制面（daemon control plane） | Accepted |
| [0008](./0008-bounded-flow-retention.md) | 流量历史默认有界 | Accepted |
| [0009](./0009-daemon-observes-body-prefix.md) | Daemon 保留 body 前缀 | Accepted |
