# 0012 · 按目标决定 TLS 处理，并记录真实的握手事实

- **Status**: Accepted
- **Date**: 2026-10-28
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§8、§24.7）、
  [`../engine-capability-status.md`](../engine-capability-status.md)（§2.2b）、
  [`../decisions/0010-connect-tunnels-recorded-not-listed.md`](./0010-connect-tunnels-recorded-not-listed.md)

## 背景

路线图 §8 的两条验收标准长期没有落地：

1. **「pinning/不兼容应用可以按策略 passthrough，而不是只能失败」** —— 此前每个 CONNECT 都无条件 MITM
   （`proxy/http.rs` → `tunnel.rs`），客户端一旦校验证书就只能失败，且失败原因在捕获里看不出来；
2. **「SNI/ALPN 在 Flow/Connection 中可用」** —— `NetworkInfo.sni`、`NetworkInfo.tls_version` 只有 `None`
   赋值点，`alpn` 字段根本不存在。也就是说，`NetworkInfo` 的文档描述比实现多两个事实。

两件事指向同一个缺口：**引擎在 TLS 上做的是一个全局决定，而 TLS 的事实是逐连接的。**
把决定做成全局开关，既无法让单个 pinning 客户端工作，也无法解释「为什么这个流没有明文」。

## 决策

### 1. TLS 处理是策略，按 CONNECT authority 匹配

`ProxyPolicy.tls: TlsPolicy`，两个列表：`passthrough_hosts`、`drop_hosts`。

- 匹配对象是 **CONNECT authority**，不是 ClientHello SNI。理由：authority 在升级前就已知，
  `drop` 因此能直接以状态码拒绝，`passthrough` 也不必解析一个它承诺不解释的握手。
- 条目支持 `host`、`host:port`、`[ipv6]:port`、`*.example.com`、IP 字面量、`cidr:`；
  裸 host 匹配任意端口；**优先级 `drop` > `passthrough` > `mitm`**；空列表 = 全量 MITM（保持历史行为）。
- 匹配器在 `relay-core-lib/src/proxy/tls.rs`，非法条目 **不匹配任何目标** 并记 warning，
  绝不「退化成匹配全部」——那会把一个配置笔误变成全局关闭检查。

### 2. passthrough 是一条真正的端到端隧道

- `handle_passthrough_tunnel` 只做 `copy_bidirectional`：不解析、不记录、不改写。
- 隧道经 **connector** 建立（新增 `OutboundConnector::open_tunnel`），因此配置了上游代理时，
  passthrough 也走父代理的 CONNECT，而不是绕过它直连——否则会得到一个「只有这条路径不受网络策略约束」
  的静默例外。直连、HTTP 父代理、HTTPS 父代理三种连接器都实现了该方法。
- CONNECT 流本身仍被记录，并打 `tls-passthrough` 标签、计入 `relay_core_proxy_tls_passthrough_total`；
  **隧道内没有 L7 流**，这是设计结果而不是遗漏。

### 3. `drop` 在 CONNECT 阶段拒绝

- 返回 `403`，CONNECT 流打 `tls-blocked` 标签、`close_reason = PolicyDrop`、写入 `end_time`，
  并计入 `relay_core_proxy_tls_dropped_total`。上游从未被连接。

### 4. 握手的三个事实来自握手本身

- `serve_tls_tunnel` 在 `TlsAcceptor::accept` 之后读 `ServerConnection`：
  `server_name()`、`alpn_protocol()`、`protocol_version()`，作为 `TlsHandshake` 传进隧道内的每个请求，
  写进 `NetworkInfo.sni` / `alpn` / `tls_version`。
- 新的 `NetworkInfo.alpn` 字段是**可加字段**（`#[serde(default, skip_serializing_if)]`），
  旧捕获反序列化为 `None`，新捕获在没有 TLS 时不出现该键。
- `tls_version` 用线上写法（`TLSv1.3` / `TLSv1.2`），不暴露 rustls 的 `Debug` 拼写（`TLSv1_3`）。

## 被排除的替代方案

| 方案 | 为什么不做 |
|---|---|
| **解析 ClientHello 得到 SNI 再决定** | 决定必须发生在升级前才可能 `drop`；而且 passthrough 的语义是「不解释这条隧道」，先解析再转发自相矛盾。代价是 authority 与 SNI 不一致时按 authority 匹配（客户端本来就是这么要求代理的）。 |
| **全局 passthrough 开关** | 等于关掉 MITM，不是「让某一个客户端工作」；也无法混合使用。 |
| **passthrough 直连、忽略上游代理配置** | 制造一条绕过操作者网络策略的路径，且从外部只能表现为「有的目标能通、有的不能」。 |
| **只在日志里记录 SNI/ALPN** | API/MCP/UI 读的是 Flow；日志不是可查询的事实来源，等于没记录。 |
| **把 CONNECT 流也填上 SNI** | MITM 路径上可以做到（握手后更新快照），但 CONNECT 记账的是「客户端要求打开哪条隧道」，握手事实属于隧道内的交换。混在一起会让「这个 CONNECT 有没有被解密」更难回答。passthrough 路径不解析握手，更不应伪造。 |

## 影响与边界

- **正面**：pinning 客户端现在有可用路径；「为什么这个流没有明文」有 `tls-passthrough` 标签与指标可答；
  SNI/ALPN 成为可查询字段，而不是文档里的承诺。
- **代价 / 仍未覆盖（必须一起读）**：
  - **没有按 client 的决策**：同一目标对所有客户端一致。按客户端证书/进程决定属于 §8 的演进项。
  - **passthrough 的 SNI 不记录**：不解析握手是这一模式的定义；CONNECT 流只记录 authority。
  - **upstream 证书验证策略**（native roots / custom CA / explicit insecure）仍未实现；
    本决策只决定「是否终止」，不决定「如何验证上游」。
  - **原始上游证书链与握手错误原因**仍未记录（§8 第 7 条）。
  - **`connect_time_ms` / `ssl_time_ms` 仍为 `None`**：HAR timing 依旧只能填 0。
  - 匹配在每次 CONNECT 时编译 glob（列表短，且不是每请求路径）。

## 相关证据

- 决策与匹配器：`relay-core-lib/src/proxy/tls.rs`（10 个单测：authority 解析、端口、glob、CIDR、优先级、坏条目）。
- 隧道：`relay-core-lib/src/proxy/tunnel.rs`（`handle_passthrough_tunnel`、`TlsHandshake` 提取）。
- 连接器：`relay-core-lib/src/proxy/outbound.rs`（`open_tunnel`，三种实现）。
- 线路契约：`relay-core-lib/tests/tls_policy_test.rs`（3 个用例）：
  - `tls_passthrough_shows_the_client_the_upstream_certificate` —— 客户端看到的叶子证书**逐字节等于**上游证书；
    已双向验证：把判定改成永不 passthrough，该断言以「看到 RelayCraft CA 签发的证书」失败。
  - `tls_is_intercepted_by_default` —— 默认策略下客户端看到的不是上游证书，且 SNI/ALPN/版本被写进 Flow。
  - `tls_drop_hosts_refuse_the_connect_and_record_why` —— 403、上游 0 次连接、流带 `tls-blocked` 与 `PolicyDrop`。
