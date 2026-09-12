# RelayCore L7-First Engine Evolution Roadmap

> RelayCore 七层优先、四层可演进的长期能力路线图

- **Status**: Active / 长期对齐文档
- **Created**: 2026-08-25
- **Updated**: 2026-08-25
- **Scope**: RelayCore engine / runtime / API 的底层能力与长期演进
- **Positioning**: L7-first, L4-ready；以 HTTP 生态为主，保留向通用传输代理扩展的架构空间
- **Related**:
  - [`proxelar-rama-learning-directions.md`](./proxelar-rama-learning-directions.md)：外部参考项目审计与借鉴优先级
  - `.ai/archived/relaycraft-integration/056-relaycraft-mitmproxy-migration-checklist.md`：RelayCraft 接入与契约迁移清单（已归档，仅作背景）
  - `.ai/archived/2026-h2-roadmap/`：历史里程碑与 1.0 规划（已归档，仅作背景）
  - `AGENTS.md`：工程纪律、TDD、Offline-First 与发布要求

---

> ## 阅读须知 / Reading Notice
>
> **本文档是 RelayCore engine 数据面与协议能力演进的对齐文档。**
> 自本文档生效起，`.ai/archived/**` 下的全部历史路线图（含 `full-roadmap-2026.md`、`BACKLOG.md`、
> `known-bugs.md`、`competitive-positioning.md` 及各期计划文档）**不再作为推进目标**，仅作历史背景保留。
> 凡历史文档与本文档冲突的，以本文档为准。
>
> 历史文档中「M6/M7 已完成」等完成度口径与代码现状不一致（例如 `benchmarks/results/` 仅有 v0.8.2 / v0.8.3
> 基线，当前版本为 0.10.0），**完成度一律以代码、测试与 CI 的实测结果为准**，不以历史文档的进度表为准。
>
> **已知失真项 / Known gaps** — 下列能力当前在代码中存在但与公开描述不符，在修复前**不得标记 Stable**：
> 见 §24。
>
> This document is the alignment source for RelayCore's engine data plane and protocol evolution.
> All historical roadmaps under `.ai/archived/**` are background only; where they disagree, this
> document wins. Completion status is determined by measured code/test/CI reality, not by the
> historical progress trackers.
>
> **Language**: 本文档当前为中文单语正文（与 `AGENTS.md` 的双语要求存在差距），英文版待补。

---

## 0. 文档目的

RelayCore 的近期目标不是在数月内复刻 mitmproxy 的全部协议、模式和生态，也不是立刻演化为覆盖完整网络链路的通用抓包平台。

本路线图用于回答三个问题：

1. **哪些能力现在必须处理**，否则会形成错误的数据面语义、失真的公开能力或未来难以兼容的架构债务？
2. **哪些能力属于 L7 代理引擎的必要基线**，应在未来数月分阶段补齐？
3. **哪些能力应保留架构入口但长期演进**，不能转化成当前版本的交付承诺？

核心原则：

> 近期优先保证已公开 L7 能力在线路层真实、稳定、可组合；长期逐步拓展协议理解、捕获模式和 L4 数据面，而不是一次性追平所有竞品功能。

---

## 1. 定位与范围边界

### 1.1 当前定位

RelayCore 是一个以七层 HTTP 生态为主要工作面、兼顾未来四层扩展能力的可嵌入代理引擎：

```text
Primary / 主工作面
  HTTP/1.x → HTTPS MITM → HTTP/2 → WebSocket → SSE
       ↓           ↓          ↓          ↓
  Rules / Scripts / Intercepts / Replay / Recording

Evolution / 可演进工作面
  TLS sniffing → Generic TCP/TLS → UDP sessions → DNS → QUIC metadata
       ↓
  Reverse / SOCKS / Local Capture / TUN / WireGuard 等捕获入口
```

### 1.2 近期不追求的范围

未来几个月不以以下目标作为交付承诺：

- 完整 HTTP/3 应用层 MITM
- 完整 DTLS MITM
- 通用内核级抓包或 Wireshark 替代
- 覆盖任意 L4/L7 协议的自动解析
- mitmproxy 全部 addon hooks、commands、content views 的逐项复刻
- 同时交付 SOCKS、TUN、WireGuard、Local Capture、DNS 等所有模式
- 因 RelayCraft 当前 UI/API 形状而污染 engine 内部模型

### 1.3 RelayCraft 的角色

RelayCraft 是 RelayCore 的重要官方宿主，但不是底层路线图的唯一目标函数：

- 作为真实需求来源，验证能力是否有产品价值
- 作为回归宿主，验证 adapter 和契约稳定性
- 作为架构约束，确保 core 不依赖具体 UI
- 不以短期迁移需求替代 engine 自身的数据面设计

建议长期投入比例：

| 投入方向 | 建议比例 |
|---|---:|
| Core 数据面、L7 协议与可编程处理 | 55%–65% |
| 正确性、测试、性能、稳定性、安全 | 20%–30% |
| RelayCraft 与其他 adapters 持续对齐 | 10%–20% |

---

## 2. 优先级定义

本文不把所有“必做”都解释为下一版本完成，而是进一步拆分：

| 等级 | 定义 | 时间含义 |
|---|---|---|
| **M0 架构必做** | 延后会扩大错误语义、破坏兼容性或导致未来大规模返工 | 立即开始，未来 1–2 个阶段完成 |
| **M1 L7 基线必做** | 成为可信 L7 代理引擎所需，但可按协议和场景分批交付 | 未来数月持续推进 |
| **E 可演进** | 有长期价值，应预留扩展点，但不是当前承诺 | 6–18 个月或按需求触发 |
| **X 暂不投入** | 成本/收益不匹配当前定位 | 不进入近期排期 |

所有公开能力还应标记成熟度：

```text
Stable        已有真实线路测试，行为受兼容性约束
Experimental  可使用，但行为和模型可能调整
Preview       面向早期验证，不保证完整语义
Planned       仅设计或占位，不应宣传为已支持
```

---

## 3. 数据面处理与 Mutation Pipeline

### 当前判断

