# @relay-core/cli

CLI and TUI for **[RelayCore](https://relaycore.dev)** — a high-performance traffic interception proxy (MITM, rules, HAR, scripts).

## Install

```bash
npm i -g @relay-core/cli
# or: npx @relay-core/cli --help
```

- **Node** ≥ 18
- **Platforms:** macOS (x64, arm64), Linux (x64, arm64), Windows (x64)
- Native binaries ship via `@relay-core/binaries-*` on the npm registry (proxy/mirror friendly)

## Quick start

```bash
relay-core ca generate && relay-core ca install   # HTTPS interception, once
relay-core start                              # daemon + proxy @ 127.0.0.1:8080
relay-core status                             # daemon, proxy port, MCP endpoint
```

Point your system or browser at the proxy port.

## Daemon lifecycle

`start` brings up a single background daemon that owns the engine, and the proxy inside it. The
daemon outlives the terminal, so rules and captured history survive restarts — and several clients
(an MCP bridge, the Web UI, another shell) share the same state instead of each starting their own
proxy.

| Command | Effect |
|---------|--------|
| `relay-core start` | Idempotent: ensure the daemon runs, start the proxy. Reports the real port, or a reason why it could not bind |
| `relay-core stop` | Stop the proxy; the daemon keeps running with its history and rules |
| `relay-core restart` | Stop the proxy if running, then start it |
| `relay-core status [--json]` | Daemon pid, engine version, control URL, proxy phase/port/uptime, MCP URL, log path |
| `relay-core shutdown` | Stop the proxy and the daemon, and clean up its registry files |

Useful flags on `start`:

- `--listen 127.0.0.1:8080` — proxy port (loopback only)
- `--api-port 8082` — control API port for the daemon it starts (falls back to an ephemeral port when busy)
- `--mcp-port 18083` / `--no-mcp` — serve (or do not serve) the MCP endpoint from the daemon
- `--idle-timeout <secs>` — stop the proxy after that many seconds without captured traffic; `0` (default) never stops it automatically
- `--no-proxy` — ensure the daemon only

`relay-core run` still exists and runs the proxy in the foreground in a single process (`--ui`,
`--web`, `--mcp-port` included). It is the right choice for an interactive session; `start` is the
right choice when something else — an agent, a script, another window — needs the same engine.

The daemon writes to `$RELAY_DATA_DIR/daemon.log` and publishes `$RELAY_DATA_DIR/daemon.json`,
which is how `status` and the MCP bridge find it.

## Links

| | |
|---|---|
| Docs | [relaycore.dev](https://relaycore.dev) |
| MCP | [`@relay-core/mcp`](https://www.npmjs.com/package/@relay-core/mcp) |
| Source | [github.com/relaycraft/relay-core](https://github.com/relaycraft/relay-core) |

## License

MIT
