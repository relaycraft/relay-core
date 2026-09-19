# 0007 · 守护进程控制面（daemon control plane）

- **Status**: Accepted
- **Date**: 2026-09-19
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§17 Adapters、§18 MCP/AI-Native）、[`0003-typed-events-alongside-flow-snapshots.md`](./0003-typed-events-alongside-flow-snapshots.md)

## 背景

MCP 适配器（`relay-core-probe`）与 CLI 都把**适配器进程和引擎进程绑成了同一个进程**：

- `relay-core-probe` 启动时**无条件**调用 `CoreState::spawn_proxy`（默认 `127.0.0.1:8080`），
  不区分谁连、连了几个（`relay-core-probe/src/main.rs`）；
- 启代理失败只打一行 `Proxy start warning` 然后**继续服务**：第二个客户端连上来时 bind 失败，
  它的 15 个工具照常应答，只是永远返回空 flow 列表——**对 Agent 表现为"没有流量"，而不是"代理没运行"**；
- MCP 工具集中**没有任何生命周期工具**（`relay-core-probe/src/tools/mod.rs`），只有 15 个流量/规则类工具；
- CLI 命令集里**没有 start/stop/status**，`relay run` 前台阻塞、不 daemonize、无 pid 文件、无发现文件；
- 每个客户端各自持有一份 `CoreState`，于是 flows / rules / intercept / 断点队列 / 脚本**互不可见**。

结论：缺的不是引擎能力（`CoreState::spawn_proxy` / `stop_proxy` / `LifecycleManager` /
`RuntimeStatusService` 早已存在），而是**控制面**。同时这条路径**没有任何"关闭代理"的出口**：
用户只能 `pkill`，Agent 断连则连历史一起消失。

## 决策

### 1. 单一权威：全局单例守护进程

- 一个长驻**守护进程**独占唯一 `CoreState` 与**唯一的代理生命周期权威**。
- 作用域是**每用户全局单例**，位于 `RELAY_DATA_DIR`（默认 `~/.relay-core`）：
  跨项目、跨窗口共享同一份流量、规则与历史。
- 守护进程宿主复用 **`relay-core-cli` 二进制**（内部子命令 `daemon`），
  **不新增第三个发布二进制**——发布矩阵（`release.yml` / `publish-npm.yml` / `ci.yml` /
  7 个 `npm/packages/*`）保持两个产物不变。

### 2. 控制面 = 守护进程的 loopback HTTP API + 发现文件

- 控制面复用已有的 `relay-core-http` 适配器（flows / rules / intercepts / policy / scripts /
  events / metrics 路由已存在），**新增代理生命周期路由**；不新造 IPC 协议。
- 发现走 `$RELAY_DATA_DIR/daemon.json` 清单（pid、api_port、proxy_port、mcp_port、
  bearer token、engine version、started_at），权限 `0600`；配合 `daemon.lock` 保证单实例。
- **过期清单自愈**：pid 不存在或 health 探测失败即视为陈旧，删除后按"未运行"处理。

### 3. 连接界面永不启动引擎；代理开/关是一等命令

```bash
relay start      # 幂等：确保守护进程在跑（必要时 detached 拉起）+ 启动代理
relay stop       # 停代理，守护进程保留（规则/历史/会话仍在）
relay restart    # 重启代理
relay status     # daemon / proxy 相位 / 端口 / MCP 端点 / 日志路径
relay shutdown   # 停代理并退出守护进程
relay run        # 同一个守护进程，前台运行（含 --ui / --web / --mcp-port）
```

**守护进程自己不起代理**：它只服务控制面与 MCP 端点，代理的启动永远来自一次显式控制调用。
这一点不是风格问题——若守护进程在启动时"顺便"起代理，客户端紧接着发出的 `proxy_start` 就会与它
竞争，同一个 `relay start` 会时而返回 `started`、时而返回 `already_running`。确定性来自
"谁下令谁负责"。（`daemon --start-proxy` 作为显式选项保留，供希望一进程到底的宿主使用。）

**`relay run` 就是守护进程本身**，不是第二份实现。它写同一份清单、服务同一个控制面与 MCP 端点，
`--ui` 渲染的 TUI 是它的**客户端**（通过 `/api/v1/events` 读取流量），退出 UI 即结束前台守护进程。
旧的那套 :8081 legacy 控制 API（WebSocket 流 + 一对实际为空操作的 intercept pause/resume）
随之删除，`relay flows` 改为读同一个事件流，并区分"列出已捕获"（默认）与 `--follow`（实时）。