RelayCore 已有 header/body 分阶段 interceptor、TapBody、规则和脚本链，但 `Flow` 观察状态与真正在线路上传输的 `Request/Response/Body` 尚未形成稳定的单一事实来源。

风险集中在：

- Body 尚未消费时执行 Body-stage 规则
- 修改 Flow 不一定修改真实线路对象
- Response header/status 修改与原始 `res_parts` 可能脱节
- Body 替换后 Content-Length/Content-Encoding 未必同步
- Rule、Script、Manual Intercept 的修改返回语义不完全一致

### 必做（M0）

1. **建立真实线路 Action Matrix**
   - 每个公开 Action 必须通过真实客户端 → RelayCore → 上游 E2E 验证
   - 断言上游或客户端实际收到的字节，而不是只断言 Flow 对象被修改
   - 覆盖 RequestHeaders、RequestBody、ResponseHeaders、ResponseBody、WebSocketMessage

2. **引入显式 Mutation 类型**
   - `RequestMutation`
   - `ResponseMutation`
   - `HeaderPatch`
   - `BodyMutation`
   - mutation 作为线路控制结果，Flow 作为观察/记录结果

3. **引入 BodyPlan**
   - `PassThrough`
   - `Tap`
   - `Buffer`
   - `Replace`
   - `TransformStream`
   - `Reject`
   - 根据已编译规则、脚本 hook、inspect 需求选择最低成本策略

4. **定义 interceptor 合并规则**
   - 顺序、优先级和短路语义固定
   - terminal action 与 non-terminal mutation 分离
   - 明确多个 header/body mutation 的覆盖和冲突规则

5. **维护 HTTP framing 正确性**
   - Body 替换后重算或删除 `Content-Length`
   - 正确处理 `Transfer-Encoding`
   - 保留 trailers
   - 不允许旧压缩头描述新未压缩 Body

6. **公开能力真实性治理**
   - 无真实线路测试的 Action 标记 Experimental/Preview
   - 完全未接线的能力不得在 README/API schema 中宣称 Stable

### 可演进（E）

- 真正的 chunk-by-chunk 流式文本/正则变换
- 增量 JSON/SSE/Protobuf 转换
- 零拷贝 splice/sendfile 路径
- 多脚本并行只读 observer
- 可撤销 mutation transaction
- 分布式或远程 interceptor
- 有意构造畸形 HTTP framing 的协议测试模式

### 验收标准

- 每个 Stable Action 至少有一个真实线路 E2E
- Flow 最终状态与线路实际状态一致
- 大 Body 在无 Body 规则时保持流式
- 超预算时结构化记录 skipped/degraded 原因

---

## 4. Connection / Stream / Message 数据模型

### 当前判断

现有 `Flow` 对单个 HTTP exchange 友好，但对 HTTP/2 多路复用、WebSocket 长会话、CONNECT 父子流、QUIC stream、TCP/UDP message 的表达能力不足。

### 必做（M0）

1. **增加兼容性的关系字段**
   - `connection_id`
   - `parent_flow_id`
   - `stream_id`
   - `protocol_stack`
   - 均以 optional/backward-compatible 方式加入

2. **区分生命周期层级**
   - Connection：传输连接生命周期
   - Stream/Flow：HTTP exchange 或逻辑 stream
   - Message：WebSocket frame、SSE event、未来 TCP/UDP message

3. **统一错误和关闭原因**
   - client close
   - upstream close
   - timeout
   - reset
   - policy drop
   - parser error
   - TLS error

4. **类型化事件模型**
   - `FlowStarted`
   - `HeadersReceived`
   - `BodyAvailable/BodyChunk`
   - `MessageReceived`
   - `MutationApplied`
   - `InterceptPaused/Resolved`
   - `FlowCompleted/Errored`

5. **保持 adapter 稳定**
   - 现有 Flow JSON 在兼容窗口内只做加法
   - HTTP/Tauri/MCP 可消费同一事件模型

### 可演进（E）

- QUIC connection migration 地址历史
- HTTP/2/3 priority/dependency 信息
- TLS session resumption 与 0-RTT
- 跨连接因果关系
- 请求 fan-out/fan-in 图
- 分布式 trace 与代理 Flow 的关联图

### 验收标准

- CONNECT 外层与内层 Flow 可关联
- 同一 HTTP/2 连接的 streams 可归组
- WebSocket/SSE messages 不再依赖覆盖整个大 Flow 才能增量传输

---

## 5. HTTP/1.x 与 Body 编解码

### 必做（M1）

1. HTTP/1.0 与 HTTP/1.1 基础兼容
2. keep-alive、half-close、upgrade、CONNECT 生命周期
3. hop-by-hop header 清理和保留规则
4. chunked、trailers、Content-Length mismatch
5. 慢速 headers/body、取消、客户端提前断开
6. 内容编码：
   - gzip
   - deflate
   - brotli
   - zstd（依实际生态需求）
7. 解压 → 规则/脚本处理 → 重编码的完整路径
8. 压缩炸弹和解压后大小预算
9. charset 与二进制 Body 的稳定表示
10. malformed/边界输入 fuzz targets

### 可演进（E）

- 宽松语法透传模式
- 原始 header 字节级保留
- 非标准 HTTP 方法/状态行的高级兼容
- 可配置 parser strictness profile
- intentional malformed HTTP 生成
- request smuggling 防御/研究模式

### 验收标准

- 常见压缩 JSON 可查看、匹配、修改并正确返回
- 无修改场景不无谓解压/缓冲
- trailer 和 chunked 不因 Tap/Throttle 丢失

---

## 6. HTTP/2

### 必做（M1）

1. 明确并测试三段能力：
   - client → RelayCore
   - RelayCore → upstream
   - H2 ↔ H1 协议转换
2. stream 级 Flow ID 和 connection 归属
3. 并发 streams、reset、GOAWAY、取消
4. flow control 与背压
5. header pseudo-fields 正确映射
6. gRPC 所需的 framing 基础不被破坏
7. H2 线路上的规则、脚本和 intercept 与 H1 语义一致
8. 覆盖真实浏览器/curl/hyper 客户端兼容测试

### 可演进（E）

