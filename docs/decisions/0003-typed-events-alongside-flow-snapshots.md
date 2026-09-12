# 0003 · 类型化事件与 Flow 快照并存

- **Status**: Accepted
- **Date**: 2026-09-12
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§4-3、§4-4、§4-5、§18、§24.8）

## 背景

路线图 §4-4 要求「类型化事件模型」（`FlowStarted` / `HeadersReceived` / `BodyChunk` /
`MessageReceived` / `MutationApplied` / `InterceptPaused/Resolved` / `FlowCompleted/Errored`），
§4-5 要求 HTTP / Tauri / MCP **消费同一事件模型**。

`relay-core-api/src/event.rs` 已定义 `FlowEvent`（9 个阶段化变体）与 `CloseReason`，但**只有类型和单测**：
没有任何生产者，也没有任何消费者。与此同时，现有 `FlowUpdate` 是**快照语义**——它把整个 `Flow` 重新发一遍，
因此消费者无法区分「响应头到了」与「这次交换结束了」，也无法知道某次改动是谁做的、某个断点正在等待。

需要裁定的是：**事件模型与现有快照通道的关系**，以及**在只有部分事件存在真实生产者时，是否要补齐其余事件**。

## 决策

1. **并存而非替换。** `FlowUpdate` 继续作为快照通道（`event: flow`、Tauri `flow-update`、
   MCP `flows://` 通知），不改变其兼容窗口（§4-5「只做加法」）。类型化事件走**独立通道**
   （`event: flow-event`、Tauri `flow-event`），消费者两者都可读。
2. **只在有真实生产者处产生事件。** 本次接线三类：
   - `InterceptPaused` / `InterceptResolved`：在 `await_user_inspect` 产生——那里同时拥有
     结构化的 flow id、真实的 phase，以及等待**实际如何结束**（含超时路径）。
   - `MutationApplied`：每个**被应用**的规则产生一条，字段名由动作本身推导
     （`actions::mutated_fields`），而非 diff 前后 Flow。
3. **不合成其余事件。** `Started` / `HeadersReceived` / `BodyChunk` / `MessageReceived` /
   `Completed` / `Errored` 目前**不产生**。它们需要在代理的终止点（`proxy/http.rs` 的 14 处）
   与握手/帧路径上新增生产者，属于后续工作（§24.8 记为开放项）。
4. **`MCP` 只对有资源意义的事件发通知**：断点暂停/恢复会改变 `intercepts_pending`，
   因此通知 `proxy://status`；其余事件已由快照流覆盖，重复通知只是噪声。

## 理由

- **不合成是本次决策的核心。** 用「比较前后快照」来推断事件，正是 §4-4 要消除的那种推断：
  它能说出「Flow 变了」，却说不出**是哪条规则、改了哪个字段**——而这恰恰是 §18 的 AI 消费者
  需要的信息。宁可让 6 个变体暂时没有生产者，也不产出一个**看起来像事实的猜测**。
- **字段名从动作推导而非 diff。** `RuleTraceSummary::Modified` 只说「有规则执行了」；
  按动作枚举命名则**免费**得到「改了哪个字段」，且 `match` 穷尽——新增 `Action` 变体
  不补映射就**编译不过**，未命名的改动无法悄悄出现。代价是它描述的是**动作意图**，
  不是字节级差分（例如把 header 设为原值仍会报 `request.headers`）。
- **控制类动作为空集。** `Drop` / `Abort` / `Tag` / `SetVariable` / `Delay` / `RateLimit` 不声明任何字段：
  它们结束了交换或只动引擎内部状态，**没有字段被改动**。为它们编造字段名会让事件整体不可信。
- **事件在 `await_user_inspect` 而非 broker 产生。** broker 只持有 `"{flow_id}:{phase}"` 字符串键，
  在那里产生事件需要**解析键**或改 `InterceptService` 的签名（牵动 5 个 crate、18 处调用）；
  而 `await_user_inspect` 手上就是结构化的 `flow.id` 与 `phase`。
  代价是**手工注册的拦截**（Tauri/HTTP 桥各自直接 `register_intercept`）不产生这两个事件——
  但那条路径的发起者本身就是消费者，不需要被通知。

## 影响

- **正面**：UI 与 agent 可以**反应**而不是轮询——断点暂停有信号（此前只有「已解决」被审计），
  改动可归因到具体规则与字段。
- **代价（需知晓）**：
  - `fields` 是**动作意图**而非字节差分，因此可能出现「动作执行了但结果与原来相同」；
  - 6 个变体仍无生产者，**不得据此宣称 §4-4 已完成**；
  - 事件通道是 `broadcast`，落后消费者会跳过（`Lagged`），因此事件**不作为状态真相**——
    状态仍以快照为准。这是有意的：事件是提示，快照是事实。

## 相关证据

- 事件类型：`relay-core-api/src/event.rs`（`FlowEvent`、`CloseReason`）
- 线上契约：`relay-core-api/src/sse.rs`（`to_sse` / `parse_flow_event`，与 `FlowUpdate` 同一模块）
- 生产者：`relay-core-runtime/src/interceptors/inspect.rs`（暂停/恢复）、
  `relay-core-runtime/src/interceptors/rule.rs`（`publish_mutations`）
- 字段命名：`relay-core-lib/src/rule/engine/actions/mod.rs`（`mutated_fields`，
  `match` 穷尽即编译期约束）、`relay-core-lib/src/rule/engine/executor.rs`（`RuleMutation`）
- 通道：`relay-core-runtime/src/services/flow_event.rs`（`FlowEventHub`、`FlowEventSink`）
- 消费者：`relay-core-http/src/routes/events.rs`、`relay-core-tauri/src/commands/flow.rs`、
  `relay-core-probe/src/server.rs`
- 未生产的变体与理由：路线图 §24.8
