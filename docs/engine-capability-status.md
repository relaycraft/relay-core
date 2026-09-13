# RelayCore 能力现状盘点（Engine Capability Status）

- **Status**: Snapshot / 一次性盘点，非路线图
- **Date**: 2026-09-13
- **Commit**: `bb10799` 起、`88581e7` 后（工作树含本次修正）
- **方法**: 6 个独立只读审计 + 1 次实测；每条结论均以 `file:line` 或可复现命令为依据
- **与路线图的关系**: [`l7-first-engine-evolution-roadmap.md`](./l7-first-engine-evolution-roadmap.md) 仍是**规划与验收的唯一权威**。
  本文只回答「**现状是什么**」，并为下一步排序提供证据。凡两者冲突，以代码与实测为准，并应修正路线图。

> **这份文档存在的理由**：路线图 §22 的 DoD 第 8 条是「文档没有超出真实实现进行宣传」。
> 本次盘点表明**这一条本身还未满足**——失真出现在两个方向：既有**夸大**（把已有结构性缺陷说成已具备），
> 也有**过期低估**（把已经修好的问题仍列为未修）。两者都会误导排期。

---

## 1. 总览

成熟度口径：**Stable**＝有线路级 E2E 且契约被测试锁定；**Working**＝功能真实可用但覆盖不全；
**Partial**＝部分生效或仅单宿主生效；**Skeleton**＝只有类型/字段/入口，线路上不成立；**Absent**＝没有。

| 能力面 | 成熟度 | 一句话现状 | 最大缺口 |
|---|---|---|---|
| §3 数据面与 Mutation Pipeline | **Partial** | 头/状态/body/编码的改写都真的到线路了，且失败与超预算有结构化原因 | 没有「显式 Mutation 类型」；`BodyObservation` 策略不是真正决定保留的开关 |
| §4 Connection/Stream/Message 模型 | **Skeleton** | `Layer` 枚举可扩展，但**关系字段全缺** | `connection_id`/`parent_flow_id`/`stream_id`/`protocol_stack` 仓库内 0 命中 ⇒ CONNECT 与外层、H2 stream 归组不可能 |
| §5 HTTP/1.x 与 Body 编解码 | **Working** | H1.0/H1.1、chunked、gzip/deflate/br/zstd 解压→规则→重编码闭环 | 请求方向 Content-Encoding 不解码（有意，已锁测试）；trailer 无线路断言；half-close 无处理 |
| §6 HTTP/2 | **Skeleton** | 仅「CONNECT-MITM 之后的 H2 客户端 → H1 上游」可用 | **没有任何 H2 线路测试**；无 RST_STREAM/GOAWAY/取消；`wire_matrix` 全是 H1 |
| §7.1 WebSocket | **Working** | 握手改写、帧替换、会话结束/失败握手现在都可观测 | `permessage-deflate` **既不支持也不剥离也不降级**（静默破坏）；帧级修改无线路断言 |
| §7.2 SSE（作为被代理流量） | **Absent** | 只把 SSE 当作**本引擎自己的 API 传输** | 引擎不识 `text/event-stream`，无解析/缓冲/事件级推送 |
| §8 TLS / PKI | **Partial** | CA 生成/加载/按主机签发、MITM 端到端可用，CA 错误处理严格 | **无「按 host 决定 MITM/passthrough」策略** ⇒ 做 pinning 的客户端只能失败；SNI/ALPN 永不记录；叶证书对 IP 字面量发的是 DNS SAN |
| §9 代理模式与捕获 | **Partial** | 正向代理默认稳定；透明模式 Linux/macOS/Windows 都有代码 | 透明捕获**零平台级测试**（三个 provider 文件 0 个测试）；reverse/多监听被生命周期**主动拒绝** |
| §10 通用 TCP/UDP/DNS | **Skeleton** | `CaptureSource` 泛型正确；UDP 可转发并上报会话 | 无协议探测；无通用 Message；UDP 绕过 `CaptureSource` |
| §11 Rules/Scripts/Manual Intercept | **Partial** | 规则加载期拒绝非法模式、失败/跳过有原因、intercept 暂停有事件 | **加载期不校验「该动作是否真有线路实现」**；`RuleTrace` 无生产者 |
| §12 Fault Injection | **Partial** | delay/throttle/rate-limit/drop/mock/redirect 等有实现 | 绝大多数**无线路断言**；rate-limit 表现为**断连而非 429**；Abort 实际等同 Drop |
| §13 Recording / HAR / Replay | **Partial** | SQLite 迁移、LRU、HAR 不假造 timing、replay 可用 | replay **绕过代理**（不捕获、不跑规则）；HAR 不写 `content.encoding` ⇒ base64 以文本导出 |
| §14 Content Codec / Inspector | **Partial** | `Content-Encoding` 四编码闭环；失败不影响透传 | 无 codec trait/registry；无 JSON/XML/multipart；无 hex/raw 表示 |
| §15 Observability / Reliability / Perf | **Partial** | harness 可信化与 baseline 真实 | **18 个指标恒为 0 或未导出**；soak/fuzz/differential 均未接 CI |
| §16 Security / Privacy | **Partial** | 落盘前脱敏、MapLocal sandbox、env/fetch 白名单、API 默认 loopback | 脱敏**默认关闭**（`enabled:false`/`redact_bodies:false`）；**CA 私钥以 0644 创建**；无依赖漏洞门禁 |
| §17 Adapters 对齐 | **Partial** | 窄 service trait 存在，Tauri 特有字段留在 adapter | CLI **自带第二套控制面**；MCP 与 HTTP 各自重复实现 replay/HAR |
| §18 MCP / AI-Native | **Partial** | 15 个工具 + 5 个资源，类型化输出 | 资源**不可订阅**却在推送通知；写工具**无幂等**（每次新 UUID） |
| §19/§20 阶段计划 | **Partial** | Phase A 的数据面关口基本走完 | Phase A 的「关系字段/typed event 基础」未完成；§20「设计」桶里 DNS/SOCKS5/WireGuard/gRPC **无任何设计产物** |
| **发布 / 分发 / Web UI**（路线图未治理） | **Working** | crates.io + npm（`@relay-core/cli`、`@relay-core/mcp` + 7 平台二进制包）+ 内嵌 Web UI | **不在任何规划文档范围内**；webui 未消费 `flow-event` |