- h2c
- Extended CONNECT / RFC 8441 WebSocket over H2
- priority 信息
- server push 观察
- 更精细的 stream queue timing
- H2 特定故障注入

### 验收标准

- HTTP/2 不只是“能成功请求”，而是多 stream、reset、规则修改均有 E2E
- H2 upstream 降级 H1 时 Flow 语义稳定

---

## 7. WebSocket 与 SSE

### 7.1 WebSocket

#### 必做（M1）

- 握手 request/response 修改真实生效
- text/binary/ping/pong/close 方向与生命周期正确
- message 修改、drop、inspect 有真实线路 E2E
- 活跃连接帧注入
- idle timeout、half-close、异常关闭
- 背压和 bounded message history
- `permessage-deflate` 等常见扩展策略明确：支持、透传或显式降级

#### 可演进（E）

- Socket.IO 语义解码
- subprotocol codec
- 帧级与消息级双视图
- 客户端/服务端 replay
- WebSocket 故障注入

### 7.2 SSE

#### 必做（M1）

- 识别 `text/event-stream`
- 增量解析 `event/id/retry/data`
- 跨 chunk UTF-8 和行边界处理
- bounded buffer 和 dropped count
- event-level 实时推送
- stream close/disconnect 的正确状态

#### 可演进（E）

- SSE event 持久化与按序回放
- event 级规则和脚本
- 重连/Last-Event-ID 辅助分析
- SSE mock/replay server

---

## 8. TLS / PKI 策略层

### 必做（M0/M1）

1. ClientHello/SNI 提取
2. ALPN negotiation 记录
3. 按 host/IP 决定：
   - MITM
   - passthrough
   - drop
4. ignore hosts / allow hosts 策略
5. upstream 证书验证策略：
   - native roots
   - custom CA bundle
   - explicit insecure（带审计和明显状态）
6. 动态叶证书：DNS/IP SAN、wildcard、IDN
7. 原始 upstream 证书链和错误原因可观察
8. TLS handshake timeout 和 timing
9. CA key 文件权限与加载失败策略

### 可演进（E）

- 按 host 选择 mTLS client certificate
- JA3/JA4 等 TLS fingerprint metadata
- session resumption
- 证书 pinning 诊断
- TLS key log / Wireshark 协作
- STARTTLS/opportunistic TLS
- DTLS
- HSM/Keychain 托管 CA key

### 验收标准

- pinning/不兼容应用可以按策略 passthrough，而不是只能失败
- SNI/ALPN 在 Flow/Connection 中可用
- insecure 模式不会静默开启

---

## 9. 代理模式与 Capture Sources

### 必做（M1）

1. **Regular forward proxy**：继续作为默认稳定入口
2. **Transparent**：维持 Linux/macOS 可用性和平台矩阵
3. **Upstream**：HTTP/HTTPS、认证、bypass、fail-open/closed
4. **Reverse**：
   - HTTP/HTTPS 固定 upstream
   - 与 regular listener 并存
   - 多 listener 配置
   - preserve/rewrite host
   - 为 RelayCraft Share 和独立开发场景提供通用底座
5. CaptureSource/Listener 抽象不得绑定 HTTP parser
6. listener 级状态、错误、端口冲突和 shutdown 统一管理

### 可演进（E）

按长期价值顺序：

1. SOCKS5 CONNECT
2. Local Capture / 按 PID、进程名捕获
3. TUN interface
4. DNS listener
5. WireGuard capture
6. SOCKS5 UDP ASSOCIATE
7. 远程 capture agent

### 暂不投入（X）

- 同时实现所有模式
- 自研复杂 reverse routing 产品层
- 将 platform capture 细节写入 HTTP engine

### 验收标准

- 新 capture mode 只负责产出标准 Connection，不复制规则/脚本/协议处理链

---

## 10. 通用 TCP/UDP/DNS 与 L4 扩展

### 必做（M0 架构）

这里的“必做”是保留架构能力，而不是近期交付完整 L4 产品：

1. `CaptureSource` 不假设输入一定是 HTTP
2. protocol detection 可返回 HTTP/TLS/Raw/Unknown
3. `Layer::Tcp/Udp/Custom` 保持可扩展
4. Interceptor 生命周期可在不破坏现有 API 的情况下增加 TCP/UDP/DNS hooks
5. Connection/Message 模型不依赖 HTTP 字段
6. metrics、audit、storage 对非 HTTP Flow 不崩溃

### 可演进（E）

- Generic TCP relay
- TLS sniff + raw TLS passthrough/MITM
- TCP message segmentation 策略
- UDP session/datagram hooks
- DNS UDP/TCP proxy
- DNS mock/override/block
- DoT/DoH/DoQ 识别
- MQTT/Redis/PostgreSQL 等协议 codec
- DTLS metadata/MITM

### 验收标准

- 当前不实现完整 L4，也不会因 Flow/API/Interceptor 设计锁死未来扩展路径

---

## 11. Rules / Scripts / Manual Intercepts

### 必做（M0/M1）

1. 三者共用统一 hook 和 mutation contract
2. Rule compile 阶段验证：
   - stage/filter/action 合法性
   - regex/glob 编译
   - action 是否真实受支持
3. 规则执行结果结构化：
   - matched
   - skipped + reason
   - failed + reason
   - mutated fields
   - terminated
4. Rule 和 Script 修改结果在线路层语义一致
5. Inspect body ownership、超时和恢复行为明确
6. Script 资源边界：
   - execution timeout
   - body read budget
   - env allowlist
   - fetch allowlist
   - shared state hard/soft limit
7. 热更新失败时保留上一份可用脚本
8. 所有错误可由 adapter/UI/MCP 读取，不要求查日志

### 可演进（E）

- TCP/UDP/DNS/TLS hooks
- 第三方模块生态
- async streaming transform
- 规则调用脚本 helper
- 自定义 commands/options
- 多脚本 dependency graph
- deterministic script replay
- WASM interceptor backend

### 验收标准

- 对同一修改，Rule 与 Script 产生相同线路结果
- 不支持的 Action 在加载时失败，而不是运行时无声 no-op

---

