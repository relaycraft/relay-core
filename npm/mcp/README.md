# @relay-core/mcp

[MCP](https://modelcontextprotocol.io) server for **[RelayCore](https://relaycore.dev)** — connect AI agents (OpenCode, Cursor, Claude Desktop, …) to live HTTP(S) traffic.

## How it works

One long-lived **daemon** owns the engine: the proxy, the captured flows, the rules and the intercept
queue. This package is a **bridge** to that daemon, not a second engine.

```
MCP client ──stdio──▶ @relay-core/mcp (bridge) ──HTTP──▶ daemon ──▶ proxy ──▶ traffic
                                                          ▲
relay start / relay stop ─────────────────────────────────┘
```

Consequences worth knowing:

- Connecting an MCP client **never starts a proxy** and never starts a second flow history. Every
  client (CLI, bridge, Web UI, editor) reads and writes the same state.
- The proxy is started and stopped by an explicit command: `relay start` / `relay stop`, or the
  `proxy_start` / `proxy_stop` tools. It is **not** stopped automatically — there is no idle timeout
  unless the daemon was started with `--idle-timeout`.
- When no daemon is running, the bridge starts one (detached, so it outlives the editor window) and
  attaches. Set `RELAY_MCP_NO_AUTOSTART=1` to require an explicit `relay start` instead.

## Setup

```bash
npx @relay-core/cli ca generate && npx @relay-core/cli ca install   # HTTPS interception, once
npx @relay-core/cli start                                          # daemon + proxy
```

Then point your MCP client at the bridge:

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

Whether the daemon is already running is irrelevant — the bridge attaches to it, or starts it.

### Clients that speak HTTP MCP

The daemon serves the same MCP endpoint over streamable HTTP, so an HTTP-capable client can skip the
bridge entirely. `relay status` prints the URL:

```jsonc
"mcp": {
  "relay-core": {
    "type": "remote",
    "url": "http://127.0.0.1:18083/mcp",   // port from `relay status`
    "enabled": true
  }
}
```

## Tools

| Area | Tools |
|------|-------|
| Lifecycle | `proxy_status`, `proxy_start`, `proxy_stop` |
| Observe | `search_flows`, `get_flow`, `get_metrics` |
| Control | `set_intercept`, `get_pending_intercepts`, `resume_flow`, `set_rule`, `delete_rule`, `mock_url` |
| Policy / scripts | `get_policy`, `update_policy`, `patch_policy`, `set_script` |
| Analyze | `export_har`, `replay_flow` |

Call `proxy_status` first when traffic tools return nothing: it separates "no traffic yet" from
"nothing is being captured". Traffic tools answer with a structured `proxy_not_running` error rather
than an empty list when no proxy is running.

Every tool returns `structuredContent` (typed fields, with a declared output schema where the shape
is stable) and the same JSON as text for older clients. Every tool also carries
`readOnlyHint` / `destructiveHint` / `idempotentHint` annotations, so a client can pre-approve the
observation tools without pre-approving `proxy_stop`. Tool contract version: **2**.

## Flags and environment

| Flag | Env | Default | Purpose |
|------|-----|---------|---------|
| — | `RELAY_DATA_DIR` | `~/.relay-core` | Which daemon to attach to |
| `--mcp-url=<url>` | `RELAY_MCP_URL` | discovered | Skip discovery and use this MCP endpoint |
| `--token=<token>` | `RELAY_MCP_TOKEN` | from manifest | Bearer token for the endpoint above |
| `--no-autostart` | `RELAY_MCP_NO_AUTOSTART=1` | from config | Refuse to start a daemon (`[client] autostart_daemon = false` in `$RELAY_DATA_DIR/config.toml` is the durable way) |

There is no embedded mode: this package never owns an engine. Point it at a daemon (discovered or
with `--mcp-url`) and it will always show you the same traffic the CLI and the Web UI see.

## Requirements

Requires **Node** ≥ 18. Binaries install via `@relay-core/binaries-*` (same as the CLI package). The
bridge looks for the `relay-core-cli` binary next to itself, then on `PATH`.

## License

MIT
