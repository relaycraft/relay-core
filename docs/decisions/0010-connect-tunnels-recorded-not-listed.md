# 0010 · CONNECT 隧道：记录，但不默认进请求列表

- **Status**: Accepted
- **Date**: 2026-10-11
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§4-1、§4 验收标准）、
  [`../engine-capability-status.md`](../engine-capability-status.md)（§4 关系字段）

## 背景

路线图 §4 的验收标准之一是「CONNECT 外层与内层 Flow 可关联」。动手时发现一个前提：
**当时引擎根本不产生 CONNECT 的 Flow** —— `handle_request` 收到 CONNECT 后直接升级连接、
开隧道，只有隧道**内部**的请求成为 Flow。因此要满足这一条，必须新产生一条外层 Flow，
而这会改变用户看到的东西：HTTPS 浏览时列表里会多出一行（每个目的地一行）。

同类工具的做法并不一致，这正是本节要记录的分歧：

| 工具 | CONNECT / 隧道 | 证据 |
|---|---|---|
| **mitmproxy** | 会建一条 `HTTPFlow` 并抛 `HttpConnectHook`，但该 hook 文档明确写「CONNECT 是给 mitmproxy 自身的代理指令，**不产生通常的 HTTP handler 事件**，对绝大多数用途可忽略」 | `mitmproxy/proxy/layers/http/_hooks.py` 的 `HttpConnectHook` 文档串；`__init__.py` 里 `yield HttpConnectHook(self.flow)` |
| **Proxyman** | 不作为一个列表行；连接细节放在每条请求的 **Connection Log** 面板（DNS/TCP 尝试/ALPN 提议与协商/TLS 版本与 cipher/证书/**连接是否被复用**） | <https://docs.proxyman.com/basic-features/connection-log> |
| **Fiddler** | **当作普通行显示**（`Tunnel to host:443`），而用户正在问怎么关掉它 | [SO 30192738](https://stackoverflow.com/questions/30192738)、[CSDN](https://blog.csdn.net/Adimstar/article/details/147321115) |

同时确认：**没有任何应用层代理暴露线路 stream id**（mitmproxy 的 `Flow` 没有、HAR 1.2 没有、
Envoy access log 没有）；它只存在于数据包层（tshark 的 `http2.streamid`，「用于 HTTP/2 与 QUIC，
因为它们多路复用」）。hyper 1.10.1 也不提供（`src/ext/` 只有 `h1_reason_phrase` 与 `informational`；
h2 server 只往 request extensions 塞 `Protocol`）。

## 决策

1. **记录**：客户端每发一个 CONNECT，就产生一条 Flow。它是隧道内所有 Flow 的
   `parent_flow_id` 指向的对象，并承载隧道自己的起止时间与结束原因。
2. **打标签**：该 Flow 带 `tags: ["connect"]`（常量 `relay_core_api::flow::CONNECT_TAG`）。
3. **默认不进列表**：`FlowQuery::include_connect` 默认为 `false`，`flow_matches_query` 与
   SQLite 查询（`flow_summaries.is_connect`）都在这一处口径下过滤。
   显式把 `method` 写成 `CONNECT` 也算请求 —— 一个要 CONNECT 却拿到空列表的过滤器，
   是在自相矛盾。
4. **不发明 `stream_id`**：§4-1 列出的四个字段里，`connection_id` / `parent_flow_id` /
   `protocol_stack` 本次交付；`stream_id` **不交付**，并把它连同上面 hyper 的证据写进 §4，
   降级为「可演进」，避免下一个人重新调研一遍。

## 理由

- **记录与呈现是两件事。** 「引擎知道隧道存在」和「请求列表里出现隧道」可以独立决定，
  而标签正是这个开关的键：引擎记录事实，宿主决定呈现。这也让 §4 的验收标准
  「CONNECT 外层与内层可关联」成立，同时不把噪声塞进默认视图。
- **两处口径必须一致。** 内存过滤与 SQL 过滤若不一致，从 SQLite 载入的一页会出现实时列表
  隐藏的行 —— 这正是保留策略那一天修掉的同一类漂移（flows 与 summaries 两表对齐）。
  因此 `is_connect` 是**列**而不是查询后再过滤：先分页后过滤，页大小与「看到几行」会不一致。
- **迁移必须能面对「版本号在说谎」。** 迁移 2 用 `pragma_table_info` 判列是否存在再 `ALTER`，
  因为测试里就有一个把 `user_version` 重置为 0 的用例；只信版本号的迁移会把一次陈旧的
  版本戳变成硬失败。回填则从已存的 summary JSON 读 `tags`（`json_each`），而不是 `LIKE '%connect%'`
  —— 后者会误伤 URL 里带 connect 的行。

## 影响

- **正面**：HTTPS 流量在列表里保持「一行一个请求」，与 mitmproxy/Proxyman 一致；
  隧道仍可查（`include_connect=true`、`--include-connect`、Web UI 设置开关），
  并且首次有了隧道的时长与结束原因。
- **代价**：
  - 存储量上升：每个 CONNECT 多一条 summary + 一条 flow（可被 retention 裁掉）。
  - `flow_summaries` 增加一列并引入 **schema v2**；写入旧库的隧道由迁移回填。
  - Web UI 需要在 SSE 入口处一起过滤，否则实时到达的隧道会绕过列表口径；
    「重新显示」需要一次重新拉取（行已不在客户端），因此设置开关会触发一次 `fullSync`。
  - 既有消费者需要按标签过滤：`test_https_mitm_h2` 就因此改为跳过 CONNECT 并**断言它恰好出现一次**。

## 相关证据

- 决策/模型：`relay-core-api/src/flow.rs`（`connection_id` / `parent_flow_id` / `protocol_stack`、
  `CONNECT_TAG`、`http_protocol_stack`）、`relay-core-api/src/flow_query.rs`（`wants_connect`）
- 生产：`relay-core-lib/src/proxy/http.rs`（`connect_flow`）、`server.rs`（每次 accept 一个 id +
  真实 `flows_count`）、`tunnel.rs` / `websocket.rs`（origin 透传）
- 存储：`relay-core-storage/src/store.rs`（`SCHEMA_VERSION = 2`、迁移 2、`is_connect` 列与过滤）
- 契约：`relay-core-lib/tests/flow_relationships.rs`（5 个）、
  `relay-core-api/src/flow_query.rs`（3 个）、`relay-core-storage/tests/store_tests.rs`（2 个）、
  `webui/src/lib/tunnels.test.ts`（6 个）
