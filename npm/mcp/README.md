# @relay-core/mcp

MCP (Model Context Protocol) server for [RelayCore](https://github.com/relaycraft/relay-core) — expose real-time traffic inspection and control to AI agents.

The `relay-core-probe` binary is delivered through the same `@relay-core/binaries-*` optional packages as `@relay-core/cli` (npm registry, not postinstall downloads from GitHub).

## Quick Start

### Cursor / Claude Desktop

```json
{
  "mcpServers": {
    "relay-core": {
      "command": "npx",
      "args": ["@relay-core/mcp"]
    }
  }
}
```

Then ask your agent: "Search for recent 5xx errors and explain what went wrong."

### Prerequisites

HTTPS interception requires a CA certificate trusted by your system:

```bash
npx @relay-core/cli ca init && npx @relay-core/cli ca install
```

## Tools

| Category | Tools |
|----------|-------|
| Observe | `search_flows` `get_flow` `get_metrics` |
| Control | `set_intercept` `resume_flow` `set_rule` `delete_rule` |
| Analyze | `export_har` `replay_flow` |
| Policy | `get_policy` `update_policy` `patch_policy` |
| Extend | `set_script` `mock_url` |

## License

MIT