`relay-core-probe` 默认**只做协议桥**：stdin/stdout 的 MCP 消息转发到守护进程的 `/mcp` 端点。

- 桥不实现任何工具语义——工具只有一份实现（服务端），因此 `search_flows` 不可能在 HTTP 与
  stdio 两种接入下含义不同。
- 守护进程未运行时，桥按策略**自动拉起守护进程**（`RELAY_MCP_NO_AUTOSTART=1` 可关闭），
  但**不会自动启动代理**。
- 内嵌引擎模式（旧 `--embedded`）已**删除**：MCP 只有一种运行形态——桥 + 守护进程。
  少一条路径就少一种"两个客户端各自抓一半流量"的可能。

### 4. 代理自动关闭默认关闭（可配置）

- 代理**不因 MCP 客户端断连而消失**；`--idle-timeout` / `RELAY_PROXY_IDLE_TIMEOUT`
  是**显式配置项，默认 `0`（永不自动关闭）**。
- 因此关闭代理只有两种来源：用户显式命令（`relay stop` / `proxy_stop` 工具），
  或用户显式配置的空闲超时。
- 空闲以"捕获流量计数是否变化"衡量：**有流量经过的代理永远不会被超时关掉**。

### 5. MCP 工具补齐生命周期与结构化错误

- 新增 `proxy_status` / `proxy_start` / `proxy_stop`（带 `readOnlyHint` / `destructiveHint` /
  `idempotentHint` 注解），并排在工具列表最前。
- 流量类工具在**没有代理且没有任何历史**时返回结构化错误 `proxy_not_running`
  （错误 `data.code` 字段，含如何启动的指引），**绝不返回一个会被误读为"没有流量"的空列表**。
  为兼容"代理停了但仍要读历史"的用法，存在历史时照常返回历史。
- 启动失败（端口被占等）返回 `start_failed` 等稳定 code，而不是"启动成功但从未 listen"。

### 6. 配置进文件（flag > env > config > 默认）

`$RELAY_DATA_DIR/config.toml` 承载端口、MCP 开关、空闲超时、流文件与客户端自动拉起策略。
优先级是 **flag > 环境变量 > 配置文件 > 内置默认**，所以一次性的覆盖不需要改文件，而文件承载
"跨命令存活"的设置。

**配置写坏了会直接报错，绝不静默用默认值**：静默忽略一个 typo，等于让守护进程用使用者没要求的
设置运行——这正是本决策要消灭的那一类失败。

### 7. 生命周期归因（替代"会话登记"）

**评估结论：不做会话登记（session attach/detach/heartbeat）。** 本决策已定"默认绝不自动关代理"，
于是会话登记唯一剩下的价值是"现在有几个客户端连着"——而代价是所有客户端都要实现注册、心跳与
TTL 清理，其中 CLI 命令只活几毫秒，登记进去纯属噪声。

真正有用的问题是**"这个代理是谁开的、谁停的"**，而它的答案不该依赖提问时谁还连着。因此：

- `ProxyControlService` 的 start/stop 都接收一个 [`Requester`]（`actor` = 传输来源，
  `label` = 客户端自述，如 `cli:relay stop` / `mcp:proxy_start`）。
  **把它做成必填参数**，这样任何调用方都不能"忘了说是谁"而匿名改动生命周期。
- label 由客户端自述（HTTP 走 `X-Relay-Client` 头），**从不用于鉴权**——token 才是边界；
  它存在的唯一目的是让审计能回答"谁做的"。
- 审计新增 `AuditEventKind::ProxyLifecycleChanged`，记录 change / port / requested_by /
  outcome / 失败原因，落进既有的 audit 通道（HTTP `…/audit`、MCP resource、SSE）。
- `relay status` 显示 `last: stopped by cli:relay stop (2m ago)`；`--json` 里是 `last_change`。

顺带修掉一个真缺陷：审计查询在有 store 时**只读库**，而落库是异步的——于是"刚刚发生的生命周期
变化"会被读成"什么都没发生"。现在内存尾部与库内事件**合并去重**，任何时刻查询都能看到刚发生的事。

