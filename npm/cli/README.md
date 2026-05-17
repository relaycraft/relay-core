# @relay-core/cli

Standalone CLI and TUI for [RelayCore](https://github.com/relaycraft/relay-core) — a high-performance Rust traffic interception engine.

## Quick Start

```bash
npx @relay-core/cli ca init && npx @relay-core/cli ca install
npx @relay-core/cli run --ui
```

The proxy listens on `127.0.0.1:8080`. Configure your system or browser to use it.

## Commands

| Command | Description |
|---------|-------------|
| `run` | Start the proxy server. Add `--ui` for TUI mode. |
| `ca {init,install,status,uninstall}` | Manage CA certificate for HTTPS interception |
| `metrics` | View runtime metrics (requires `--api-port`) |

For a full command reference: `npx @relay-core/cli run --help`

## License

MIT
