# RelayCore

基于 Rust 的高性能流量拦截引擎 —— 新一代 mitmproxy 替代方案。

RelayCore 是一个独立的代理平台，在共享运行时之上提供了多种宿主适配器（CLI、TUI、HTTP API、MCP、Tauri 插件）。

> `relay-core` 名称在 crates.io 已被占用，`relay-core-runtime` 是官方主包。

## 功能特性

- **HTTP/HTTPS 代理** — 完整的中间人代理，支持动态 TLS 证书生成
- **WebSocket 拦截** — 消息级别的检查、修改与重放
- **规则引擎** — 匹配 + 动作管线（请求头、请求体、状态码、重定向、Mock、限速）
- **脚本引擎** — 基于 Deno/V8 运行时，动态修改请求、响应与 WebSocket 消息
- **断点拦截** — 暂停实时流量，检查、修改后放行或丢弃
- **透明代理** — Linux TPROXY（TCP+UDP）、macOS PF（TCP）
- **流量脱敏** — 可配置的敏感字段掩码（请求头、查询参数、请求/响应体）
- **审计追踪** — 控制面操作全量记录，支持执行者归因与持久化
- **指标监控** — Prometheus 文本格式端点、结构化指标快照
- **流量持久化** — 内存 LRU 缓存 + 可选 SQLite 持久化，支持分页查询

## 架构

```
适配层（CLI、HTTP API、MCP、Tauri）
        │
relay-core-runtime（状态中心、生命周期、事件总线）
        │
relay-core-lib（流量捕获、代理、MITM、TLS、规则引擎）
        │
relay-core-api / relay-core-script / relay-core-storage
```

## 包说明

| 包 | 角色 | 状态 |
|------|------|--------|
| `relay-core-runtime` | 主 API — 状态编排、代理生命周期、规则管理、事件流 | 公开 |
| `relay-core-http` | REST + SSE 适配器，供 Web UI 和外部工具集成 | 公开 |
| `relay-core-probe` | MCP 适配器，供 AI Agent 与自动化使用 | Beta |
| `relay-core-cli` | 独立 CLI 与 TUI 二进制程序 | Beta |
| `relay-core-tauri` | RelayCraft 桌面端 Tauri 插件 | 内部 |

*内部支撑包：`relay-core-api`（类型合约）、`relay-core-lib`（引擎）、`relay-core-storage`（SQLite）、`relay-core-script`（Deno）。非面向用户，不应直接依赖。*

## 快速开始

```rust
use relay_core_runtime::{CoreState, ProxyConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // 创建运行时状态
    let state = Arc::new(CoreState::new(None).await);

    // 配置代理
    let config = ProxyConfig::from_app_data_dir("./proxy-data", 8080).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1000);

    // 启动代理
    state.spawn_proxy(config, tx, None).unwrap();

    // 接收实时流量更新
    while let Some(update) = rx.recv().await {
        println!("Flow update: {:?}", update);
    }
}
```

### 命令行使用

```bash
# 安装
cargo install relay-core-cli

# 在 8080 端口启动代理
relay-core-cli run --port 8080

# 生成用于 HTTPS 拦截的 CA 证书
relay-core-cli ca init

# 验证规则文件
relay-core-cli rules validate --file rules.yaml
```

### HTTP API 集成

```toml
[dependencies]
relay-core-http = "0.1"
```

```rust
use relay_core_runtime::CoreState;
use relay_core_http::HttpApiServer;
use std::sync::Arc;

let state = Arc::new(CoreState::new(None).await);
let server = HttpApiServer::new(Default::default(), state);
server.serve().await.unwrap();
// REST API 地址: http://127.0.0.1:3000/api/v1/
```

## HTTP API 端点

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| GET | `/api/v1/flows` | 搜索与分页查询流量 |
| GET | `/api/v1/flows/:id` | 获取流量详情 |
| GET | `/api/v1/rules` | 列出全部规则 |
| PUT | `/api/v1/rules` | 更新或创建规则 |
| DELETE | `/api/v1/rules/:id` | 删除规则 |
| POST | `/api/v1/intercepts` | 创建断点规则 |
| POST | `/api/v1/intercepts/resolve` | 处理待定断点 |
| GET | `/api/v1/intercepts` | 列出待定断点 |
| GET | `/api/v1/metrics` | JSON 格式指标快照 |
| GET | `/api/v1/metrics/prometheus` | Prometheus 文本格式 |
| GET | `/api/v1/audit` | 查询审计事件 |
| GET | `/api/v1/events` | SSE 实时事件流 |

## 开发

```bash
# 运行全部测试
cargo test --workspace

# 代码检查
cargo clippy --workspace

# 离线测试套件（Tauri 契约测试）
cargo test --package relay-core-tauri --test offline_dev_tests
```

## 许可证

MIT
