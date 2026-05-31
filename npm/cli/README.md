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
relay-core ca generate && relay-core ca install   # HTTPS interception
relay-core run --ui                           # proxy @ 127.0.0.1:8080
```

Point your system or browser at the proxy port. Full CLI reference: `relay-core run --help`.

## Links

| | |
|---|---|
| Docs | [relaycore.dev](https://relaycore.dev) |
| MCP | [`@relay-core/mcp`](https://www.npmjs.com/package/@relay-core/mcp) |
| Source | [github.com/relaycraft/relay-core](https://github.com/relaycraft/relay-core) |

## License

MIT
