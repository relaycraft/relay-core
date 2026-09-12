# RelayCore Learning Directions from Proxelar and Rama

> 基于 Proxelar 与 Rama 实际源码审计的 RelayCore 学习方向

- **Status**: Active / 设计参考
- **Created**: 2026-08-27
- **Updated**: 2026-08-27
- **Scope**: 借鉴方向、反例与 RelayCore 落地优先级
- **Related**: [`l7-first-engine-evolution-roadmap.md`](./l7-first-engine-evolution-roadmap.md)
- **Evidence snapshots**:
  - Proxelar: [`02cfeb21`](https://github.com/emanuele-em/proxelar/tree/02cfeb21c88886f605c71ec0f0e3c3e931b04768)
  - Rama: [`da137803`](https://github.com/plabayo/rama/tree/da13780305c4a5251d3a76d18a8dc085481d736f)

---

> ## 阅读须知 / Reading Notice
>
> 本文档是**参考来源与借鉴优先级**说明，不是交付承诺。
> 「做什么、按什么顺序做、验收标准」以
> [`l7-first-engine-evolution-roadmap.md`](./l7-first-engine-evolution-roadmap.md) 为准，
> 本文档只回答「为什么值得学、哪些不能照搬」。
>
> 因此出现重叠时（如 §7 映射表、§8 实施顺序）**以路线图为准**，本文档不再维护独立排期。
> 历史路线图已归档至 `.ai/archived/**`，不再作为推进目标。
>
> This document explains *where the ideas come from and what not to copy*; the roadmap owns
> scope, ordering and acceptance criteria. Historical roadmaps are archived and are background only.
>
> **Language**: 本文档当前为中文单语正文，英文版待补。

---

## 0. 文档目的

本文不是竞品功能表，也不建议 RelayCore 改用 Proxelar 或 Rama 作为底层依赖。

目标是从两个方向不同的 Rust 项目中提取可复用经验：

- **Proxelar**：一个紧凑、单作者主导、面向开发者的本地流量工作台；产品面宽，底层协议深度有限。
- **Rama**：一个大规模、通用、Service/Layer 驱动的网络框架；协议与测试深度强，但复杂度远高于 RelayCore 当前需要。

本文回答：

1. 哪些设计值得 RelayCore 近期直接吸收？
2. 哪些只能学习思想，不能照搬实现？
3. 哪些方向不适合 RelayCore 的 L7-first 定位？
4. 如何把学习结果映射到现有 engine/runtime/api，而不做框架级重写？

---

## 1. 总体结论

### 1.1 两个项目的真实定位

| 维度 | Proxelar | Rama |
|---|---|---|
| 本质 | 本地 MITM 流量工作台 | 通用网络 Service 框架 |
| 主要用户 | 直接使用 CLI/TUI/WebUI 的开发者 | 构建网络服务和代理的 Rust 开发者 |
| 当前版本 | 0.5.x | 0.4.x |
| 核心优势 | Body 修改闭环、content views、分发与文档 | 协议分层、H2/TLS/WS 深度、平台边界、验证体系 |
| 主要弱点 | 协议模式多数较浅、内存 session、规则简单 | 类型与 crate 复杂度极高、学习和编译成本大 |
| 对 RelayCore 的价值 | 产品与工程交付参考 | 底层架构与协议正确性参考 |
| 是否适合作为依赖 | 不建议 | 不建议在当前阶段整体引入 |

### 1.2 学习原则

> 从 Proxelar 学“如何让能力真实可用”，从 Rama 学“如何让协议语义长期正确”；不复制 Proxelar 的浅层功能扩张，也不复制 Rama 的通用框架规模。

### 1.3 RelayCore 应保持的独立性

RelayCore 已经拥有自己的优势：

- `CoreState` + narrow service traits
- API/runtime/engine 分层
- Rule/Script/Intercept/Audit/Policy
- CLI/TUI/HTTP/MCP/Tauri 多宿主
- RelayCraft 产品生态
- Deno/V8 scripting
- SQLite persistence
- 透明代理和平台能力

因此学习应采用“局部替换错误抽象”的方式，不应：

- 重写为 Tower/Rama 风格的全泛型框架
- 为获得更多 mode 数量复制浅实现
- 引入新的平行 runtime
- 将 RelayCore 变成无产品边界的通用网络库

---

## 2. Proxelar 实际能力判断

### 2.1 做得扎实的部分

#### Body 修改与线路同步

Proxelar 的 `CapturingHandler` 会根据 Script/Intercept 是否启用，在 Buffer 与 Streaming 之间切换，并将修改后的 method/URI/headers/body 真正重建到线路对象中。

相关实现：

- [`proxyapi/src/handler.rs`](https://github.com/emanuele-em/proxelar/blob/02cfeb21c88886f605c71ec0f0e3c3e931b04768/proxyapi/src/handler.rs)
- [`proxyapi/src/encoding.rs`](https://github.com/emanuele-em/proxelar/blob/02cfeb21c88886f605c71ec0f0e3c3e931b04768/proxyapi/src/encoding.rs)

它正确考虑了：

- Body 修改后移除旧 `Transfer-Encoding`
- Body 修改后更新 `Content-Length`
- gzip/br/zstd/deflate 解压后交给脚本
- 修改后按新旧 `Content-Encoding` 重编码
- 未修改时复用原始压缩字节，避免无谓重压缩
- Script 出错时透传原始响应

这正是 RelayCore 当前 Mutation Pipeline 最需要补齐的线路语义。

#### 较强的短周期回归测试

实际运行 `cargo test --workspace --locked`：

- 258 tests passed
- 0 failed
- 26 个 forward/reverse 网络 E2E
- 多平台 CI
- workspace line coverage 80% gate
- `proxyapi` line coverage 90% gate
- feature-off、package、rustdoc、audit、cargo-deny

这说明 Proxelar 不是 README 空壳；它对已实现的小范围能力有不错的回归纪律。

#### Content View

[`proxyapi/src/content.rs`](https://github.com/emanuele-em/proxelar/blob/02cfeb21c88886f605c71ec0f0e3c3e931b04768/proxyapi/src/content.rs) 提供了一个紧凑的内容识别层：

- gzip/br/zstd/deflate
- charset
- JSON/XML/HTML/form/multipart
- image/binary preview
- descriptorless Protobuf wire fields
- MessagePack JSON roundtrip

协议理解不深，但成功把“Body bytes → 可展示/可编辑语义”从 UI 中抽出来，值得 RelayCore 学习。

#### 产品交付

- Homebrew、Winget、Cargo、Docker
- TUI/WebUI/API
- CA 安装页
- 明确的 Known Limitations
- addon manifest 与完整性校验
- SBOM/provenance/checksum

这部分体现的是交付质量，不是协议深度，但对独立 RelayCore 产品同样重要。

### 2.2 能力表容易高估的部分

#### HTTP/2

Proxelar 接受 downstream H2/H2C，但所有 upstream request 被强制转换为 HTTP/1.1：

```text
H2 client → Proxelar → H1 upstream
```

没有完整的：

- upstream H2 multiplexing
- stream lifecycle model
- GOAWAY/RST_STREAM 语义
- end-to-end gRPC/H2
- H2 SETTINGS/flow-control relay

因此只能称为 H2 ingress compatibility，不是完整 H2 MITM。

#### DNS

DNS mode 是最小 UDP 转发器：

- exactly one question
- A/AAAA override
- UDP upstream
- A/AAAA answer 摘要

没有 DNS TCP、EDNS、truncation fallback、DoH/DoT/DoQ、完整 RR 模型和 rules/scripts。

#### UDP

UDP mode 是 fixed-target request/one-response relay，每个请求创建新的 upstream socket，不具备长期 session 或双向 datagram 流。

#### SOCKS5

只支持 no-auth CONNECT；没有 BIND、UDP ASSOCIATE 或 listener authentication。

#### Raw TCP

本质是双向 copy + directional chunks；没有 message interceptor、修改、drop、inject、STARTTLS 或协议 codec。

#### WireGuard

WireGuard tunnel 和 userspace stack 是真实实现，但从 netstack 获取的是 destination IP；TLS 路径未见 ClientHello SNI 解析，直接按 IP authority 生成证书。普通域名 HTTPS 的 hostname validation 存在明显风险，且缺少移动设备域名 HTTPS E2E。

#### Reverse

Reverse mode 只是固定 target URI rewrite：

- 无多 target/listener 模型
- 无 client-facing TLS termination 配置
- 无 reverse TCP
- 无 reverse WebSocket 专门 upgrade path

### 2.3 不应学习的 Proxelar 设计

1. **默认无限 Body capture**
   - 默认 `free/unlimited`
   - streaming forwarding 仍会在内存累积完整 capture

2. **内存 SessionRecorder**
   - HTTP/WS/DNS/UDP 全部存 Vec
   - HTTP/WS 无整体 retention
   - snapshot clone 整个 session
   - 无增量数据库和 crash recovery

3. **单 Lua VM + 全局同步 Mutex**
   - 无 execution timeout/fuel
   - 无限循环可阻塞请求和后续脚本执行

4. **功能 mode 与可编程处理链不统一**
   - HTTP 有 Script/Intercept/Rules
   - DNS/UDP/raw TCP 基本只是旁路观察

5. **浅规则系统**
   - 只有 map remote/local、redirect、mock、request header
   - 无 response/body/stage/priority/logical composition

6. **以 mode 数量代表底层完备度**

---

## 3. Rama 实际能力判断

### 3.1 Rama 的核心不是 Proxy，而是统一 Service 模型

Rama 用一个通用抽象覆盖不同协议层：

```rust
trait Service<Input> {
    type Output;
    type Error;
    async fn serve(&self, input: Input) -> Result<Output, Error>;
}
```

`Layer<S>` 则负责把一个 service 包装成另一个 service。

关键价值不是 trait 本身，而是所有层都能使用同一种组合方式：

```text
TCP listener
  → connection observation
  → protocol peek
  → TLS ClientHello peek
  → MITM / passthrough policy
  → TLS relay
  → HTTP/1 or HTTP/2 relay
  → WebSocket upgrade relay
  → body capture / decompression / mutation / HAR
  → egress client
```

参考：

- [`rama-core/src/service/svc.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-core/src/service/svc.rs)
- [`rama-core/src/layer/mod.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-core/src/layer/mod.rs)

### 3.2 Extensions 的连接/流状态继承

Rama 没有让所有协议共享一个大 Context struct，而是使用类型化 `Extensions`：

- TCP connection 有自己的 extensions
- TLS 在同一 connection 上叠加
- HTTP/2 stream 从 connection extensions `fork()`
- Response 从 Request `fork()`
- Retry attempt 从原 Request `fork()`
- Ingress/Egress 状态分别可寻址

参考：[`rama-core/src/extensions.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-core/src/extensions.rs)

这个 parent/fork 模型很好地表达：

```text
Connection state
  └── TLS state
      └── HTTP/2 stream state
          ├── Request attempt state
          └── Response state
```

它对 RelayCore 规划中的 Connection/Stream/Message 模型很有参考价值。

### 3.3 有预算的协议 Peek

Rama 把协议识别做成可组合 `PeekRouter`，具备：

- max peek bytes
- read chunk size
- timeout
- fail-open/fail-closed
- Match/Rejected/Eof/ReadError/Timeout/AttemptsExhausted/MaxSize 等停止原因
- 完整 replay 已读取 prefix
- HTTP/1、H2 与非 HTTP fallback

参考：

- [`rama-core/src/io/peek.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-core/src/io/peek.rs)
- [`rama-net/src/http/server/peek.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-net/src/http/server/peek.rs)

相比“读几个字节判断 TLS/HTTP，否则 unknown”，这是可观测、可配置、可安全降级的协议识别边界。

### 3.4 真正的 HTTP/1 与 HTTP/2 Relay

`HttpMitmRelay` 不是把 H2 请求统一降级到 H1，而是维护独立 relay state：

- H1 ingress → H1 egress state
- H2 ingress → H2 egress state
- upstream H2 eager handshake
- 将关键 upstream SETTINGS 投影到 ingress
- H2 多 streams 不持有共享 mutex 执行请求
- 区分 stream-scoped RST 与 connection-scoped GOAWAY/transport error
- stream reset 不关闭 sibling streams
- Extended CONNECT capability 按 upstream 支持传播

参考：[`rama-http-backend/src/proxy/mitm.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-http-backend/src/proxy/mitm.rs)

这是 RelayCore 做完整 HTTP/2 时最有价值的外部参考。

### 3.5 ClientHello 驱动的 TLS MITM

Rama 的 BoringSSL MITM 路径会：

- 先 peek ingress ClientHello
- 提取 SNI、ALPN、TLS fingerprint 信息
- 根据 connector target 与 SNI 准备 egress
- 镜像兼容的 ClientHello 参数
- 正常 verification/custom trust/pinning
- 根据 upstream certificate 异步签发 leaf certificate
- 支持 cert issuer cache、deny/static/in-memory issuer
- 保存 ingress/egress extensions
- 无 ClientHello 时显式告警并降级

参考：

- [`rama-tls-boring/src/proxy/mitm/service.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-tls-boring/src/proxy/mitm/service.rs)
- [`rama-tls-boring/src/proxy/mitm/issuer`](https://github.com/plabayo/rama/tree/da13780305c4a5251d3a76d18a8dc085481d736f/rama-tls-boring/src/proxy/mitm/issuer)

它解决了透明代理/WireGuard 模式下“只有 destination IP、但证书必须匹配 SNI domain”的核心问题。

### 3.6 WebSocket 作为 message bridge

Rama 将 WebSocket 处理拆成：

```text
raw BridgeIo
  → WebSocket protocol upgrade
  → WebSocketBridge<Ingress, Egress>
  → per-direction message middleware
  → relay
```

具备：

- message-level middleware
- ping/pong/close 策略
- coordinated close timeout
- control event 可观察版本
- per-direction cancellation safety
- 外部 message injection
- injection queue capacity
- max injected message size
- permessage-deflate 协商 metadata 传递

参考：

- [`rama-ws/src/handshake/mitm.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-ws/src/handshake/mitm.rs)
- [`rama-http/src/layer/upgrade/mitm/svc.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-http/src/layer/upgrade/mitm/svc.rs)

这比在 HTTP handler 中手写两个 tungstenite pump 更适合长期扩展。

### 3.7 Streaming Body Capture Sink

Rama 的 `CaptureBody`：

- 原始 frame 继续传递
- data 和 trailers 都作为 event
- sink future 在 frame 下游可见前执行
- sink 可选择 backpressure、bounded queue 或立即返回
- 一次最多保留一个 frame 和一个引用计数副本
- 无默认 whole-body buffer
- 区分 Complete/Error/Aborted

参考：[`rama-http-types/src/body/capture.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-http-types/src/body/capture.rs)

这与 RelayCore 的 `TapBody` 目标类似，但生命周期、trailers、backpressure 和 abort 语义更完整。

### 3.8 新 CLI Inspector 的 Capture Store

Rama 的 MITM GUI Inspector 是近期新增能力，不应视为长期成熟产品；但 Capture Store 设计值得参考：

- Connection 与 Exchange 分离
- `connection_id` / `exchange_id`
- body records 与 metadata records 分离
- streaming frame capture
- request/response trailers
- WebSocket message record
- per-exchange Body limit
- global total storage budget
- max connections/exchanges/messages
- FIFO retention
- selected entries retention pinning
- encrypted temporary record file
- metadata snapshot 不解密/加载整个 Body
- body 按需流式解密读取
- 捕获暂停是 writer quiescence boundary

参考：

- [`rama-cli/src/cmd/serve/proxy/capture.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-cli/src/cmd/serve/proxy/capture.rs)
- [`rama-cli/src/cmd/serve/proxy/capture/model.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-cli/src/cmd/serve/proxy/capture/model.rs)
- [`rama-cli/src/cmd/serve/proxy/capture/service.rs`](https://github.com/plabayo/rama/blob/da13780305c4a5251d3a76d18a8dc085481d736f/rama-cli/src/cmd/serve/proxy/capture/service.rs)

### 3.9 验证体系

Rama 的验证体系远超普通 Rust 代理项目：

- 6800+ Rust test attributes
- H1/H2 protocol tests
- H2 RST/GOAWAY/SETTINGS regression
- WebSocket Autobahn server/client
- 18 个 fuzz targets
- URI/HTTP/H2/JSON/HTML/DNS 等 fuzz
- Loom concurrency tests
- Miri 条件测试
- benchmarks
- MSRV + stable
- Linux/macOS/Windows x64/ARM
- Android/iOS cross-target
- Apple Network Extension Swift/FFI E2E
- OCSP relay gate
- cargo-vet supply-chain audits
- GitHub Actions 固定 commit SHA

这部分最值得 RelayCore长期学习。

---

## 4. Rama 不适合直接照搬的部分

### 4.1 过度通用的 Service/Layer 类型系统

Rama 为 TCP、UDP、TLS、HTTP、gRPC、DNS、FastCGI、PAC、Apple FFI 等共享一套框架，导致：

- 极长泛型类型
- 大量 associated type bounds
- 编译错误复杂
- 需要 boxing/ArcLayer/MapOutput 等辅助层
- 学习和维护门槛高

RelayCore 不需要成为网络服务通用框架，只需在内部形成有限的 pipeline stage 和 mutation contract。

### 4.2 40+ crates 的拆分规模

Rama workspace 包含大量协议和平台 crate。RelayCore 当前不应按协议拆成几十个 crate，否则会增加：

- 发布和版本同步成本
- 编译时间
- API 稳定负担
- 跨 crate 重构摩擦

### 4.3 Extensions parent/fork 的全部复杂度

Extensions 的思想值得学习，但完整实现包含：

- parent chain
- ingress/egress wrapper traversal
- append-only type map
- newest-wins lookup
- extension grafting

Rama 自己的注释已记录错误 graft 可能形成 self-reference 并 stack overflow。RelayCore 应采用更窄、显式的 ConnectionContext/FlowContext/MessageContext，不复制完整 typemap 图。

### 4.4 BoringSSL ClientHello 指纹镜像

Rama 的 TLS MITM 深度依赖 BoringSSL，可模拟较完整 client fingerprint。RelayCore 当前使用 Rustls：

- 不应为追求浏览器指纹完全仿真立即切换 TLS 栈
- 不应引入 BoringSSL FFI 和巨大构建复杂度
- 近期只需 SNI/ALPN/passthrough/upstream trust/mTLS 等核心策略

### 4.5 自有 HTTP stack 的维护成本

Rama 已承担 HTTP internals、H2、header/URI/body 等大量底层维护。RelayCore 没有必要 fork 或重建 Hyper 协议栈，应尽量在 Hyper/H2 的稳定抽象上完成 relay。

### 4.6 新 CLI Inspector 不能视为成熟竞品产品

Rama GUI inspector 刚合入，示例 MITM proxy 也明确标注“not production ready”。其 Capture Store 设计可学习，但不能用 UI 功能表判断 Rama 已经替代 mitmproxy。

### 4.7 单一核心维护者风险与 0.x API

Rama 虽有多个贡献者，但主要维护高度集中；当前仍为 0.4.x，MSRV 1.96，API 变化和升级成本不可忽略。

---

## 5. 统一学习方向

### 5.1 方向一：建立 RelayCore 明确的处理平面

### 来源

- Proxelar：修改后的 Request/Response 真正重建到线路
- Rama：Layer 包装 Service，协议 relay 与 middleware 分离

### RelayCore 应做

定义有限而明确的内部 pipeline：

```text
Connection Accept
  → Protocol Detect
  → TLS Policy
  → Request Headers
  → Body Plan
  → Request Body
  → Upstream Relay
  → Response Headers
  → Response Body
  → Upgrade / Message Relay
  → Completion
```

每个 stage 只返回：

- Continue
- Mutation
- ShortCircuit
- Drop
- Inspect
- Error

不要继续依赖“修改 Flow 后主循环猜测哪些字段需要同步”。

### 优先级

**M0 / 立即**

---

### 5.2 方向二：BodyPlan + Streaming Capture Sink

### 来源

- Proxelar：仅在 Script/Intercept 需要时 Buffer；超限后重放 prefix + remaining stream
- Rama：frame-level CaptureSink、trailers、backpressure、Complete/Error/Aborted

### RelayCore 应做

```rust
enum BodyPlan {
    PassThrough,
    Capture { limit: usize },
    Buffer { limit: usize },
    TransformStream,
    Replace(BodyData),
    Reject,
}
```

并把 `TapBody` 演进为：

- data/trailers frame 均可观察
- 捕获策略显式选择 backpressure 或 drop
- abort/error/complete 区分
- 无 Body rule 时绝不 whole-buffer
- Script/Rule 需要完整 Body 时预算内 Buffer
- 超限后不允许用“修改 prefix”静默替换完整 Body

### 优先级

**M0 / 立即**

---

### 5.3 方向三：Connection / Flow / Message 分层

### 来源

- Rama Extensions fork：connection → H2 stream → request/response
- Rama Capture Store：ConnectionId / ExchangeId / records
- Proxelar 的局限：TrafficSession 以平铺 Vec 保存完整 snapshot

### RelayCore 应做

近期以兼容字段开始：

```text
Connection
  id / endpoints / transport / TLS / lifecycle

Flow or Stream
  id / connection_id / parent_flow_id / stream_id / HTTP exchange

Message
  id / flow_id / direction / type / timestamp / payload ref
```

不要一次替换现有 Flow JSON，但新事件和存储应以该层级设计。

### 优先级

**M0 / 近期架构关口**

---

### 5.4 方向四：有预算、有结果的协议识别

### 来源

Rama `PeekRouter`。

### RelayCore 应做

将协议识别从局部字节判断升级为统一组件：

```rust
struct PeekPolicy {
    timeout: Duration,
    max_bytes: usize,
    read_chunk_size: usize,
    timeout_policy: FailOpen | FailClosed,
}

enum PeekStopReason {
    Matched,
    Rejected,
    Eof,
    ReadError,
    Timeout,
    MaxBytes,
}
```

支持：

- HTTP/1
- H2 preface
- TLS ClientHello
- Unknown/raw fallback
- prefix replay
- 结构化 metrics 和 audit

### 优先级

**M0/M1**

---

### 5.5 方向五：完整 HTTP/2 Relay 语义

### 来源

Rama `HttpMitmRelay`。

### RelayCore 应学习

- ingress/egress version 独立但显式关联
- H2 egress connection 共享
- 不用大 mutex 串行 streams
- stream-scoped RST 不关闭 connection
- GOAWAY/transport error 才关闭 connection
- SETTINGS 中可跨代理传播的能力与仅本方向预算分开
- Extended CONNECT capability 不可凭空声明
- upstream H1/H2 translation 明确建模

### 不需要复制

- Rama 自有 HTTP stack
- 完整 SETTINGS 镜像实现细节

### 优先级

**M1 / 未来数月重点**

---

### 5.6 方向六：TLS Policy 与 ClientHello

### 来源

- Rama ClientHello-aware TLS relay
- Proxelar WireGuard HTTPS 的 SNI 缺失反例

### RelayCore 应做

```text
ClientHello peek
  → SNI/ALPN
  → MITM policy: inspect / passthrough / drop
  → upstream trust policy
  → leaf certificate subject
  → HTTP protocol selection
```

近期覆盖：

- SNI
- ALPN
- passthrough/ignore hosts
- native/custom/insecure upstream trust
- IP/DNS SAN
- handshake timing/error

长期再考虑：

- mTLS client cert
- fingerprint metadata
- key log
- pinning diagnostics

### 优先级

**M0/M1**

---

### 5.7 方向七：WebSocket Message Bridge

### 来源

Rama `WebSocketBridge` 与 injection/close policy。

### RelayCore 应做

- raw upgrade 与 message relay 分离
- message middleware 按方向执行
- text/binary 与 control message 生命周期明确
- coordinated close timeout
- injection queue capacity
- max injected message size
- compression extension策略
- message history 与 relay 生命周期解耦

### 优先级

**M1**

---

### 5.8 方向八：Content Codec 独立于 UI

### 来源

Proxelar content view。

### RelayCore 应做

先建立最小 registry：

```rust
trait ContentCodec {
    fn detect(metadata, bytes) -> Confidence;
    fn decode(bytes) -> SemanticDocument;
    fn encode(document) -> Bytes;
}
```

近期：

- gzip/br/deflate/zstd
- charset
- JSON/XML/form/multipart
- raw/text/hex/base64

长期：

- SSE
- gRPC framing
- descriptor-aware Protobuf
- MessagePack/CBOR
- Socket.IO

### 原则

- codec failure 不影响 traffic
- 解压后预算
- renderer 留在 adapter/UI
- 不照搬 Proxelar 手写所有格式到一个大文件

### 优先级

**M1**

---

### 5.9 方向九：记录存储采用 Metadata + Record Journal

### 来源

- Rama Inspector 的 encrypted temporary record files 与 budget
- Proxelar in-memory SessionRecorder 的反例

### RelayCore 应做

保持 SQLite 作为索引和权威 metadata，同时演进：

```text
flow/connection summary → SQLite
body/message records     → bounded blob/file store
record locations         → SQLite
live updates              → typed event stream
```

关键能力：

- per-body limit
- global body budget
- max flows/messages
- retention/eviction
- selected/pinned entries
- metadata query 不加载 Body
- Body 按需流式读取
- crash-safe index/blob coordination
- trailers 和 completion outcome

不必复制 Rama 的临时文件加密格式，但可学习 record journal 和 budget 模型。

### 优先级

**M1**

---

### 5.10 方向十：验证体系升级

### 来源

- Proxelar：高覆盖率和多平台 blocking gate
- Rama：协议规范、fuzz、Autobahn、Loom、平台 E2E、cargo-vet

### RelayCore 分阶段推进

#### 近期必须

- Stable Action wire-level matrix
- H1/H2/WebSocket/TLS differential fixtures
- 修复 benchmark harness 假阳性
- 2h soak
- parser/rule/body-codec fuzz
- macOS/Windows 周期测试

#### 中期

- WebSocket Autobahn
- H2 protocol/reset/GOAWAY suite
- Loom/并发模型用于 actor/channel 热点
- Apple/Linux transparent E2E
- cargo-audit/deny

#### 长期

- cargo-vet
- 24h/7d soak
- dedicated performance runners
- platform capture end-to-end

### 优先级

**M0/M1**

---

### 5.11 方向十一：公开 Stable Surface 与 Limitations

### 来源

Proxelar 的 README/ROADMAP/limitations。

### RelayCore 应做

每项公开能力标记：

- Stable
- Experimental
- Preview
- Planned

并明确：

- HTTP/2 的具体方向和转换语义
- QUIC downgrade 与 MITM 的区别
- transparent 平台矩阵
- Body budget 和截断
- Script hook 与权限
- Storage retention
- WebSocket replay/inject 限制

避免模型中存在 enum 就宣传为已支持。

### 优先级

**M0 / 文档和 API 治理**

---

## 6. 不应进入近期路线的学习项

以下即使 Rama/Proxelar 已有，也不应改变 RelayCore 当前重点：

- WireGuard capture
- 完整 SOCKS5/UDP ASSOCIATE
- DNS proxy
- Windows WFP driver
- Apple Network Extension 产品化
- BoringSSL ClientHello 完整 fingerprint mirror
- User-Agent/TLS 指纹模拟
- 自有 HTTP stack
- 40+ crate 协议框架
- Protobuf wire editor
- 全协议 content view
- 加密临时 capture 文件
- 远程多租户 proxy control plane

这些属于可演进能力或特定产品需求，不应挤占 Mutation Pipeline、Body、H2、TLS 和稳定性工作。

---

## 7. 与现有路线图的映射

| 现有路线图方向 | Proxelar 学习 | Rama 学习 | 建议 |
|---|---|---|---|
| Mutation Pipeline | 真实 Request/Response 重建 | Service/Layer stage | M0 |
| BodyPlan | 条件 Buffer/Streaming | CaptureBody/Sink/Outcome | M0 |
| Connection/Stream/Message | 反例：平铺 Session | Extensions fork + Capture IDs | M0 |
| HTTP/2 | 反例：H2→H1 | H2 relay/settings/reset | M1 |
| WebSocket | 基础 frame pump | message bridge/injection/close | M1 |
| TLS | upstream trust policy | ClientHello/SNI/ALPN/MITM policy | M0/M1 |
| Content Codec | 压缩与 views | streaming compression layers | M1 |
| Storage | 反例：unbounded Vec | bounded record journal | M1 |
| Reverse mode | 固定 target 的最小产品语义 | listener/relay 组合边界 | M1 |
| SOCKS/DNS/WireGuard 等模式 | 功能面参考 | transport/protocol boundary | E |
| Testing | coverage/multi-OS | fuzz/spec/Autobahn/Loom | M0/M1 |
| RelayCraft | 产品回归宿主 | 不相关 | 持续约束 |
| MCP/AI | 无明显参考 | structured extensions/telemetry | RelayCore 自有差异化 |

---

## 8. 推荐实施顺序

### 第一阶段：吸收两者最关键经验

1. Action wire-level matrix
2. 显式 Request/Response Mutation
3. BodyPlan
4. TapBody → CaptureSink/Outcome
5. framing + content encoding
6. Stable/Experimental 能力治理
7. benchmark harness 修复

### 第二阶段：吸收 Rama 的协议边界

1. Connection/Flow/Message IDs
2. protocol peek policy
3. ClientHello/SNI/ALPN
4. TLS passthrough policy
5. H2 stream-scoped relay
6. WebSocket bridge/injection/close

### 第三阶段：吸收存储和语义层经验

1. metadata/body 分离
2. total/per-flow budget
3. retention/pinning
4. content codec registry
5. HAR timing/fidelity
6. differential/fuzz/soak

### 后续阶段

在核心处理平面稳定后按 L7 路线实现 Reverse mode；SOCKS、DNS、Local Capture、WireGuard 等模式再根据 RelayCraft 和独立用户需求选择，不按竞品功能表逐项追赶。

---

## 9. 最终结论

Proxelar 与 Rama 代表两个极端：

```text
Proxelar
  小、快、面向用户、功能面宽
  但协议深度和 runtime 承载能力有限

Rama
  深、广、协议和平台基础强
  但复杂度远超一个 L7-first 产品引擎所需
```

RelayCore 应选择中间路径：

> 保留产品明确性和多宿主 runtime，以 Rama 级别的协议边界和验证标准打磨核心 L7 数据面，同时采用 Proxelar 的交付意识和内容处理经验。

近期最重要的学习不是增加六种 mode，而是：

1. Proxelar 证明 Mutation 必须真正作用于 wire。
2. Rama 证明 Connection/Protocol/Middleware 必须有清晰边界。
3. 两者共同证明 Body capture、content encoding、测试和限制说明不能后补。
4. RelayCore 的 MCP、审计、规则和 RelayCraft 生态仍是自己的差异化，应建立在可信数据面之上。
