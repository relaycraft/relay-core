# 0002 · HTTP body 观察策略

- **Status**: Accepted
- **Date**: 2026-09-12
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§22、§24.2）、[`../mitmproxy-policy-benchmark.md`](../mitmproxy-policy-benchmark.md)

## 背景

「宿主要保留多少 body 供展示/存储」此前是**从「有没有 body 阶段规则」推断**出来的。这有两个问题：

1. 推断会**静默决定用户能看到什么**——桌面 UI 需要 body，无界面运行不需要；
2. 它挡住了宿主统一：桌面宿主已自行缓冲 body，若改为依赖代理保留，而代理的观察默认是 `Off`，
   则桌面端会**静默丢失 body 展示**。

因此需要把观察变成一个**显式策略**。可选项是「保留有界前缀（保持流式）」与「完整缓冲（按预算）」
两条路。用户给出的判据是：**对齐 mitmproxy 即可**。

## 决策

1. 新增 `BodyObservation { Off, Prefixed, Full }`，作为 `ProxyPolicy::body_observation`；
2. **引擎默认 `Off`**：无消费者时零拷贝，符合 §22「无修改场景不无谓缓冲」；
3. **桌面宿主（RelayCraft/Tauri）声明 `Full`**；
4. 桌面宿主不再自行缓冲：代理已保留 body（并在 `Flow.meta` 标记），interceptor 直接继续，
   消除重复读取。

## 理由

- **对齐判据 → `Full`**。实测 mitmproxy 12.2.3 的默认就是**完整缓冲** body（`stream=False`），
  仅在显式 `flow.response.stream = True` 时才流式，且一旦流式就**不再可读**
  （见对标文档 §1.3）。`Prefixed` 是 RelayCore 自己的折中，**不是对齐**。
- 选择 `Full` 后，桌面宿主观察到的 body 与 mitmproxy 同类行为**在预算内一致**，
  且保留了桌面既有的「按 1 MiB 预算截断」语义（`rule_body_inspect_budget` 与观察共用同一预算）。
- 引擎默认保持 `Off`，因此 **CLI / MCP / 无界面场景不受影响**，也不会因升级而开始无谓缓冲。

## 影响

- **正面**：观察不再是推断，宿主的可见行为由策略声明；桌面宿主消除了对同一 body 的**二次读取**；
  「body 规则看到了什么」与「UI 看到了什么」现在是同一个来源。
- **代价（需知晓）**：桌面宿主会按预算**物化**请求与响应 body（上限 `rule_body_inspect_budget`，
  默认 1 MiB），因此**大 body 失去流式**——这正是 mitmproxy 的默认代价，本决策是有意接受它。
  超过预算的 body 仍按截断处理并打 `rule_body_*` 标记。
- 观察是**策略而非每请求**开关：目前无法做到「小 body 全量、大 body 只留前缀」的自适应，
  因为响应方向必须在读取 body **之前**决定（见 §24.2 的两阶段约束）。

## 相关证据

- 决策类型：`relay-core-api/src/body_plan.rs`（`BodyObservation`、`decide`）
- 引擎默认与预算：`relay-core-api/src/policy.rs`（`body_observation`、`rule_body_inspect_budget`）
- 桌面声明：`relay-core-tauri/src/commands/system.rs`（代理启动前设置 `Full`）
- 跳过重复读取：`relay-core-tauri/src/interceptor.rs`（依据 `body_already_captured`）
- 对标实测：`docs/mitmproxy-policy-benchmark.md` §1.1–§1.3
