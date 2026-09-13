# 0005 · 熔断器默认值与可配置化

- **Status**: Accepted
- **Date**: 2026-09-13
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§15、§24.10）、[`../engine-capability-status.md`](../engine-capability-status.md)（§2.5）

## 背景

一次 `CONNECTIONS=100` 的实测暴露出熔断器会把**瞬时抖动放大成持续故障**：

| 观测 | 数值 |
|---|---|
| 上游 connect 失败 | 439 次（约占请求 0.08%） |
| `Circuit breaker OPEN` 次数 | 383 次 |
| 熔断拒绝（`Circuit breaker open for upstream`） | 约 12 万次 |
| 结果 | 成功率 62.6%、P99 21.68ms |

原默认是 `3 次失败 → 30s 全 host 拒绝`，且**不可配置**（`CircuitBreaker::default()` 是硬编码常量，
`ProxyPolicy` 无对应字段）。计数规则是「连续失败」——任何成功即清零——因此 3 次连续失败就足以
让该 host 在 30 秒内被完全拒绝。

## 决策

1. 阈值与退避**改为策略**：`ProxyPolicy::circuit_breaker = CircuitBreakerPolicy { failure_threshold, backoff_ms }`，
   `failure_threshold: 0` 表示**完全禁用**熔断。
2. 默认值由 `3 次 / 30s` 调整为 **`10 次 / 5s`**。
3. 熔断拒绝**单独计数**（`relay_core_proxy_circuit_rejected_total`），因为它计的是
   「本代理拒绝去尝试的请求」，**不是**上游失败。此前两者在监控上无法区分。

## 理由

- **30s 的放大倍率与证据不匹配**：0.08%（首轮）到 0.25%（次轮）的上游瞬时错误率，
  被放大成 21%–37% 的失败率。保护真正宕机的上游只需覆盖「持续失败」，不需要 30 秒。
- **暴发阈值保留但提高**：计数语义（连续失败、成功即清零）保持不变——它确实能识别
  「连续失败」这一上游不健康的信号，只是 3 次太低。10 次仍然能在真正宕机时快速保护
  （10 次失败后 5 秒内几乎全部拒绝）。
- **禁用能力是必要的**：运营者需要能关掉它。此前没有出口，只能改代码。
- **拒绝必须与失败可区分**：这是本次最实质的修正。此前一个被熔断拒绝的请求与一次真实上游失败
  在指标上完全一样，所以「健康上游看起来挂了」无法被诊断出来。

## 影响

- **正面**：默认不再把瞬时抖动放大成数十秒故障；运营者可调可关；拒绝与失败可分别观测。
- **代价（实测，需知晓）**：**这并没有让 100 连接变得可用**。同一条件下重测：
  成功率 62.6% → **66.3%**，P99 21.68ms → **17.42ms**；connect 失败 1370 次、`OPEN` 790 次。
  原因有二：
  1. 本代理在 100 并发下的**自身吞吐上限**约为 36k req/s（同一上游直连为 102k req/s，成功率 100%），
     所以 100 连接下失败与延迟中相当一部分是容量问题，不是熔断问题；
  2. connect 失败呈**成簇**出现，而熔断器按「连续失败」计数，因此仍然会开。
  结论：**`CONNECTIONS=25` 的 baseline 设置保持不变**，但理由已从「代理回 `Connection: close`」
  （该归因已被证伪，见 §24.10）改为「代理在该并发下的容量上限 + 熔断放大」。
- 这也说明：把「压测不可用」单归因于熔断器是不完整的。本轮修正缩小了放大，但**没有**解除容量上限。

## 相关证据

- 决策类型：`relay-core-api/src/policy.rs`（`CircuitBreakerPolicy`）
- 实现：`relay-core-lib/src/proxy/circuit_breaker.rs`（`from_policy`、`disabled`）
- 计数点：`relay-core-lib/src/proxy/http.rs`（拒绝处，而非开启处）
- 导出：`relay-core-runtime/src/lib.rs`（`relay_core_proxy_circuit_rejected_total`）
- 测试：`intermittent_failures_never_open_the_circuit`、`policy_governs_threshold_and_backoff`、
  `a_zero_threshold_disables_the_breaker`
- 容量对照实测：上游直连 100 连接 = 100% 成功 / 102k req/s；经代理 100 连接 = 66.3% / 36.4k req/s
