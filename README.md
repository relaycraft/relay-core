# RelayCore

**Embeddable Rust traffic interception engine — CLI, REST, MCP, and Tauri on one runtime.**

Capture, inspect, and modify HTTP/HTTPS/WebSocket traffic locally. Built for developers who want programmatic control (rules, scripts, breakpoints) and for AI agents via native MCP — not just a GUI proxy.

[![Website](https://img.shields.io/badge/website-relaycore.dev-00d4ff?style=flat-square)](https://relaycore.dev) [![crates.io](https://img.shields.io/crates/v/relay-core-runtime?style=flat-square)](https://crates.io/crates/relay-core-runtime) [![docs.rs](https://img.shields.io/docsrs/relay-core-runtime?style=flat-square)](https://docs.rs/relay-core-runtime) [![CodSpeed Badge](https://img.shields.io/endpoint?url=https://app.codspeed.io//badge.json)](https://app.codspeed.io//relaycraft/relay-core?utm_source=badge) [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)

[中文](README.zh.md) · [Documentation](https://relaycore.dev/en/docs/getting-started) · [Releases](https://github.com/relaycraft/relay-core/releases) · [RelayCraft Desktop](https://github.com/relaycraft/relaycraft) (Tauri)

> The `relay-core` crate name was unavailable on crates.io. **`relay-core-runtime`** is the official main library crate.

---

## Quick start (CLI)

```bash
# Install (pick one)
cargo install relay-core-cli
npm install -g @relay-core/cli

# HTTPS interception: generate & trust the local CA once
relay-core-cli ca generate && relay-core-cli ca install
# npm: relay-core ca generate && relay-core ca install

# Start proxy (default 127.0.0.1:8080) + optional REST/SSE API on 8082
relay-core-cli run --listen 127.0.0.1:8080 --api-port 8082

# Or launch the terminal UI
relay-core-cli run --ui
```

Point your browser or app at `127.0.0.1:8080`. Full guide: [Getting started](https://relaycore.dev/en/docs/getting-started) · [Installation](https://relaycore.dev/en/docs/installation)

---

## Why RelayCore

| | RelayCore | GUI proxies | Script-first tools |
|---|-----------|-------------|------------------|
| **Model** | Embeddable engine + adapters | Manual inspection | Automation scripts |
| **Runtime** | Async Rust (`relay-core-lib`) | JVM / desktop app | Python, etc. |
| **Integration** | CLI · REST · SSE · MCP · Tauri | Limited APIs | Strong scripting |
| **AI agents** | Native MCP (`relay-core-probe`) | BYO | BYO |

**Good fit:** local API debugging · HTTPS MITM in dev · WebSocket inspection · CI traffic capture · embedded desktop proxy · **AI agents inspecting live traffic**

**Not a replacement for:** production reverse proxies, CDN edge, or untrusted multi-tenant SaaS MITM.

---

## Architecture

Layered crates: **Adapters → API → Runtime → Engine**. Public adapters share one `CoreState`; the engine handles MITM, rules, and optional scripting/storage.

<p align="center">
  <a href="https://relaycore.dev/en/#architecture">
    <img src="docs/architecture.svg" alt="RelayCore crate architecture" width="920" />
  </a>
</p>

<p align="center"><sub>Solid = primary dependency · Dashed = optional (persist / <code>feature "script"</code>) · <a href="https://relaycore.dev/en/docs/architecture">Details</a></sub></p>

---

## Crates

| Crate | Role | Status |
|-------|------|--------|
| [`relay-core-runtime`](https://crates.io/crates/relay-core-runtime) | Main API — state, lifecycle, rules, events | Public |
| [`relay-core-http`](https://crates.io/crates/relay-core-http) | REST + SSE adapter | Public |
| [`relay-core-probe`](https://crates.io/crates/relay-core-probe) | MCP adapter for AI agents | GA |
| [`relay-core-cli`](https://crates.io/crates/relay-core-cli) | CLI · TUI · embedded HTTP API | GA |
| `relay-core-tauri` | Tauri plugin (RelayCraft Desktop) | Internal |

Internal: `relay-core-api`, `relay-core-lib`, `relay-core-storage`, `relay-core-script` — not for direct downstream use.

---

## MCP for AI agents

Connect Cursor, Claude Desktop, or other MCP hosts to live traffic:

```json
{
  "mcpServers": {
    "relay-core": {
      "command": "npx",
      "args": ["-y", "@relay-core/mcp"]
    }
  }
}
```

Example tools: `search_flows`, `get_flow`, `set_rule`, `export_har`, `replay_flow`.  
Docs: [MCP guide](https://relaycore.dev/en/docs/mcp) · npm: [`@relay-core/mcp`](https://www.npmjs.com/package/@relay-core/mcp)

---

## Embed in Rust

```rust
use relay_core_runtime::{CoreState, ProxyConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(CoreState::new(None).await);
    let config = ProxyConfig::from_app_data_dir("./proxy-data", 8080)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1000);

    state.spawn_proxy(config, tx, None)?;

    while let Some(update) = rx.recv().await {
        println!("flow update: {:?}", update);
    }
    Ok(())
}
```

HTTP adapter: [`relay-core-http`](https://docs.rs/relay-core-http) · API reference: [relaycore.dev/docs/api](https://relaycore.dev/en/docs/api)

---

## Capabilities

- **MITM / TLS** — dynamic certificates, CA management
- **WebSocket** — message-level inspect, modify, replay
- **Rule engine** — match + actions (mock, redirect, delay, rate-limit, …)
- **Scripting** — Deno/V8 via optional `relay-core-script`
- **Breakpoints** — pause, edit, resume or drop live flows
- **Observability** — Prometheus metrics, audit trail, optional SQLite persistence
- **Advanced** — transparent proxy (Linux/macOS/Windows), QUIC/HTTP3 tier-1 downgrade — see [docs](https://relaycore.dev/en/docs/configuration)

---

## FAQ

**Do I need to trust a custom CA?**  
Yes, for HTTPS MITM. Generate with `relay-core-cli ca generate` and install/trust the CA locally. See [installation](https://relaycore.dev/en/docs/installation).

**How is this different from mitmproxy?**  
RelayCore is a Rust engine with multiple host adapters (including MCP), designed to embed in apps and automation. See [scripting comparison](https://relaycore.dev/en/docs/scripting-vs-mitmproxy).

**Is it production-ready as a public MITM appliance?**  
RelayCore targets **local development, debugging, testing, and agent tooling** — not internet-facing interception infrastructure.

---

## Development

```bash
cargo test --workspace
cargo clippy --workspace
cargo test --package relay-core-tauri --test offline_dev_tests
```

Transparent proxy platform matrix and integration tests: `relay-core-lib/tests/transparent_proxy_test.rs`

---

## License

MIT — see [LICENSE](LICENSE).
