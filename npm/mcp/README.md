# @relay-core/mcp

[MCP](https://modelcontextprotocol.io) server for **[RelayCore](https://relaycore.dev)** — connect AI agents (Cursor, Claude Desktop, …) to live HTTP(S) traffic.

## Configure

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