## 12. Fault Injection 与流量模拟

### 必做（M1）

- 固定延迟
- 带宽限制
- request/response drop
- mock response
- redirect/map local/map remote
- HTTP status/header/body 修改
- rate limit
- 所有能力有真实线路测试和可观测 trace

### 可演进（E）

- jitter 和延迟分布
- 概率丢包
- TCP reset/half-close
- 截断 Body
- slow headers/slow body
- DNS failure/delay
- TLS handshake failure
- malformed HTTP/WebSocket frame
- 按连接/用户/host 的状态化故障模型
- 可复现 seed

---

## 13. Recording / Body Storage / Replay / HAR

### 必做（M0/M1）

1. Flow metadata 与 Body 生命周期分离
2. 为未来 `BodyRef`/blob handle 预留兼容模型
3. bounded in-memory Flow 与 message history
4. SQLite schema migration 机制
5. retention/size policy，不允许无限增长
6. HAR 1.2 核心字段准确：
   - request/response
   - headers/cookies/query
   - redirectURL
   - body encoding
   - timing 的未知值不伪造
7. client request replay
8. replay 请求需要明确是否再次被捕获、是否应用规则

### 可演进（E）

- 小 Body inline、较大 Body 压缩、超大 Body 文件/blob 分层
- server-side response replay
- session/workspace 概念
- `.relay` 原生归档格式
- HAR/session 流式导入导出
- replay concurrency/timing reproduction
- event journal 重建 Flow
- remote/object storage backend

### 验收标准

- 长时间运行不会因 Body/WS history/SQLite 无界增长失控
- HAR 中没有用固定 0 冒充实际 timing

---

## 14. Content Codec / Semantic Inspector

### 必做（M1）

- raw/text/hex/base64 稳定表示
- JSON/XML/form/multipart 基础解析
- Content-Encoding 解压和重编码
- MIME、charset、文件名 metadata
- codec 失败不影响原始流量透传
- codec 和 renderer 分离：core 输出语义，UI/TUI/MCP 自行展示

### 可演进（E）

按价值逐步增加：

1. SSE
2. gRPC framing
3. Protobuf descriptor-aware decode/re-encode
4. GraphQL
5. MessagePack/CBOR
6. Socket.IO
7. MQTT
8. 图片/音视频/压缩包 metadata
9. 用户自定义 codec
10. 可编辑 content view 与 re-encode

### 验收标准

- 新 codec 不需要修改 HTTP proxy 主循环
- codec 异常、超时或大输入有预算保护

---

## 15. Observability / Reliability / Performance

### 必做（M0/M1）

1. ✅ 修复并可信化 benchmark harness（已完成，待补 baseline）：
   - 验证 target/proxy 进程和端口 ✅
   - 支持 `CARGO_TARGET_DIR` ✅
   - 验证响应成功率 ✅
   - 失败时不生成有效报告 ✅
   - ✅ 额外修复：就绪探测不带 `--fail`（502 曾判为 ready）、端口被占用时不中止
     （曾导致外部监听者冒充代理被压测）、无法读取的 RSS 记为 0MB 反而 PASS、
     `--strict` 在 release 模式不可达、`commit-baseline.sh` 不读状态字段
   - ✅ 额外修复：Python 上游饱和于 ~2.7k req/s（低于 10k DoD），导致所有吞吐数字实际
     测量的是上游。已替换为 `benchmarks/rust_echo_server.rs`（直连 ~104k req/s）。
     实测参考（M4 Max / 100 连接 / 1KB）：直连上游 ~104k req/s，经 RelayCore ~43k req/s，
     P99 ~10.3ms，成功率 100%
2. ✅ 已建立 0.10.0 可信 baseline（`benchmarks/results/baseline_v0.10.0.{json,md}`）：
   cold start 144.2ms、idle 内存 52.0MB、S1 吞吐 47,678.8 ±578.3 req/s、P99 1.17ms、
   成功率 100%；方差从旧基线的 ~40% 降至 ~1.2%。复现命令：
   `CONNECTIONS=25 ./benchmarks/bench_minimal.sh release --version 0.10.0 --runs 5 --warmup-runs 2 --duration 20`
3. mitmproxy differential fixtures：比较线路行为，不只比较 QPS
4. 关键协议和 Action E2E
5. backpressure、channel full、subscriber lagged 可观测
6. 取消、超时、shutdown 不泄漏 task/connection
7. 2h soak，核心场景定期运行
8. parser/codec/rule fuzz targets
9. metrics 区分：
   - connections
   - HTTP flows
   - streams/messages
   - bytes
   - dropped events
   - body degraded
   - rule/script errors
   - TLS/upstream errors
10. CI Linux quality gate 保持阻塞；macOS/Windows 至少有周期性真实验证

### 可演进（E）

- 24h/7d soak
- TSan/loom/model checking
- OpenTelemetry spans
- eBPF/OS-level process/network metrics
- 固定专用机器性能门禁
- flamegraph/perf regression bot
- chaos testing
- large-scale distributed load generation

### 验收标准

- 性能报告首先可信，其次才比较数值
- Stable 能力均有失败路径和资源释放验证

---

## 16. Security / Privacy

### 必做（M1）

- CA private key 权限和错误处理
- API 默认 loopback
- 非 loopback 暴露需要显式鉴权/确认
- proxy listener 远程暴露策略明确
- upstream credentials、API token 不进入日志和 Flow
- redaction 在 HTTP/Tauri/MCP/导出路径一致
- MapLocal 路径 sandbox
- script env/fetch allowlist
- Body、headers、URL 的大小和数量限制
- 依赖漏洞检查和锁文件更新流程

### 可演进（E）

- listener 端 proxy authentication
- 多租户隔离
- per-client policy
- mTLS API/control plane
- encrypted local body store
- CA key Keychain/HSM 托管
- SBOM、二进制签名和 provenance

---

## 17. Adapters 与 RelayCraft 对齐

### 必做（M0/M1）

