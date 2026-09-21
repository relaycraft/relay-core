# relay-core MCP / Script wire gaps — 优化交接说明

> **状态（2026-09-22）**：实现会话已按评估落地，本文只保留当时的现象记录。  
> `onResponseHeaders` 返回 flow 后继续转发上游 body；async hook 会推动 event loop，body 在代理侧缓冲后再交给脚本，超时的 hook 会放行并打 `script-error`。  
> `request_headers` / `response_headers` 仍是整表替换；加头用 `request_header_upserts` / `response_header_upserts`。`patch_policy` 拒绝未知字段。  
> **验证环境**：daemon `0.13.0`，proxy `127.0.0.1:8080`，目标 `httpbin.org`，MCP `user-relay-core`。  
> **权威对齐**：修复后应对照 [`l7-first-engine-evolution-roadmap.md`](./l7-first-engine-evolution-roadmap.md) §22 / §24；未过 wire 断言前不得标 Stable、不得当可用能力宣传。  
> **完整验证表**：Cursor Canvas `relay-core-mcp-usage-report`（同会话产物）。

---

## 0. 一句话结论

观测 / 规则 mock / HAR / replay / 代理启停 **可用**。  
`set_script` 与部分 MCP 语义（intercept resume 改头、`patch_policy`）**默认用法就会踩雷**，需要按下面优先级修，修完用本文 §5 回归清单再验。

---

## 1. P0 — 脚本：`onResponseHeaders` 返回 flow 会空 body

### 现象

脚本里：

```js
globalThis.onResponseHeaders = (ctx, flow) => {
  flow.layer.data.response.headers.push(["X-Relay-Script", "headers-ok"]);
  return flow;
};
```

经代理访问 `https://httpbin.org/json`：

- 客户端能看到 `x-relay-script: headers-ok`
- **body 为空**，且常无 `content-length`
- 无脚本 / noop 脚本时同 URL 正常返回 429 字节 JSON

### 根因（代码）

1. `ScriptInterceptor::on_response_headers` 在脚本返回 `Some(flow)` 时映射为  
   `InterceptionResult::ModifiedResponse(response.clone())`  
   — `relay-core-script/src/lib.rs`（约 328–340 行）
2. `handle_http_request` 对 `ModifiedResponse` **直接** `return Ok(mock_to_response(resp))`，  
   **跳过**后续 body 流式阶段  
   — `relay-core-lib/src/proxy/http.rs`（约 570–578 行）
3. Headers 阶段 `response.body` 通常仍是 `None` → `mock_to_response` 造出「有头无体」的响应

规则侧早已按「Flow 为单一事实来源 + Continue 继续传 body」修过（roadmap §24.1）；**脚本路径仍走旧的 ModifiedResponse 短路语义**。

### 建议修复方向

- **语义**：`onResponseHeaders` 返回修改后的 flow → 写回 `*flow` 后返回 **`InterceptionResult::Continue`**（与「只改头、继续上游 body」一致）。
- **整段替换响应**（mock）：应显式走 `MockResponse` / 专用 API，或仅当脚本设置了完整 `response.body` 时才 `ModifiedResponse`。
- 同步检查 `onRequestHeaders` → `ModifiedRequest` 是否有对称坑（请求方向是否误短路）。

### 验收（必须 wire）

新增类似 `wire_matrix` 的用例（真实 client + 真实/本地上游）：

1. 脚本只在 `onResponseHeaders` 加响应头 → 客户端 **既有新头又有完整上游 body**
2. 无脚本对照组 body 字节一致（除 framing 合法差异）

---

## 2. P0 — 脚本：`async onResponse` / `body.json()` 卡死引擎

### 现象

- `globalThis.onResponse = async (body, flow) => { ... }` 即使立刻 return，经代理请求也会卡住。
- `await body.json()` / `body.text()` 会卡住；Deno **单通道**队列堵死后，后续 `set_script` 全部 MCP timeout，只能 `relay shutdown` + `start`。
- **同步** `function (body, flow) { ...; return flow; }` 且 **不读** upstream body、直接写 `response.body` 替换 → **线路上成功**（已验证）。

### 根因线索

- Deno worker 按 `DenoCommand` **串行**处理（`relay-core-script/src/deno_engine.rs` `while let Some(cmd) = rx.recv()`）。
- Body 经 `HttpBodyResource` + `op_read_body`；与 `TapBody` 流式路径叠加时，`async` + `resolve()` / 读流易挂死整队列。
- 单元测多用内存 `Full` body，**盖不住**这条线上问题。

### 建议修复方向

1. **短期（文档 + 模板）**  
   - CLI `scripts` 模板、`set_script` tool description、README：明确推荐 **同步** `onResponse`；警告 `async` / `body.json()` 在流式路径上未就绪。  
2. **中期（引擎）**  
   - 保证 pass-through 的 `async` hook（不读 body）不会 hang（修 `resolve` / event loop 与 body resource 生命周期）。  
   - 读 body：在进入 script 前 **buffer 到预算内**（对齐 `rule_body_inspect_budget` / body observation），再把完整字节交给 JS；或提供显式 `body.buffer()` 且有超时。  
3. **硬超时**：单次 hook / `LoadScript` 排队超时，失败时打 `script-error` tag + 放行/502，**禁止**永久占住队列。

### 验收

