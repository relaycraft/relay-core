# 0013 · 出站证书验证是策略，不是常量

- **Status**: Accepted
- **Date**: 2026-10-30
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§8.5、§8 验收）、
  [`../engine-capability-status.md`](../engine-capability-status.md)（§8）、
  [`0012-tls-passthrough-and-handshake-metadata.md`](./0012-tls-passthrough-and-handshake-metadata.md)

## 背景

在此以前，出站 TLS 只有一种行为：`start_proxy` 用 `with_native_roots()` 建一个 client，所有目标都用它验证。
这带来两个相反的问题：

1. **内部 CA 无处可去。** 使用私有 PKI 的部署只能看到校验失败，且没有配置入口；
2. **没有「明确不安全」的合法出口。** 需要临时连接自签名上游时，唯一做法是改代码或改用别的手段，
   于是「不安全」既不可见也不可审计——这比提供一个带理由的显式开关更危险。

路线图 §8.5 因此长期是 ⬜；§8 验收的第三条「insecure 模式不会静默开启」在当时只约束了一个**不存在**的开关。

## 决策

### 1. `ProxyPolicy.upstream_tls.verification` 三选一

```json
{ "verification": { "mode": "native_roots" } }
{ "verification": { "mode": "custom_ca", "path": "/etc/relaycore/internal.pem" } }
{ "verification": { "mode": "insecure", "reason": "lab fixture, self-signed upstream" } }
```

- 默认 `native_roots`；该字段**总是序列化**（与 `tls` 的空值省略不同）：读 `GET /api/v1/policy` 的人
  必须能直接确认验证没有被关掉，而不是从「字段不存在」去推断。
- `custom_ca` **替换** native roots，而不是叠加。装内部 CA 的部署通常只应信任那套链；叠加会让一条
  本不完整的链因为某个公共根恰好存在而通过。
- `insecure` **必须带非空 reason**；每次构建 client 都 `warn` 一次。没有任何路径会在验证失败后
  自动重试为 insecure——那会把一次校验失败变成静默降级。

### 2. 策略先验证，再入库

`update_policy_from` 在任何状态变更之前调用 `upstream_tls::validate`：

- 不可读 / 无证书 / 非法 PEM 的 bundle → 拒绝整次更新，**保留旧策略**并记 `error`；
- 空 reason 的 insecure → 同样拒绝；
- 拒绝会写一条 `PolicyUpdated` / `AuditOutcome::Failed` 审计事件，包含原因——「谁试图改信任、为什么被拒」
  不只存在于日志行里。

理由：如果坏 bundle 被存进策略，API 会显示 `custom_ca`，而运行中的 client 仍是上一个（可能正是
`insecure`）——「报告的安全姿态」与「实际信任集合」分离，是安全设置最不该有的失真。
拒绝之后策略与行为重新一致。

### 3. 变更即时生效

policy watcher 现在同时看 `upstream` 与 `upstream_tls`。TLS 策略变化会重建 client（hyper 的连接池
属于 client，复用旧池等于继续用旧的信任设置）与 connector，之后新连接使用新策略。
这也意味着从 `insecure` 收紧到 `custom_ca` 时，旧池子不会漏过一两个连接。

### 4. 范围：只约束本引擎主动发起的**目标** TLS

- 不约束浏览器→代理的 MITM 证书（那是 §8 的叶证书策略）；
- **同样约束 HTTPS 父代理**：父代理与目标是同一个出站信任决策，否则「信任目标但不信任你让我用的代理」
  会让内部 HTTPS 代理无法使用。`HttpUpstreamConnector` 里「通过父代理访问 HTTPS 目标」的那段 TLS
  也使用同一份配置，不再有第三条硬编码 native roots 的路径；
- 不改变 passthrough 隧道：客户端自己验证上游，本引擎不验证也不介入。

## 被排除的替代方案

| 方案 | 为什么不做 |
|---|---|
| 环境变量 `SSL_CERT_FILE` / rustls 默认根 | 不可从 API/UI 看见，也无法审计「谁改了信任」；且与 policy 的其余部分分裂。 |
| 自定义 CA **追加**到 native roots | 看似方便，实际会掩盖缺失的中间证书；且让「只信任内部 CA」无法表达。 |
| 校验失败后自动以 insecure 重试 | 静默降级，正是 §8 验收禁止的行为。 |
| 每请求一个 insecure 标记 | 把信任决策散落到流量里，无法审计，也无法回答「现在信任什么」。 |
| 只提供全局开关（禁用验证） | 无法让一个内部上游工作而保持其余验证；等价于关掉安全面。 |

## 影响与边界

- **正面**：内部 CA 部署可用；insecure 变成带理由、会被记录、从 API 可见的显式状态；
  §8.5 从 ⬜ 变为已交付；策略无法在不被察觉的情况下与运行行为分离。
- **代价 / 仍未覆盖（必须一起读）**：
  - **Web UI 现在会在 Settings → TLS 显示 `DISABLED` 与理由**，但这是类型检查 + vitest 级别的验证，
    没有浏览器截图靶场；`custom_ca` 只显示路径，不校验文件当前是否仍可读。
  - `custom_ca` 是**替换**信任库，因此 HTTPS 父代理与目标必须同时在该 bundle 里；只装其中一个会被拒绝。
  - 验证与 watcher 构建之间理论上存在竞态：策略被接受后 bundle 被删除，watcher 会记 error 并保留旧
    client。窗口很小，但「旧 client 可能更宽松」这件事仍成立；彻底的修法是失败时换成一个拒绝一切 TLS
    的 client，目前未做。
  - 原始上游证书链、握手错误原因、`connect_time_ms`/`ssl_time_ms` 仍不可观察（§8 第 7、8 条）。

## 相关证据

- 策略与序列化：`relay-core-api/src/policy.rs`（`UpstreamTlsPolicy`、`UpstreamTlsVerification`；
  2 个单测：默认值/可见性、patch 替换与未知 mode 拒绝）。
- 构建与校验：`relay-core-lib/src/proxy/upstream_tls.rs`（5 个单测：native 构建、空 reason、缺文件、
  空 bundle、有效 bundle）。
- 接线：`relay-core-lib/src/proxy/server.rs`（启动构建、watcher 重建 client+connector）、
  `relay-core-runtime/src/lib.rs`（入库前拒绝：`an_unreadable_ca_bundle_is_rejected_instead_of_stored`）。
- 线路契约：`relay-core-lib/tests/upstream_tls_policy_test.rs`（6 个用例）：
  默认拒绝自签；custom CA 接受；显式 insecure 接受；**不含目标链的 custom CA 不回落**；
  运行中更新策略无需重启即生效；**HTTPS 父代理与目标使用同一信任策略**（insecure 下经父代理访问
  自签目标成功，默认策略下父代理自签证书被拒绝）。
- CLI：`--upstream-ca` / `--upstream-insecure <reason>`（互斥；空 reason 与不可读文件在启动前拒绝），
  2 个 daemon 单测。