---

## 2. 五类横切问题（比任何单个功能更重要）

### 2.1 「声明了，但没有生产者」族

同一类缺陷在四个层面重复出现。它们的共同后果是：**外部无法区分「没有发生」与「没有实现」**。

| 层面 | 声明但无生产者的项 | 证据 |
|---|---|---|
| 事件 | `FlowEvent::Started` / `HeadersReceived` / `BodyChunk` / `MessageReceived`（9 个变体中 4 个） | 仅出现在 `#[cfg(test)]`（`relay-core-api/src/sse.rs` 测试模块） |
| 关闭原因 | `CloseReason::TlsError` | 枚举有、映射有、`relay-core-lib` 内 0 个赋值点 |
| Flow 字段 | `ResponseTiming.connect_time_ms` / `ssl_time_ms`、`NetworkInfo.sni`、`tls_version` | 生产代码只写 `None`（`proxy/server.rs:171-172` 为 TODO） |
| 指标 | 15 个 `relay_core_script_hook_*` 序列、`proxy_invalid_method_total`、`proxy_retry_total` | 有导出、无调用点 ⇒ **永远显示健康** |

反向也有一例：**代理侧真实存在约 20 个丢弃计数点**（`websocket.rs` 等），但那个计数器**从未导出**——真实信号反而看不见。

**同族的第二种形态：实现了，但没有任何宿主能调到。** 这比「没实现」更难发现，因为代码、测试、路线图 ✅ 全都齐全：

| API | 调用者 | 后果 |
|---|---|---|
| `CoreState::set_retention_policy` | **已接线**：`ProxyPolicy.retention` → `update_policy_from` | 任何能设策略的宿主（HTTP `PATCH /policy`、Tauri、MCP）现在都能给存储设界；未设界时仍不启动裁剪任务 |
| `CoreState::redact_stored_history` | **已接线**：脱敏 `false → true` 时自动执行（后台） | 打开脱敏现在会重写既有历史；只重写历史、不再重复重写（已锁测试） |
| `ScriptEngine::set_fetch_config` | **已接线**：`--script-fetch-allow` → `CoreState::set_script_fetch_allow` → 引擎 | `relay.fetch` 首次可由宿主启用；**必须在 `load_script` 之前设置**（引擎在构造时快照配置），该顺序要求已写在 setter 文档上 |
| `QuicMode::ExperimentalMitm` | **零调用者**（cfg 门控变体） | 与 §20「暂不投入」边界一致，但它是「未接线的 API 枚举」，正是该条禁止的形态 |
| `get_flows_dropped()` | **零调用者** | 真实存在的代理侧丢弃计数没有出口 |

