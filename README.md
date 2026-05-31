# RelayCore

High-performance Rust traffic interception engine.

Built as a standalone proxy platform with multiple host adapters (CLI, TUI, HTTP API, MCP, Tauri plugin) on top of a shared runtime.

> The `relay-core` crate name was unavailable on crates.io. `relay-core-runtime` is the official main package.

## Features

- **HTTP/HTTPS Proxy** — full MITM with dynamic TLS certificate generation
- **WebSocket Interception** — message-level inspection, modification, and replay
- **Rule Engine** — match + action pipeline (headers, body, status, redirect, mock, rate-limit)
- **Scripting** — Deno/V8 runtime for dynamic request/response/WebSocket modification
- **Interception Breakpoints** — pause live traffic, inspect, modify, then resume or drop
- **Transparent Proxy** — Linux TPROXY (TCP+UDP), macOS PF (TCP+UDP), Windows WinDivert (TCP)
- **QUIC/HTTP3** — Tier 1 downgrade (strip Alt-Svc headers) + Tier 2 UDP forward; application-layer MITM (decryption) deferred to post-1.0
- **Traffic Redaction** — configurable sensitive field masking (headers, query params, bodies)
- **Audit Trail** — control-plane mutations tracked with actor attribution and persistence
- **Metrics** — Prometheus text format endpoint, structured metrics snapshot
- **Flow Storage** — in-memory LRU + optional SQLite persistence with pagination

## Architecture

```
Adapters (CLI, HTTP API, MCP, Tauri)
        │
relay-core-runtime (state hub, lifecycle, event bus)
        │
relay-core-lib (capture, proxy, MITM, TLS, rule engine)
        │
relay-core-api / relay-core-script / relay-core-storage
```

## Crates

| Crate | Role | Status |
|-------|------|--------|
| `relay-core-runtime` | Main API — state orchestration, proxy lifecycle, rules, events | Public |
| `relay-core-http` | REST + SSE adapter for Web UIs and external tooling | Public |
| `relay-core-probe` | MCP adapter for AI agents and automation | GA |
| `relay-core-cli` | Standalone CLI and TUI binary | GA |
| `relay-core-tauri` | Tauri plugin for RelayCraft desktop application | Internal |

*Internal support crates: `relay-core-api` (types), `relay-core-lib` (engine), `relay-core-storage` (SQLite), `relay-core-script` (Deno). Not intended for direct use.*

## Quick Start

```rust
use relay_core_runtime::{CoreState, ProxyConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Create runtime state
    let state = Arc::new(CoreState::new(None).await);

    // Configure proxy
    let config = ProxyConfig::from_app_data_dir("./proxy-data", 8080).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1000);

    // Start proxy
    state.spawn_proxy(config, tx, None).unwrap();

    // Receive live traffic updates
    while let Some(update) = rx.recv().await {
        println!("Flow update: {:?}", update);
    }
}
```

### Using the CLI

```bash
# Install
cargo install relay-core-cli

# Start proxy on port 8080
relay-core-cli run

# Or with TUI
relay-core-cli run --ui

# Generate CA certificate for HTTPS interception
relay-core-cli ca generate

# Validate rules
relay-core-cli rules validate --file rules.yaml
```

### Using the HTTP API

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
// REST API available at http://127.0.0.1:3000/api/v1/
```

## HTTP API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/flows` | Search & paginate flows |
| GET | `/api/v1/flows/:id` | Get flow detail |
| GET | `/api/v1/rules` | List all rules |
| PUT | `/api/v1/rules` | Upsert a rule |
| DELETE | `/api/v1/rules/:id` | Delete a rule |
| POST | `/api/v1/intercepts` | Create intercept rule |
| POST | `/api/v1/intercepts/resolve` | Resolve pending intercept |
| GET | `/api/v1/intercepts` | List pending intercepts |
| GET | `/api/v1/metrics` | JSON metrics snapshot |
| GET | `/api/v1/metrics/prometheus` | Prometheus text format |
| GET | `/api/v1/audit` | Query audit events |
| GET | `/api/v1/events` | SSE stream of live updates |

## Platform Support

### Transparent Proxy

| Platform | TCP | UDP | Status |
|----------|-----|-----|--------|
| Linux | Verified | Verified | Full support via TPROXY + SO_ORIGINAL_DST |
| macOS | Verified | Experimental | TCP via PF DIOCNATLOOK; UDP lookup available, await wiring |
| Windows | Experimental | 1.x | TCP via WinDivert (driver signing required); UDP deferred |

> See `relay-core-lib/tests/transparent_proxy_test.rs` for integration tests.
> For platforms without transparent proxy support, use explicit proxy mode
> (`curl --proxy http://127.0.0.1:8080`) or system proxy configuration.

## Development

```bash
# Run all tests
cargo test --workspace

# Check lints
cargo clippy --workspace

# Run offline test suite (Tauri contract tests)
cargo test --package relay-core-tauri --test offline_dev_tests
```

## License

MIT
