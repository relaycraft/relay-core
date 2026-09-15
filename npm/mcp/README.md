# @relay-core/mcp

[MCP](https://modelcontextprotocol.io) server for **[RelayCore](https://relaycore.dev)** — connect AI agents (OpenCode, Cursor, Claude Desktop, …) to live HTTP(S) traffic.

## Two ways to run

### A. Shared server (recommended)

One long-running process owns the proxy port and all captured state; every MCP client connects to the same stream of flows. Your client proxy settings stay fixed, and multiple agent windows see the same data.

```bash
relay-core-probe --transport=sse
# optional persistence so history survives restarts:
relay-core-probe --transport=sse \
  --db-url="sqlite:///$HOME/.local/share/relay-core/probe.db?mode=rwc"
```

Then point each MCP client at `http://127.0.0.1:18083/mcp`. OpenCode example (`~/.config/opencode/opencode.jsonc`):

```jsonc
"mcp": {
  "relay-core": {
    "type": "remote",
    "url": "http://127.0.0.1:18083/mcp",
    "enabled": true
  }
}
```

| Flag | Env | Default | Purpose |
|------|-----|---------|---------|
| `--transport=` | `RELAY_PROBE_TRANSPORT` | `stdio` | `stdio` or `sse` (streamable HTTP) |
| `--probe-port=` | `RELAY_PROBE_PORT` | `18083` | MCP listen port (sse mode) |
| `--probe-bind=` | `RELAY_PROBE_BIND` | `127.0.0.1` | MCP listen address |
| `--port=` | `RELAY_PORT` | `8080` | Proxy listen port |
| `--db-url=` | `RELAY_DB_URL` | in-memory | `sqlite:///path.db?mode=rwc` for persistence |
| `--ca-cert=` / `--ca-key=` | `RELAY_CA_CERT` / `RELAY_CA_KEY` | data dir | CA paths |

### B. Per-session stdio

The client spawns one process per session. Simple, but each session is a separate proxy and a separate flow history — fine for a single window, confusing with several.

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

Requires **Node** ≥ 18. Binaries install via `@relay-core/binaries-*` (same as the CLI package).

## HTTPS prerequisite

Install and trust the RelayCore CA once:

```bash
npx @relay-core/cli ca generate && npx @relay-core/cli ca install
```

## Tools (overview)

| Area | Examples |
|------|----------|
| Observe | `search_flows`, `get_flow`, `get_metrics` |
| Control | `set_intercept`, `resume_flow`, `set_rule` |
| Analyze | `export_har`, `replay_flow` |
| Policy / scripts | `get_policy`, `set_script`, `mock_url` |

Details: [relaycore.dev](https://relaycore.dev) · [GitHub](https://github.com/relaycraft/relay-core)

## License

MIT