### 2.2 策略与实际行为不一致（`BodyObservation`）

`BodyObservation::Off` 的文档说「Retain nothing」，决策 [`0002`](./decisions/0002-body-observation-policy.md) 也说引擎默认 `Off` 是为了「无消费者时零拷贝」。但
`relay-core-lib/src/proxy/http.rs` 在**两个方向都无条件安装** `TapBody`，预算取 `policy.max_body_size`（默认 10 MiB），
并把保留下来的前缀发布为 `FlowUpdate::HttpBody`。

- 影响：不是字节错误，而是**成本与承诺不符**——`Off` 并不「零拷贝」，`Full` 对**规则**生效仍需存在 body 阶段规则。
- 同一字段的两份文档互相矛盾：`relay-core-api/src/policy.rs:183-184` 承认 tap 存在，`body_plan.rs` 里 `Off` 却写「Retain nothing」。
- 附带不一致：`body_observation` 字段是 `#[serde(default)]`，会取枚举的 `#[default] = Prefixed`，而 `ProxyPolicy::default()` 显式给 `Off` ⇒ **省略该字段的 JSON 会静默把策略从 Off 变成 Prefixed**。

### 2.2b 安全面：三处「默认不设防」

均已在代码与文件系统上核实：

1. **CA 私钥以 `0644` 落盘。** `relay-core-lib/src/tls/ca.rs:247-251` 用普通 `fs::write`，工作区内**没有任何** `set_permissions`；本机实测 `ca_key.pem` 为 `-rw-r--r--`（仓库根与 `relay-core-cli/` 各一份）。
   好消息：这两个文件**未被 git 跟踪**且被 `.gitignore:80` 覆盖，因此没有泄漏进版本库；坏消息是同一台机器上的任何用户都能读走签发任意站点的能力。
2. **脱敏默认关闭。** `RedactionPolicy::default()` 为 `enabled:false` + `redact_bodies:false`（`relay-core-api/src/policy.rs:126-131`）⇒ `Authorization` / `Cookie` / body 默认**原样写入 SQLite 并原样由 API 返回**。脱敏链路本身是完整的（落盘前、输出前、快照路径都过），但默认值使 §16 的隐私条目在默认配置下不成立。
3. **依赖漏洞门禁（本次新增后暴露出一批真实漏洞）。** 此前无 `deny.toml`、无 `cargo-deny`/`cargo-audit` 步骤、无 dependabot。
   现在 CI 有 `audit` job：**licences 与 bans 为阻塞门禁**（已实测通过），advisories 先作为**追踪项**（见下）。

   实测 `cargo deny check advisories` 的存量清单（2026-09-13，`Cargo.lock`）：

   | 依赖 | 问题 | 与本项目的关系 |
   |---|---|---|
   | `rustls-webpki 0.101.7` | **3 条证书校验漏洞**：URI name constraints 被错误接受、wildcard name constraints 被接受、CRL 解析可 panic | 直击威胁模型：本引擎做上游证书校验 |
   | `h2 0.4.15` | 空 DATA 帧无界（DoS） | MITM 后的 H2 路径正在用 |
   | `quick-xml 0.39.4` | 属性名重复检查为二次复杂度；`NsReader` 命名空间分配无界 | 间接依赖 |
   | `rustls-pemfile 2.2.0` | 已停止维护 | 与证书 PEM 解析相关 |
   | `lru 0.16.4` | `LruCache::pop()` 缺 panic 安全，潜在 UAF（unsound） | 间接依赖 |
   | `atomic` | `fmt::Pointer` 实现中无效指针解引用 | 间接依赖 |
   | `spin 0.9.8` | 版本被 yank | 经 `sqlx-sqlite` → `flume` |

   **未做任何 ignore**：把其中任何一条写进 `ignore` 等于替团队做了一次安全决策，因此这些条目只报告、不被接受。
   多数属补丁级升级（`cargo update -p <crate>`）可解，但需要独立提交与验证，属待办。

另有一处能力缺口值得单列：**没有「按 host 决定 MITM / passthrough / drop」的策略**。每个 CONNECT 都无条件 MITM（`proxy/http.rs:46-96` → `tunnel.rs:41`），因此使用证书 pinning 的客户端只能失败——而 §8 的验收标准正是「可按策略 passthrough 而非只能失败」。

