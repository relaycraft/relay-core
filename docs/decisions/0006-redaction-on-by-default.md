# 0006 · 敏感信息默认脱敏

- **Status**: Accepted
- **Date**: 2026-09-13
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§16）、[`../engine-capability-status.md`](../engine-capability-status.md)（§2.2b）

## 背景

`RedactionPolicy::default()` 此前是 `enabled: false`。这意味着 `Authorization`、`Cookie`、
`X-Api-Key` 以及 `token` / `access_token` / `password` 等查询参数**原样写入 SQLite、原样由 API 返回**。

脱敏链路本身是完整且正确的（落盘前、输出前、SSE 快照、历史重写都覆盖），问题只在**默认值**：
只要有人接上来读流量，凭据就开始泄漏。对本项目即将用于的用途——**headless 代理 + MCP 给 AI 消费**——
这个默认值等于「AI 第一次连上就看到原始凭据」。

## 决策

1. `RedactionPolicy::default().enabled` 改为 **`true`**。
2. `redact_bodies` **保持 `false`**（body 脱敏仍是 opt-in）。
3. 脱敏范围仍是**固定的敏感名单匹配**（header 名 + query key），不做启发式猜测。

## 理由

- **默认值不是中性的**。「不脱敏」不是「保持原样」的中立选择，而是「默认外泄」——
  它把安全性建立在「使用者一定会先配置」这个假设上，而 API 与 MCP 的默认行为恰恰是让人直接开始读。
- **代价有界且可预期**。header/query 名对**固定清单**匹配，因此不会产生误判式的内容改写；
  开启后的代价是「有些 header 值看不到」，而不是「流量被改变」。
- **body 必须保持 opt-in**。body 脱敏会**改变载荷内容**，而规则与脚本可能合法地需要读到原始字节
  （例如签名校验、加解密）。把这条也默认打开会静默破坏这类用法。
- 与决策 [`0002`](./0002-body-observation-policy.md) 的取向一致：**宿主可见行为由显式策略决定**，
  但**安全相关的默认值应向安全一侧倾斜**，需要放宽时由使用者显式声明。

## 影响

- **正面**：默认配置下，落盘与 API 输出都不再包含明文凭据；MCP/AI 消费者默认看不到 `Authorization`。
- **代价（需知晓，且是发布可见的行为变更）**：
  - 依赖原始 header 的下游（调试、审计脚本、把流量喂给自建分析器）**会看到 `[REDACTED]`**，
    需要显式关闭脱敏才能恢复原行为；
  - 这属于**行为变更**，应在 release notes 中明确说明，不能只靠提交信息；
  - 脱敏发生在**落盘前**，因此对已存在的历史数据不会自动生效——需要显式触发历史重写
    （打开脱敏时会自动跑一次，见决策与实现说明）。
- **不改变**：body 内容默认不被改写；规则与脚本看到的 body 仍是原始字节。

## 相关证据

- 默认值：`relay-core-api/src/policy.rs`（`impl Default for RedactionPolicy`）
- 落盘前脱敏：`relay-core-runtime/src/actors/flow_store.rs`（序列化前）
- 历史重写：`relay-core-runtime/src/lib.rs`（`redact_history_with`，脱敏 `false → true` 时自动执行）
- 契约：`relay-core-api/tests/serde_tests.rs`（默认策略必须脱敏；body 仍为 opt-in）