1. Core/runtime 能力先通过窄 service traits 暴露
2. HTTP/Tauri/MCP/CLI 不各自实现不同业务语义
3. API 变更优先只做加法，破坏性变更需要版本化
4. 每个底层里程碑跑 RelayCraft 核心 contract fixtures
5. RelayCraft 特有字段由 adapter/extension metadata 承担
6. `.ai/archived/relaycraft-integration/056-relaycraft-mitmproxy-migration-checklist.md` 保持为接入清单，不反向主导 engine 数据模型
7. 在 core 模型稳定后再生成 OpenAPI/codegen，避免冻结临时模型

### 可演进（E）

- RelayCraft 双引擎灰度
- 完整 OpenAPI + TS/Swift codegen
- 兼容 `/_relay/*` 的迁移 adapter
- 跨版本 capability negotiation
- 远程 RelayCore host
- 第三方宿主 SDK

### 验收标准

- Core 数据面修复不要求 RelayCraft 大规模重写
- RelayCraft 接入需求不会把 UI-specific 概念下沉到 transport engine

---

## 18. MCP / AI-Native 能力

### 必做（M1）

- MCP 使用稳定 service traits，不直接复制业务逻辑
- flow/rule/intercept/audit 输出类型化
- Body 默认限量、脱敏、按需读取
- mutation 有 actor、来源和审计记录
- Agent 写操作具备明确错误和幂等语义
- 能解释规则命中、跳过和失败原因

### 可演进（E）

- 语义 codec 直接向 Agent 暴露 gRPC/Protobuf/GraphQL 结构
- before/after mutation diff
- 自动重放验证
- 临时规则事务和自动清理
- 流量聚类、异常检测和因果分析
- trace/Flow/日志联合诊断
- Agent 可订阅的高层语义事件

### 原则

AI/MCP 是 RelayCore 的差异化 adapter，但不能替代底层协议正确性。先保证结构化事实可信，再增加自动分析。

---

## 19. 分阶段路线

> 以下时间仅表示建议的投入顺序和滚动窗口，不是版本 deadline。每个阶段以退出条件为准；未完成的必做项顺延，不为追赶日期并行铺开大量新协议。

### Phase A — 近期架构关口（0–8 周）

目标：公开 L7 能力在线路层真实闭环。

- Mutation Pipeline
- BodyPlan
- Rule/Script/Intercept 统一语义
- Stable Action 真实线路矩阵
- Content-Length/Transfer-Encoding/Content-Encoding 正确性
- connection/stream/message 关系字段
- typed event 基础
- benchmark harness 修复
- 当前版本可信 baseline

**明确不扩张**：暂不同时新增 DNS/SOCKS/TUN/WireGuard/HTTP3 MITM。

### Phase B — L7 基线完善（2–6 个月滚动推进）

目标：成为可信的现代 HTTP 调试/修改引擎。

- HTTP/1 边界
- gzip/br/deflate
- HTTP/2 stream 语义
- WebSocket 修改、注入、关闭、压缩策略
- SSE 一等事件
- SNI/ALPN
- TLS passthrough/ignore hosts
- upstream TLS 策略
- Reverse mode
- 长时间稳定性和存储治理

### Phase C — 现代 L7 能力（6–12 个月持续演进）

目标：在 mitmproxy 基础覆盖之上形成结构化和自动化优势。

- client/server replay 完善
- codec registry
- gRPC/Protobuf
- GraphQL/MessagePack/Socket.IO
- 更完整 fault injection
- event journal/lazy body
- OpenTelemetry 与 mutation explainability
- RelayCraft 深度接入和 MCP 能力统一

### Phase D — L4 与捕获模式演进（12–24 个月或按需求触发）

目标：保持 L7-first，同时扩展通用网络入口和非 HTTP 流量能力。

- Generic TCP/TLS
- UDP datagram hooks
- DNS
- SOCKS5
- Local Capture/TUN
- WireGuard
- QUIC metadata
- 依据生态成熟度评估 HTTP/3 MITM

---

## 20. 未来几个月的明确范围

### 必须完成或持续推进

1. 线路级 Mutation 语义闭环
2. Body-stage 规则/脚本真实生效
3. Stable Action E2E matrix
4. HTTP framing + content encoding
5. connection/stream/message 兼容模型
6. HTTP/2、WebSocket、SSE 主流 L7 能力
7. SNI/ALPN + TLS passthrough 基础策略
8. Reverse mode
9. benchmark、differential、fuzz、soak 的可信反馈
10. 存储增长和资源释放边界
11. RelayCraft contract 持续回归，但不以全量迁移阻塞 core

### 只做设计或扩展点，不承诺完整交付

- Generic TCP/TLS
- UDP message-level interception
- DNS
- SOCKS5
- Local Capture/TUN
- WireGuard
- gRPC/Protobuf 完整可编辑视图
- server replay 高级匹配
- OpenTelemetry 全链路

### 暂不投入

- 完整 HTTP/3 MITM
- DTLS MITM
- 覆盖所有 mitmproxy addon hooks
- 通用任意协议自动解析
- 为追求功能数量而增加未接线 API 枚举

---

## 21. 决策准则

新增能力进入排期前，依次回答：

1. 它是否修复已宣称能力的错误线路语义？若是，优先级最高。
2. 它是否避免未来 breaking data model / interceptor redesign？若是，进入 M0。
3. 它是否属于主流 L7 调试场景？若是，进入 M1 候选。
4. 它是否只对单一 adapter/UI 有价值？若是，优先留在 adapter。
5. 它是否需要新的 L4 capture/protocol stack？若是，默认进入 E。
6. 是否有真实用户场景、fixture 和验收方式？没有则不进入 Stable 路线。
7. 是否能在不复制处理链的前提下复用现有 CaptureSource/Interceptor/Service？不能则先解决架构。

---

## 22. Definition of Done

一项 engine 能力只有同时满足以下条件，才能标记 Stable：

- 有明确的数据面语义
- 有真实线路 E2E
- 有失败路径测试
- 有大小/时间/并发预算
- 有结构化错误和可观测指标
- Flow 记录与线路事实一致
- HTTP/Tauri/MCP 至少能通过共享 service 获取结果
- 文档没有超出真实实现进行宣传
- `./scripts/ci-check.sh` 通过
- 性能和资源消耗没有无法解释的显著回退

