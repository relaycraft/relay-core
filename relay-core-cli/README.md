# relay-core-cli

Standalone CLI and TUI for [RelayCore](https://github.com/relaycraft/relay-core) — a high-performance Rust traffic interception engine. Provides local proxy operation, HTTPS decryption, rule-based modification, script injection, and real-time traffic inspection.

## Quick Start

```bash
# Install
cargo install relay-core-cli

# Generate CA certificate for HTTPS interception
relay-core-cli ca init
relay-core-cli ca install

# Start proxy
relay-core-cli run

# Start proxy with TUI (recommended for interactive use)
relay-core-cli run --ui
```

The proxy listens on `127.0.0.1:8080` by default. Configure your browser or system to use it as an HTTP/HTTPS proxy.

## Commands

| Command | Description |
|---------|-------------|
| `run` | Start the proxy server. Use `--ui` for TUI mode. |
| `ca {init,install,status,export,uninstall}` | Manage CA certificate for HTTPS decryption |
| `rules` | Manage interception rules (list, validate, add, delete) |
| `scripts` | Manage Deno scripts for dynamic traffic modification |
| `flows` | Query captured flows (requires running proxy with `--api-port`) |
| `metrics` | View proxy runtime metrics |

## Run Options

```
relay-core-cli run [OPTIONS]

Options:
  -l, --listen <LISTEN>          Proxy address [default: 127.0.0.1:8080]
  -c, --control-port <PORT>      Control API port [default: 8081]
  --ui                            Enable TUI mode (interactive terminal UI)
  --api-port <PORT>               Enable REST/SSE HTTP API on this port
  --api-bind <ADDR>               HTTP API bind address [default: 127.0.0.1]
  --api-token <TOKEN>             Bearer token for HTTP API authentication
  --api-cors <ORIGINS>            CORS allowed origins (comma-separated)
  --rules <PATH>                  Load rules from JSON/YAML file
  --script <PATH>                 Load Deno script file
  --script-watch                  Watch script file for changes
  --transparent                   Enable transparent proxy mode
  --output <FORMAT>               Output format (table, json, jsonl) [default: table]
  --save-stream <PATH>            Save flow stream to file (JSONL)
  -h, --help                      Print help
```

## TUI Mode

Run with `--ui` for an interactive terminal interface:

```
relay-core-cli run --ui
```

Keyboard shortcuts:
- `j/k` or `↑/↓` — navigate flow list
- `g` — jump to newest flow
- `Enter` / `l` — focus detail panel
- `Esc` / `h` — focus flow list
- `Tab` — switch detail tab (Overview / Request / Response / Messages)
- `1-4` — jump to specific tab
- `/` — filter flows by host/URL/method
- `?` — show help overlay
- `q` — quit

## HTTPS Interception

To intercept HTTPS traffic, you need to generate and install a CA certificate:

```bash
# Generate CA (one-time)
relay-core-cli ca init

# Install to system trust store (macOS)
relay-core-cli ca install

# Verify installation
relay-core-cli ca status
```

**macOS**: The certificate is installed to the System Keychain and trusted automatically.
**Linux**: Copy the generated `ca_cert.pem` to `/usr/local/share/ca-certificates/` and run `update-ca-certificates`.
**Windows**: Import `ca_cert.pem` via `certmgr.msc` into "Trusted Root Certification Authorities".

After installation, configure your browser to use `127.0.0.1:8080` as the HTTP/HTTPS proxy.

## HTTP API

Enable with `--api-port` to expose a REST + SSE API:

```bash
relay-core-cli run --api-port 8082
```

Endpoints: `/api/v1/flows`, `/api/v1/rules`, `/api/v1/intercepts`, `/api/v1/metrics`, `/api/v1/events` (SSE), `/api/v1/audit`.

Add `--api-token` for Bearer authentication and `--api-cors` for CORS origins.

## Platform Support

| Feature | macOS | Linux | Windows |
|---------|-------|-------|---------|
| HTTP/HTTPS proxy | ✅ | ✅ | ✅ |
| TUI | ✅ | ✅ | ✅ |
| Transparent proxy | ✅ (PF) | ✅ (TPROXY) | ⏳ |
| CA auto-install | ✅ | ⏳ (manual) | ⏳ (manual) |

## License

MIT