### 8. 控制面保持 loopback HTTP，不做 unix socket

**评估结论：不做。** 理由：

1. **TCP 端口绕不开**。浏览器 WebUI 与 HTTP 版 MCP 客户端只能走 TCP，所以"改用 socket"实际是
   **增加**第二套传输，而不是替换掉端口。
2. **安全上没有净收益**。威胁模型是"另一个本地用户"与"浏览器页面"：前者读不到 0600 的
   `daemon.json`，后者被 token 与 `SameSite=Strict` cookie 挡住。socket 只是把"文件权限即边界"
   从 token 文件换成 socket 文件。
3. 代价是每个客户端两套连接逻辑、两套认证，Windows 还要 named pipe。

但"浏览器连不上、`EventSource` 又不能设 header"暴露了一个**真问题**：守护进程恒有 token
之后，`relay run --web` 的界面会全线 401。修法是：

- 守护进程打印的 Web UI URL 把 token 放在 **URL fragment**（`…/#token=…`）。
  fragment 是浏览器唯一**不会发给服务器**的部分，因此 token 不会进请求日志；
- 前端把 fragment 换成 `SameSite=Strict` 的 same-origin cookie 并立刻 `replaceState` 抹掉 URL，
  cookie 同时覆盖 `fetch` 与 `EventSource`；
- 鉴权中间件接受 `Authorization: Bearer` **或**该 cookie。

### 9. 工具契约 v2：结构化结果与注解（破坏性）

- 每个工具同时返回 `structuredContent`（Agent 读的typed 字段）与同一份 JSON 的文本回退；
  两者形状**永远一致**，因此新旧客户端不可能对同一次调用得到不同结论。
- `search_flows` 从裸数组改为 `{ count, flows }`；写操作为 `{ ok, message, … }`，
  Agent 不必再匹配提示语字符串。
- 每个工具都带 `readOnlyHint` / `destructiveHint` / `idempotentHint`，客户端因此可以预授权
  "只读观察"而不会顺带授权 `proxy_stop`。
- 只有在形状稳定处才声明 `outputSchema`——声明了却不成立的 schema 比没有更糟；
  为此测试强制"声明了 schema 的写工具必须 `required: ["ok"]`"。

## 理由

- **"连接即启动"把进程生命周期当成了会话生命周期**。MCP 客户端的生命周期由宿主（编辑器、
  Agent 框架）决定且不可控，让它决定代理的启停，就必然出现抢占端口、状态碎片化、
  以及"没有任何关闭手段"三个后果。
- **权威必须唯一**。多个 `CoreState` 意味着多份规则与历史：Agent 在 A 窗口设的 mock 规则在
  B 窗口不生效，而两个窗口都认为自己是对的。单例守护进程是唯一能让"我看到的规则就是我读到的流量"
  成立的形态。
- **复用 HTTP 而不是新造 IPC**。控制面需要的读写能力（流量、规则、断点、策略、脚本、事件）
  已经全部在 `relay-core-http` 路由里，且 CLI 的在线命令已经在用它们；新增一套 unix socket 协议
  等于把同一份语义实现两遍，并让 CLI/MCP/WebUI 三处产生漂移风险。代价是控制面依赖一个
  loopback 端口（见"影响"）。
- **默认不自作主张**。任何"为了省资源而自动关代理"的启发式，都会在 Agent 重启、编辑器重载、
  用户切窗口时表现为"代理莫名其妙断了"。宁可让用户 `relay stop`，也不要猜。
- **不新增二进制是发布纪律的延续**。七平台两产物的矩阵已经覆盖 3 个 workflow 与 7 个
  npm 平台包；新增产物的成本远高于把 daemon 放进已有 CLI 二进制。

## 影响

- **正面**
  - 多客户端共享一份流量、规则与历史；`relay status` 能看到谁连着。
  - 代理启停变成可脚本化、可幂等的命令；Agent 与用户都能控制。
  - 端口冲突从"静默返回空数据"变成"结构化错误 + 明确指引"。
  - MCP 桥进程可以随时被杀，代理与历史不受影响。