---

## 23. 最终方向

RelayCore 不需要在短期成为“支持所有协议和捕获模式”的庞大网络平台。

它首先需要成为：

> 一个数据面语义可靠、流式处理正确、HTTP/HTTPS/HTTP2/WebSocket/SSE 能力完整、规则和脚本真正在线路上生效、可嵌入且可持续演进的现代 L7 代理引擎。

在此基础上，通过稳定的 Connection/Stream/Message、CaptureSource、ProtocolLayer 和 Interceptor 扩展点，逐步演进到 Generic TCP/UDP/DNS、SOCKS、TUN、WireGuard 和 QUIC，而不要求现在一次实现。

RelayCraft 的接入应成为这套底座成熟度的证明，而不是底座设计的唯一驱动力。

---

## 24. 已知失真项（代码审计结论）

> 本节记录「已公开/已实现于代码，但与线路行为不符」的能力。这些项在修复并通过 §22 之前
> **不得标记 Stable**，也不应在 README / API schema / 文档中作为可用能力宣传。
>
> 审计基线：`eb4c4a5`（v0.10.0）。每条均附 file:line，随修复逐条移除。

### 24.1 规则动作已接受但未作用于线路（非 Tauri 宿主）

> **部分修复（A5a，2026-09-12）**：HTTP 响应方向已收敛到「Flow 为单一事实来源」——
> `handle_http_request` 现通过 `build_client_response_head` 从 Flow 重建客户端响应头与状态码
> （`proxy/http_utils.rs`），同时 body 仍保持上游流式传输。以下动作因此恢复生效并有
> wire-level 断言：`Add/Update/DeleteResponseHeader`、`SetResponseStatus`。
>
> 仍开放的项见下方表格。

| Action | 状态 | 说明 | 位置 |
|---|---|---|---|
| `Add/Update/DeleteResponseHeader` | ✅ 已修复 | 由 Flow 重建响应头 | `proxy/http_utils.rs` `build_client_response_head` |
| `SetResponseStatus` | ✅ 已修复 | 由 Flow 重建状态码 | 同上 |
| `SetResponseBody` / `TransformResponseBody` | ✅ 已修复 | 响应 body 替换后由 Flow 提供字节并重建 framing（`content-length` 重算，丢弃 `transfer-encoding`/`content-encoding`），与请求方向对称 | `proxy/http_utils.rs` `build_response_body_from_flow` / `reframe_response_headers_for_replaced_body`；`proxy/http.rs` |
| `SetRequestBody` / `TransformRequestBody` | ✅ 已修复 | 检测到 Flow 持有替换体时改由 Flow 提供 body 并重建 framing（`content-length` 重算，丢弃 `transfer-encoding`/`content-encoding`）；未被替换的 body 仍保持流式 | `proxy/http_utils.rs` `build_request_body_from_flow` / `reframe_request_headers_for_replaced_body` |
| `SetTtl` | ⬜ 未实现 | 自带告警日志，属有意未实现 | `proxy/server.rs:238-247` |
| `MockWebSocketMessage` | ✅ 已修复 | mock 产生的帧即该阶段刚压入的消息，现直接替换（此前被通用终止路径转成 Drop） | `runtime/src/interceptors/rule.rs` |
| WebSocket 握手响应头 | ✅ 已修复 | WS 路径现调用 `on_response_headers` 并记录 `flow.handshake_response`，101 由 Flow 构造；同时补齐了此前缺失的握手响应可观测性 | `proxy/websocket.rs` |
| `MapRemote`（WebSocket 握手） | ✅ 已修复 | 转发目标改由 Flow 的 `handshake_request.url` 决定；此前取自原始请求元数据，改写被忽略 | `proxy/websocket.rs` |
| `ForwardPort` 的 `target_host` | ✅ 已修复 | 主机名现被解析（IP 字面量直接用，主机名解析，失败回退原 IP，无可解析目标则保持不变而非拨 `0.0.0.0`） | `proxy/server.rs` `resolve_forward_port_target` |

Tauri 宿主经 `TauriInterceptor` → `ModifiedResponse` 使多数项生效（`tauri/src/interceptor.rs:123-128`），
但其代价见 24.4。

### 24.2 Body-stage 规则在非 Tauri 宿主无法匹配 —— ✅ 已修复

**历史问题**：`RuleInterceptor::on_request`/`on_response` 在 body 被 poll 之前执行 body 阶段
引擎，而 body 过滤器读取 `flow.layer.*.body`，该字段此刻恒为 `None`；TapBody 抓取到的 body
写入的是 FlowStore actor 中的**另一个 Flow 实例**，活 Flow 永远不可见。

**请求方向已修复**：`RuleInterceptor::on_request` 现在先用 `BodyPlan` 决策，仅当 body 阶段规则
**确实需要 body**（`RuleEngine::stage_consumes_body`：存在 `ResponseBody`/`WebSocketMessage`
过滤器，或存在 body 改写动作）时才在预算内物化并写入 Flow，然后执行阶段；否则保持流式。
超预算时打 `rule_skipped:body_truncated` 标签而**不**让规则在前缀上匹配。
契约由 `wire_matrix_body_stage_rule_matches_on_the_body` 在真实线路上锁定。

**响应方向已修复**：响应 body 阶段在 header 时刻执行，而 body 此时尚未读取，因此规则
interceptor 在**请求阶段**就把「需要响应 body」及预算写入 `Flow.meta`
（`stage_guard::request_response_body`）；代理据此在 header 投影**之后**、header 阶段**之前**
按预算保留响应 body 并写入 Flow；未声明需求时响应保持流式。
契约由 `wire_matrix_response_body_rule_matches_on_the_body` 锁定（同时断言 body 未被消费仍送达客户端）。

**顺序陷阱（已记录）**：`update_flow_with_response_headers` 会整体替换 `HttpResponse`，
因此保留必须发生在其**之后**，否则刚记录的 body 会被丢弃。

### 24.3 响应构造器 framing 不安全