| 用例 | 期望 |
|------|------|
| sync `onResponse` 替换 body | 客户端收到新 JSON + 新头；flow tag 可见 |
| async `onResponse` 空实现 | **不得 hang**；应在超时内完成或明确报错 |
| `await body.json()` 改字段再写回 | 不卡死引擎；后续 `set_script` 仍可用 |
| 故意慢/挂的 hook | 超时后引擎仍能 `LoadScript` |

---

## 3. P1 — `resume_flow` 的 `request_headers` 是整表替换

### 现象

`resume_flow` 传入 `request_headers: { "X-Relay-Intercept": "resumed" }` 后，flow 里请求头 **只剩这一条**，原 `User-Agent` / `Accept` 丢失。

### 根因

`apply_flow_modification`：

```text
req.headers = h.into_iter().collect();  // replace
```

— `relay-core-runtime/src/modification.rs`（约 36–38 行）

### 建议

- **默认改为 merge**（同名覆盖，其余保留），或  
- 保留 replace，但 MCP schema / 文档写明，并增加 `request_headers_mode: "merge" | "replace"`。

### 验收

Intercept → resume 只加一个自定义头 → 上游 / flow 中仍保留 curl 默认头 + 新头。

---

## 4. P1 — `patch_policy` 静默忽略未知字段

### 现象

- `patch_policy({ request_timeout_ms: 35000 })` → ack `ok`，`get_policy` 仍为 `30000`。
- `patch_policy({ redaction: { enabled: false } })` → **生效**（可逆已验）。

### 根因

`ProxyPolicyPatch` **仅**含 `redaction` / `upstream`（`relay-core-api/src/policy.rs`）。  
serde 默认丢弃未知字段 → MCP 返回成功造成假阳性。

### 建议

1. MCP / HTTP：`deny_unknown_fields` 或手动校验后 **400 + 明确错误**；或  
2. 扩展 `ProxyPolicyPatch`（至少 `request_timeout_ms`、`body_observation` 等常用项）；  
3. Tool description 列出 **可 patch 字段白名单**。

### 验收

未知字段 → 错误且值不变；白名单字段 → `get_policy` 立刻反映。

---

## 5. 回归验证清单（修完后回本会话跑）

用 MCP（或等价 HTTP control API）+ `curl -x http://127.0.0.1:8080 --proxy-insecure`：

```text
[ ] proxy_status / stop / start：历史仍在，端口正确
[ ] mock_url httpbin.org/uuid → 非 200 假 body；delete_rule 后恢复
[ ] set_rule AddResponseHeader → 响应头上线
[ ] set_intercept → get_pending_intercepts → resume（merge 头）→ 200
[ ] export_har / replay_flow 仍绿
[ ] set_script：
      [ ] 仅 onResponseHeaders 加头 → body 完整（P0）
      [ ] sync onResponse 替换 body → 客户端可见
      [ ] async 空 onResponse → 不 hang
      [ ] body.json() 改写 → 不卡死；事后还能 set_script
[ ] patch_policy：非法字段失败；redaction 可逆
```

**对照目标**：`https://httpbin.org/json`、`/uuid`、`/headers`、`/ip`。

---

## 6. 建议落地顺序（给实现会话）

| 顺序 | 项 | 主要改动面 | 测试 |
|------|----|------------|------|
| 1 | P0 onResponseHeaders Continue | `relay-core-script` + 必要时 `http.rs` 语义澄清 | **新 wire 测** |
| 2 | P0 script hang / timeout | `deno_engine.rs` + body resource | wire + 回归 set_script |
| 3 | P1 resume header merge | `modification.rs` + probe schema | intercept e2e |
| 4 | P1 patch_policy 校验/扩展 | `policy.rs` + probe/http | API 单测 |
| 5 | 文档 | MCP README、`scripts` 模板、roadmap §24 增「脚本失真」条 | — |

原则：**先写失败的 wire/集成测，再改代码**（仓库 TDD / Offline-First）。规则侧已有 `wire_matrix_*` 可作模板；脚本目前缺对等覆盖。

---

## 7. 实现时不要做的事

- 不要只加单元测（`Full` body）就标 Fixed。  
- 不要在未修 hang 的情况下在文档里鼓励 `async` + `body.json()`。  
- 不要把「脚本能 load」当成「线路可用」。  
- daemon restart 会使 MCP session 失效（`Session not found`）——回归时注意重连 / `mcp_auth`。

---

## 8. 相关文件速查

| 区域 | 路径 |
|------|------|
| Script → ModifiedResponse | `relay-core-script/src/lib.rs` |
| ModifiedResponse 短路 | `relay-core-lib/src/proxy/http.rs` |
| Deno 队列 / onResponse | `relay-core-script/src/deno_engine.rs` |
| Body resource | `relay-core-script/src/streams.rs` |
| resume 改头 | `relay-core-runtime/src/modification.rs` |
| Policy patch 形状 | `relay-core-api/src/policy.rs` |
| MCP set_script | `relay-core-probe/src/tools/script.rs` |
| MCP intercept | `relay-core-probe/src/tools/intercept.rs` |
| 规则 wire 范例 | `relay-core-lib/tests/wire_matrix.rs` |

---

## 9. 交接话术（可直接贴给实现会话）

> 请按 `docs/mcp-script-wire-gaps-handoff.md` 修 P0/P1。  
> 核心：`onResponseHeaders` 返回 flow 不应再映射成会空 body 的 `ModifiedResponse`；脚本 async/读 body 不得卡死 Deno 队列。  
> 先补 wire 失败用例再改代码。修完用该文档 §5 清单回归；不要标 Stable 直到清单全绿。