- **代价（需知晓）**
  - 控制面在 loopback 上多开一个 HTTP 端口（默认与 `--api-port` 同一端口），
    本地任意进程可用 `daemon.json` 中的 token 调用；因此清单文件必须 `0600`，
    且默认只 bind `127.0.0.1`。
  - 守护进程需要常驻内存（含 Deno/V8 脚本引擎）；不使用者需要 `relay shutdown`。
    **不引入**开机自启或系统服务托管——那是使用者的选择，不是默认行为。
  - `relay-core-probe` **不再是引擎**：它是桥。守护进程未运行时桥会自动拉起（见 §3），
    首次连接因此有一次启动延迟；想并行跑多个互相隔离的代理需要多个 `RELAY_DATA_DIR`
    （每个目录一个守护进程），不再是"每会话一个引擎"。
- **不改变**
  - 引擎数据面、mutation pipeline、脱敏默认值。
  - RelayCraft（Tauri）当前的内嵌启动路径；它后续应迁到同一控制面，但不是本决策的前置条件。

## 相关证据

### 决策前的现状（问题）

- 无条件启代理：`relay-core-probe/src/main.rs`（`spawn_proxy` 后仅打印 warning 即继续）
- 无生命周期工具：`relay-core-probe/src/tools/mod.rs`（`tool_list()` 只有 15 个流量/规则工具）
- 运行态只能读不能控：`relay-core-probe/src/resources/mod.rs`（`proxy://status` 为 resource）
- 引擎已具备启停原语：`relay-core-runtime/src/lib.rs`（`spawn_proxy` / `stop_proxy`）、
  `relay-core-runtime/src/lifecycle.rs`（`LifecycleManager`）
- 发布矩阵约束：`publish-npm.yml` / `release.yml` / `ci.yml`、`npm/packages/binaries-*`

### 实现落点

| 关注点 | 位置 |
|---|---|
| 控制面契约（清单 / 发现 / 单实例锁） | `relay-core-http/src/control/manifest.rs` |
| 控制面客户端与发现状态 | `relay-core-http/src/control/client.rs` |
| 启动守护进程（找宿主二进制、detach、等待就绪） | `relay-core-http/src/control/bootstrap.rs` |
| 代理生命周期能力与"start 意味着在 listen" | `relay-core-runtime/src/services/proxy.rs` |
| 生命周期路由与结构化错误 | `relay-core-http/src/routes/proxy.rs`、`routes/daemon.rs` |
| 守护进程宿主（锁、清单、日志、信号、空闲超时、前台宿主） | `relay-core-cli/src/commands/daemon.rs` |
| 生命周期命令 | `relay-core-cli/src/commands/lifecycle.rs` |
| `relay run`（前台守护进程 + TUI 客户端） | `relay-core-cli/src/commands/run.rs` |
| 配置文件（加载、优先级、模板、`relay config`） | `relay-core-http/src/control/config.rs`、`relay-core-cli/src/commands/config.rs` |
| MCP 协议桥 | `relay-core-probe/src/bridge.rs` |
| MCP 生命周期工具与结构化错误 | `relay-core-probe/src/tools/lifecycle.rs` |
| 生命周期归因（`Requester` / 审计 / status 展示） | `relay-core-runtime/src/services/proxy.rs`、`relay-core-http/src/routes/proxy.rs`、`relay-core-cli/src/commands/lifecycle.rs` |
| Web UI 鉴权（fragment → cookie） | `relay-core-http/src/server.rs`、`webui/src/lib/auth.ts`、`relay-core-http/src/control/manifest.rs`（`webui_url`） |
| 契约测试 | `relay-core-http/tests/daemon_registry_tests.rs`、`tests/control_client_tests.rs`、`relay-core-cli/tests/daemon_lifecycle_tests.rs`、`relay-core-probe/tests/bridge_tests.rs` |

### 已知未做

- **会话（session）追踪**：见 §7 的评估——不做。空闲判定基于"捕获流量计数是否变化"，
  "谁连过"由审计归因回答，而不是由连接注册表回答。
- **Unix domain socket 控制面**：见 §8 的评估——不做。
- **TUI 的历史回填**：`--ui` 显示自它连上之后的流量（与旧行为一致）；要看到此前的历史需要
  先 `relay flows` 列表。
- **Tauri/RelayCraft 迁移**：桌面应用仍在进程内自建引擎（见"影响 · 不改变"）。