### 2.3 文档失真（两个方向，且已修好的一项仍被列为未修）

**已修正（本次）**：
- 路线图 §24.3 仍把 `mock_to_response` 描述为 framing 不安全、base64 以文本发出；**代码已修**（现委托 `build_response_from_flow_response`，丢弃 `content-length`/`transfer-encoding`、按 `encoding` 解码）。已改写为「✅ 已修复」，并注明此前属反向失真。
- `relay-core-lib/src/proxy/mod.rs` 声称「引擎内不存在任何 gzip/deflate/brotli/zstd 处理」，而 `content_encoding.rs` 四个都实现了——已修正。
- `benchmarks/bench_minimal.sh` 把 100 连接不可用归因于「RelayCore answers with `Connection: close`」——**该归因不成立**（见 §2.5），已改写。

**仍未修（需要在路线图里改口径）**：
- `relay-core-lib/tests/wire_matrix.rs` 的 **Action 覆盖表** 是被 §3/§22 当作唯一追踪点引用的表，但它把**已有通过用例**的动作标为 `gap — §24.1`（如 `SetRequestBody`、`SetResponseBody`、响应头），并声称 `Inspect` 是「no wire effect by design」——后者是错的：`Inspect` 会真的暂停交换至多 60s。
- 同一文件头部声称「暴露已知缺口的用例标了 `#[ignore]`」——文件里**没有任何** `#[ignore]`。
- 路线图 §24.1 声称 `SetResponseStatus` 有线路断言，实际只有未被 case 选中的脚手架。
- 路线图 §24.8 把「历史数据可追溯脱敏」标为 ✅，但该入口只有测试调用（见 §2.1）——能力存在、**不可达**。
- 路线图 §24.10 的「493 tests / 22 E2E」已过期（当前 workspace 计数为 836 个测试属性，`wire_matrix` 26 个用例）；同段「不存在断言上游/客户端实际收到字节的测试」**已不成立**。

### 2.4 验证体系：绿灯，但有一部分是空转

| 项 | 现状 | 证据 |
|---|---|---|
| mitmproxy 差分（3 个 fixture） | **在 CI 里静默跳过** | 代码在 `mitmdump` 缺失时打印 skip 后**返回成功**（`mitmproxy_differential.rs:87-92`）；`.github/workflows/` 从不设置 `REQUIRE_MITMPROXY`，也从不安装 mitmproxy ⇒ ubuntu 上 3 个测试记为 **ok**。本机装了 mitmdump，因此本地 3/3 真实通过 |
| soak（`stability_test.sh`，默认 2h） | 未接任何 workflow，且仍指向已饱和的 Python 上游 | grep `.github/workflows` 无命中 |
| fuzz | 无 `fuzz/` 目录、无 `cargo-fuzz`/`arbitrary`；只有 6+5 个确定性对抗用例 | `content_encoding.rs`、`rule/engine/loader.rs` |
| HTTP/2 | **零线路用例** | `wire_matrix.rs` 全为 H1 socket；唯一 H2 测试是单请求且断言与版本无关 |
| 透明捕获 | **零平台级覆盖** | `linux_tproxy.rs` / `macos_pf.rs` / `windows.rs` 各 0 个测试；测试全用 mock |
| UDP E2E | 唯一两个用例是 `#[cfg(not(target_os = "linux"))]` ⇒ **在唯一阻塞门禁平台上被跳过** | `udp_integration_test.rs:45,101` |
| 覆盖率门禁 | 有 job、无阈值 | `ci.yml` 使用 `--summary-only` |
| 依赖审计 | 无 `cargo-audit`/`cargo-deny` 步骤 | grep workflows 无命中 |
| 性能门禁 | `benchmark-gate.yml` 只报告、不阻断 | 同文件 |
| CLI / HTTP 断言密度 | 明显低于 lib/runtime | 测试属性数：`relay-core-http` 17、`relay-core-cli` 88（其中多数是参数解析） |

> 结论：**「CI 绿」目前不能被读作「这些能力都被验证过」**。差分、soak、H2、透明捕获、UDP 五项在 CI 上没有任何有效信号。

### 2.5 性能基线的根因被误判（实测证据）

