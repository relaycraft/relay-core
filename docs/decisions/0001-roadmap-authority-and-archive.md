# 0001 · 路线图权威性与历史文档归档

- **Status**: Accepted
- **Date**: 2026-09-12
- **Supersedes**: 无
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)、[`.ai/README.md`](../../.ai/README.md)

## 背景

仓库内长期存在多份各自声明权威性的规划文档：

- `.ai/full-roadmap-2026.md` 自称「2026 H2 → 1.0 期间**唯一的对齐文档**……凡与历史 spec 冲突以本文档为准」；
- 新增的 `docs/l7-first-engine-evolution-roadmap.md` 定位为「长期对齐文档」；
- 两者对同一批工作（如 M7 benchmark/性能门禁）各有一套说法与进度口径。

更严重的是**完成度口径与代码现状不一致**：历史进度表记录 M6/M7「已完成/进行中」，
而 `benchmarks/results/` 只有 v0.8.2 / v0.8.3 两个基线，当前版本为 0.10.0；
审计还发现多项能力「存在于代码与公开 API，但未作用于线路」（见路线图 §24）。

这带来两个具体风险：
1. 协作者与 AI agent 可能依据过期的进度表判断能力状态并对外宣称；
2. 历史文档中的设计（例如「修改 Flow 即等于修改线路」）会持续误导实现。

## 决策

1. **`docs/l7-first-engine-evolution-roadmap.md` 是 engine 数据面与协议能力演进的唯一对齐文档。**
   `docs/proxelar-rama-learning-directions.md` 只负责「参考来源与借鉴优先级」，不拥有独立排期。
2. **历史路线图全部归档**至 `.ai/archived/`（按主题分子目录：`2026-h2-roadmap/`、
   `relaycraft-integration/`、`planning/`），**仅作历史背景，不再作为推进目标**。
3. **完成度一律以代码、测试与 CI 实测结果为准**，不以任何历史文档的进度表为准。
4. **归档采用移动、不删除**；后续新增历史材料放入 `archived/<topic>/`，不回填顶层。
5. **关键决策写入 `docs/decisions/`（对外可见）；内部讨论留在 `.ai/`（仅本地）。**

## 理由

- 单一权威文档是消除「口径互相矛盾」的唯一低成本手段；两份平行路线图必然漂移。
- 归档而非删除：历史决策链本身是有价值的信息，且已投入的写作成本不应作废。
- 以实测为准：本项目已出现「文档声称完成、线路实际未生效」的实例，进度表不再是可信信号。
- `.ai/` 被 `.gitignore:63` 忽略，因此**不能**承担对外决策记录的职责；而归档属于内部沿革，
  留在 `.ai/` 是合适的。

## 影响

- **正面**：agent 与协作者有唯一入口；路线图 §24 提供了「哪些能力不得标 Stable」的清单；
  `AGENTS.md` 已指向该文档并在文档纪律中写入该约束。
- **代价（需知晓）**：`.ai/` 被忽略，因此**归档内容不进仓库、协作者不可见**，
  历史沿革只存在于本地工作区。新文档中指向 `.ai/archived/**` 的链接对 clone 者是死链。
  若日后需要团队可见的历史归档，应迁移到 `docs/archive/`（未被忽略）。
- 两份新文档目前为**中文单语正文**，与 `AGENTS.md` §3 的双语要求存在差距，已在文档内标注英文版待补。

## 相关证据

- 归档结构：`.ai/README.md`
- 能力失真清单：路线图 §24（含 file:line）
- 完成度矛盾的实例：`benchmarks/results/` 仅含 `baseline_v0.8.2/0.8.3`，`Cargo.toml` 版本为 `0.10.0`
- 文档纪律约束：`AGENTS.md` §3「Alignment source / Archived」