`mock_to_response`（`proxy/http_utils.rs:208-235`）是唯一的 Flow→线路响应构造器：
- `:212-219` 原样复制全部 header，包括残留的 `Content-Length` / `Transfer-Encoding` / `Content-Encoding`
- `:222` `Bytes::from(b.content)` 不按 `b.encoding` 解码，而 `BodySource::Base64` 确凿写入
  `encoding: "base64"`（`rule/engine/actions/utils.rs:19-23`）⇒ 二进制 mock 会发出 base64 文本

hyper 1.10.1 对调用方给出的 `Content-Length` 直接采信（`proto/h1/role.rs:695-717`）：
release 下产生错帧，debug 下触发 `debug_assert!`。

### 24.4 重复执行与重复头部（Tauri 宿主）—— ✅ 已修复

**历史问题**：`runtime/src/lib.rs:1325-1336` 无条件加入 `RuleInterceptor`，Tauri 于
`tauri/src/commands/system.rs:208` 追加 `TauriInterceptor`，而 `CompositeInterceptor`
不对 `ModifiedRequest` 短路（`intercept/types.rs:186-212`）⇒ 每条规则执行两次；
`AddRequestHeader` 无条件 append（`actions/http.rs:146-152`）导致重复头部真实到达上游，
`Delay` 双重睡眠、`RateLimit` 双重计数。

**修复方式**：引入 `relay-core-lib/src/rule/stage_guard.rs` —— 首个执行该 stage 的
interceptor 在 `Flow.meta`（`#[serde(skip)]`，不进任何线路格式与存储）记录标记，
后续成员的规则执行跳过，但**各自宿主特有的职责（如 Tauri 的 body 缓冲、UI 事件）仍然执行**。
`RuleInterceptor` 记录标记，`TauriInterceptor` 依据标记跳过。
契约由 `wire_matrix_stage_guard_applies_mutation_once_per_chain` 在真实线路上锁定。

### 24.5 手工 Intercept「带修改恢复」在宿主间结果相反

`runtime/src/interceptors/rule.rs:56-62, 101-107` 将 `ModifiedRequest`/`ModifiedResponse`
落入 `_ => Drop` ⇒ body 阶段手工放行并修改返回 **403**；同一 API 在 Tauri 正常
（`tauri/src/interceptor.rs:82-94`）。直接违反 §11「Rule 与 Script 修改结果在线路层语义一致」。

### 24.6 校验与结构化结果缺失

- 非法 regex/glob/CIDR 降级为永不匹配的哨兵，无任何报错：`rule/engine/compiler.rs:52-57`、
  `matcher.rs:110, 122`
- `RuleOutcome::Skipped` 从未被构造；`RuleTrace`（`api/rule.rs:303-310`）全仓无构造点
- `ctx.trace` 被所有生产调用方丢弃（`interceptors/rule.rs:34,52,75,97,124`）
- `relay_core_rule_exec_errors_total` 的唯一来源无发送方 ⇒ 指标恒为 0

### 24.7 模型与时间语义失真

- `Flow.end_time` 在线路路径上从不设置 ⇒ `FlowSummary.duration_ms` 恒为 `null`
  （`actors/flow_store.rs:197-199`）
- `ResponseTiming.connect_time_ms` / `ssl_time_ms` 无任何赋值点 ⇒ HAR timing 只能填 0
- `NetworkInfo.sni` 与 `ConnectionInfo.tls_sni` 无写入方（`proxy/server.rs:171-172` 为 TODO）
- 关闭/错误原因无枚举，仅为自由字符串 tag；文档中提到的 `"error"` tag **没有任何生产者**

### 24.8 事件与存储

- `relay-core-api/src/event.rs` 为空文件；`FlowUpdate` 仅 4 个变体，无阶段区分
- SSE `event: http-body` 丢弃 `direction` 与 `body`（`http/routes/events.rs:58`）；
  文档注释承诺的 `event: intercept` 无任何代码发出（`events.rs:26`）
- CLI SSE 客户端 `ws-message` 分支反序列化必然失败（`cli/src/sse_client.rs:154`），且无测试
- 存储无迁移机制（全仓无 `ALTER TABLE` / `PRAGMA user_version` / `.sql`），
  无保留策略（对 flows/summaries 无任何 `DELETE`），脱敏只在输出路径
  （`runtime/src/lib.rs:1501-1564`），`persist_flow` 落盘为原始 Flow

### 24.9 协议与压缩

- HTTP/2 仅存在于 CONNECT-MITM 的 TLS 之后（`proxy/tunnel.rs:66`、`tls/ca.rs:311`）；
  明文首跳为 H1-only（`proxy/server.rs:271`）
- **入站 HTTP version 从不传播**：`build_forward_request` 从不设置 `.version(...)`
  （`proxy/http_utils.rs:238-314`），TLS 上游走 h2 只是 ALPN 的副产物
- 响应版本从不回传：`build_client_response_from_flow`（`http_utils.rs:437`）带
  `// TODO: Parse version...`，且无生产调用点
- ✅ **部分修复（A7）**：新增 `proxy/content_encoding.rs`。`gzip` / `deflate` 现在会被解码，
  规则看到的是明文；重写后按原编码**重新编码**，`Content-Encoding` 与所发字节一致。
  `br` / `zstd` 仍不解码，但重写时会**丢弃**该头并发送明文（而不是继续宣称是压缩数据），
  同时打 `body-encoding-dropped` 标签使降级可见。未知编码不做猜测。
  契约由 `wire_matrix_gzip_response_rewrite_stays_decodable` 锁定（客户端按声明的编码解码必须得到替换内容），
  已双向验证：不重新编码时该测试必失败。
- ⬜ 仍未实现：`br` / `zstd` 解码与重编码；请求方向的 `Content-Encoding` 处理；
  规则匹配响应体时仍未对 compress 后的 body 做「解压→匹配→重编码」全链路（当前仅重写路径生效）

### 24.10 验证体系基线

- 全 workspace **493 个测试**，其中 socket 级 E2E **仅 22 个**，全部位于 `relay-core-lib/tests/`
- **不存在任何**「修改后断言上游/客户端实际收到的字节」的测试
- 两个 UDP E2E 为 `#[cfg(not(target_os = "linux"))]`（`udp_integration_test.rs:45,101`）
  ⇒ 在唯一的阻塞门禁 Linux 上被跳过