**可复现命令**（本机，release 二进制）：
```bash
CARGO_TARGET_DIR="$PWD/target-dsh" cargo build --release -p relay-core-cli
CARGO_TARGET_DIR="$PWD/target-dsh" RELAY_CORE_BIN="$PWD/target-dsh/release/relay-core-cli" \
  CONNECTIONS=100 ./benchmarks/bench_minimal.sh quick --duration 10
```

实测结果：**34,532 req/s / P99 21.68ms / 成功率 62.6%**，同时代理日志里
**439 次上游 connect 失败**与 **383 次 `Circuit breaker OPEN`**。

两件事因此可以确定：

1. **「RelayCore 用 `Connection: close` 回应」是错的。** 新增线路用例
   `wire_matrix_client_connection_is_reused_for_a_second_request` 在同一客户端连接上连续发两个请求并都成功（上游未声明 close 时）。
   代理是**忠实转发上游的关闭语义**；当年的症状来自当时默认的 Python 上游（`echo_server.py` 未设
   `protocol_version`，即 HTTP/1.0，每次响应后就关闭）。
2. **100 连接下真正压垮成功率的是熔断器放大**：439 次 connect 失败（≈0.08% 请求）触发 383 次熔断，
   每次对**该 host** 拒绝 30s（`proxy/circuit_breaker.rs:90`，`3 次失败 → 30s`，**不可配置**），
   于是 0.08% 的上游瞬时抖动变成 37% 的失败率。harness 的 INVALID 检测只识别 `os error 49`，
   因此**这种情况会被判为 FAIL 并记在代理账上**。

需要决策的三件事（本文不代为决定）：
- 熔断器默认（3 次 / 30s / 按 host / 不可配置）是否应改为更保守的放大倍率，或改为可配置/默认关闭；
- `CONNECTIONS` 默认值是否重新评估（结论「25」可能依然正确，但**理由必须更换**）；
- baseline 是否在更高并发下重采（当前 `connections: 25`）。

---

## 3. 分布层面：路线图未治理的部分

以下实体存在且已发布，但**不属于 §3–§18 任何一条**：

- `npm/cli`、`npm/mcp`（`@relay-core/cli`、`@relay-core/mcp`）与 `npm/packages/binaries-{darwin,linux,win32}-*` 共 7 个平台二进制包；
- 内嵌 Web UI（`webui/`，随 `relay-core-http` 的 `webui` feature 打包）；
- `publish.yml` / `publish-npm.yml` / `release.yml` / `RELEASE.md` / `scripts/release-preflight.sh` / `scripts/sync-npm-version.py`。

具体缺口：
- **Web UI 不消费 `flow-event`**：`webui/src/lib/sse.ts` 监听 8 个 SSE 事件，但不含本轮新加的 `flow-event` ⇒ 桌面/网页 UI 看不到暂停与改动归因。
- **CLI 有两套控制面**：`src/server.rs` 的旧 WebSocket/`/api/intercept/*`（自带 `AtomicBool` 状态）与共享 HTTP adapter 并存；`rules list` 默认打到无人启动的 18082，`metrics` 调用无人提供的 `/_relay/metrics` ⇒ 这两个命令在当前引擎上**必然失败**。
- `relay-core-api` 的 crate 文档让用户「依赖 `relay-core`」，但**工作区不存在该 crate**；真正的门面是 `relay-core-runtime`，而它只再导出 `flow` 与 `policy` ⇒ 外部消费者拿不到 `event` / `sse` / `body_plan`。

---

## 4. 建议的推进顺序（跨领域，按「修复已宣称能力的错误语义」优先）

依据 §21 的决策准则排序，**不按模块**：

1. **止血三类「声明但无效」**（§21-1，最高优先）：
   `BodyObservation` 真正成为保留的开关（或改口径、删承诺）；从 Prometheus 面移除或补上那 18 个恒零指标；
   `CloseReason::TlsError` 与 `connect_time_ms`/`ssl_time_ms`/`sni` 二选一：**要么补生产者，要么删字段**。
2. **修文档失真**（§22-8，成本极低、收益即时）：重写 `wire_matrix` 的 Action 覆盖表与顶部说明；
   修正 §24.1 的 `SetResponseStatus` 声明；把 §24.10 的计数与「无字节断言」更新为实测值。
2b. **安全面三件事（成本极低，风险最高）**：CA 私钥改 `0600` 并加断言测试；
   决定脱敏默认值（打开 headers/query，body 仍可选）或**明确写清默认关闭**；
   接入 `cargo-audit`/`cargo-deny` + dependabot。
   同时把三个「只有测试能调到」的入口接上宿主（retention / 历史脱敏 / script fetch 白名单），或从路线图降级为「未接线」。
3. **让验证体系真的会失败**（§22-2）：CI 安装 mitmproxy 并设 `REQUIRE_MITMPROXY=1`；
   至少 1 个 H2 线路用例；把 UDP E2E 的 `cfg(not(linux))` 移除或说明为何只在 macOS 跑；
   给覆盖率设阈值；接 `cargo-audit`；决定 soak/fuzz 是接 CI 还是从路线图降级为「不承诺」。
4. **关系字段**（§4-1/§19-A）：加 `connection_id`/`parent_flow_id`/`stream_id`（serde 可加，已有兼容性测试证明安全）。
   这是 H2 stream 归组、CONNECT 父子关联、连接级统计（`ConnectionStats.flows_count` 目前硬编码 `0`）的共同前提。
5. **WebSocket `permessage-deflate`**：当前是**静默破坏**（握手头透传但隧道不协商扩展）。
   最小正确解是显式降级：剥离并记录，而不是继续假装支持。
6. **熔断器放大倍率**（§2.5 的决策项）：它同时影响可靠性语义与性能 DoD 的可解释性。
7. **分发与适配器一致性**（§17/§18）：Web UI 接 `flow-event`；MCP 资源改为可订阅或停止推送；
   写工具加幂等键；CLI 的两套控制面收敛为一套，并修掉两个必然失败的命令。
8. **typed event 补齐**：`Started`/`HeadersReceived`/`BodyChunk`/`MessageReceived` 的生产者，
   与 §4-4 的完成度直接绑定（当前 9 个变体中 4 个无生产者，不得宣称完成）。

**明确不投入（建议在路线图里保持）**：DNS / SOCKS5 / TUN / WireGuard / DTLS / HTTP-3 MITM / gRPC-Protobuf 可编辑视图 / OpenTelemetry 全链路 / 分布式压测。
其中 reverse 与多监听建议**从 M1 移到 1.x**——它们不是「未实现」，而是被 `lifecycle.rs` **主动拒绝**，需要生命周期重设计。

---

## 5. 方法与已知不确定项

**方法**：6 个互不重叠的只读审计（全部已返回）（§3/§9/§10/§22；§4/§5/§6/§7/§14；§8/§16；§11/§12/§24.5/§24.6；§13/§15/§24.7/§24.10；§17/§18/§1.3/§19/§20），
每条结论要求给出 `file:line`；加上本文作者对若干高价值结论的独立复核与 1 次实测（§2.5）。
审计**未运行 cargo**，因此「由测试锁定」的说法来自阅读测试源码；测试总数与绿/红状态来自本文作者实跑。

**本文作者独立复核并纠正了审计结论 2 处**：
- 「`mutated_fields` 响应头映射疑似错位」——**不成立**，映射正确（`relay-core-lib/src/rule/engine/actions/mod.rs`）。
- 「§24.3 mock framing 仍不安全」——**不成立**，代码已修（见 §2.3）。

**已知不确定项（不应被当作结论）**：
1. 透明捕获（Linux TPROXY / macOS PF / WinDivert）是否**真的工作**无从判断——0 测试且需要 root；本文只能确认「无覆盖」。
2. `Layer::Tcp`/`Udp` 的 Flow 能否原样通过 SQLite 往返，无测试；只能确认不 panic。
3. `scripts/ci-check.sh` 的 webui 步骤在本沙箱内因 `~/.npm` 不可写而失败（环境限制，非代码问题）；Rust 三段（fmt/clippy/test）为本文实跑通过。
4. §20「只做设计」桶在仓库内**没有任何设计产物**，因此「已设计」与「未开始」无法从代码区分。

**可复现命令**：
```bash
CARGO_TARGET_DIR="$PWD/target-dsh" cargo test --workspace        # 661 passed / 0 failed
CARGO_TARGET_DIR="$PWD/target-dsh" cargo clippy --workspace --tests -- -D warnings
CARGO_TARGET_DIR="$PWD/target-dsh" cargo test -p relay-core-lib --test wire_matrix   # 26/26
REQUIRE_MITMPROXY=1 CARGO_TARGET_DIR="$PWD/target-dsh" \
  cargo test -p relay-core-lib --test mitmproxy_differential      # 3/3（本机有 mitmdump）
```