- CI 无 soak、无 fuzz、无 mitmproxy differential、coverage 无阈值、
  无 cargo-audit/deny、macOS/Windows 仅构建不测试

**载荷生成器上限（重要）**

- 默认 `CONNECTIONS=100` 在 20s 持续压测下会**耗尽客户端临时端口**
  （oha 报 `Can't assign requested address (os error 49)` 数万次），成功率塌陷到 0–30%。
  这是**压测客户端**的限制，不是代理回归。harness 现已识别该特征并把该轮标记为
  `INVALID` 而非 `FAIL`，避免产生假回归信号。
- 根因是代理响应携带 `Connection: close`，使 oha 无法复用连接；每次请求都要新端口。
  根治需单独决策（是否让代理默认 keep-alive 或改用 keep-alive 压测模式）。
  当前 baseline 以 `CONNECTIONS=25` 采集，并在报告 methodology 中记录。

**已知的待办与代价（A5b）**

- Tauri 宿主对**每个**携带 body 的请求都会 `collect()` 完整 body（`tauri/src/interceptor.rs:65,147`，
  且不受 `rule_body_inspect_budget` 限制），因此该宿主下 body 恒为缓冲态；A5b 之后会再发生一次
  「Flow → 线路」的物化。功能正确（非预算超限路径下字节等价），但属重复开销。
  根治需要在 A6（BodyPlan）中统一 body 计划，而不是各处自行缓冲。
- 未来在 `Flow.meta` 中引入「body 显式替换」标记，可让未被替换的 body 免于该次物化。

**A6 BodyPlan / 类型化事件（进行中）**

- ✅ 新增 `relay-core-api/src/body_plan.rs`：`BodyPlan { PassThrough, Capture{limit}, Buffer{limit} }`
  与纯决策函数 `decide(BodyPlanInputs)`，含单测（无消费者→PassThrough；仅观察→Capture 保持流式；
  任一改写方→Buffer；budget=0→不缓冲）。
- ✅ 新增 `relay-core-lib/src/proxy/body_plan.rs`：`PrefixBuffer` 保留有界前缀且**不改变线路字节**，
  超预算明确标记 `truncated`（供 body 规则拒绝对前缀做匹配），并有单测覆盖「超限仍完整转发」
  与「读到上限即停」。
- ✅ `TapBody` 改为委托 `buffer_prefix`，消除重复实现，原有测试全部通过。
- ✅ `relay-core-api/src/event.rs` 不再是空文件：补 `FlowEvent`（9 个阶段化变体）与 `CloseReason`
  枚举（对应 §4-3/§4-4 的缺口），含序列化单测。
- ✅ `RuleEngine::stage_consumes_body`：区分「body 阶段规则只是改元数据」与「真的需要 body」，
  避免为前者付出缓冲代价；含 3 个单测。
- ✅ `buffer_body_within_budget`：决策发生在转发之前的场景用物化（按帧读取、到预算即停），
  与 `buffer_prefix`（观察场景，仅保留流过的字节）职责分离。
- ✅ `RuleInterceptor::on_request` 已接线 BodyPlan，请求方向 body 阶段规则可匹配（§24.2 请求侧关闭）。
- ✅ 响应方向接线完成（§24.2 关闭）：请求阶段声明意图 → 代理在 header 投影后按预算保留 → header 阶段匹配。
- ✅ **body 观察策略已显式化**：`ProxyPolicy::body_observation`
  （`off` / `prefixed` / `full`）。观察是产品决策，不再从「有没有 body 阶段规则」推断——
  桌面 UI 要显示 body，无界面运行不需要，推断会静默改变用户能看到什么。
  默认 `off`：保留流式，且计划层有回归测试锁定「无消费者 → 零拷贝」。
- ⬜ Tauri 宿主尚未改为依赖该策略跳过自身缓冲；现在已有显式开关，这一步不再会改变
  用户可见行为（需要把 Tauri 的 UI 依赖改为 `prefixed`/`full`）。

**修复进度（2026-09-12）**

- ✅ harness 可信化：就绪探测带 `--fail`、进程与端口预检、oha 成功率 DoD、
  RSS 不可测即失败、报告闸门、`CARGO_TARGET_DIR` 支持、`commit-baseline.sh` 拒绝非 PASS 报告
- ✅ 上游性能上限解除：`benchmarks/rust_echo_server.rs` 取代饱和于 ~2.7k req/s 的 Python
  实现，harness 现可测量代理本身（实测 ~43k req/s / P99 10.3ms / 100% 成功率）
- ✅ wire-level 矩阵（`relay-core-lib/tests/wire_matrix.rs`）：**6 个用例全部通过，无 `#[ignore]`
  占位**（基线往返、请求头、请求 body、响应头、响应状态、链式单次应用、WS 握手响应头）
- ✅ **0.10.0 baseline 已产出**（见 §15-2）
- ✅ A5a HTTP 响应方向收敛（§24.1 响应头/状态已修复）
- ✅ A5b 请求 body 替换生效并重建 framing（§24.1 `SetRequestBody` 已修复）
- ✅ A5c WS 握手响应收敛并补齐 `on_response_headers`（§24.1 WS 握手项已修复）
- ✅ A1 双执行已修复（§24.4，stage_guard）
- ✅ §24.1 响应 body 替换已修复（`SetResponseBody`/`TransformResponseBody`）
- ✅ §24.1 `MockWebSocketMessage` 已修复（帧替换而非丢弃）
- ✅ §24.1 `MapRemote`(WS) 已修复（目标由 Flow 决定）
- ✅ §24.1 `ForwardPort` host 已修复（此前 host 被丢弃，仅 port 生效）
- ✅ §24.1 `MapRemote`(WS) 已修复
- ✅ §24.1 `ForwardPort` 的 `target_host` 已修复
- ⬜ §24.1 剩余：`SetTtl`（有意未实现：需要 raw socket 访问，超出当前连接模型）
- ⬜ §24.2–§24.9 的其余线路缺陷仍开放
