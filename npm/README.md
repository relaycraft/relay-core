# RelayCore — npm packages

User-facing docs: **[relaycore.dev](https://relaycore.dev)**

## Published packages

| Package | Install | Purpose |
|---------|---------|---------|
| [`@relay-core/cli`](https://www.npmjs.com/package/@relay-core/cli) | `npx @relay-core/cli` | CLI / TUI wrapper → `relay-core-cli` |
| [`@relay-core/mcp`](https://www.npmjs.com/package/@relay-core/mcp) | `npx @relay-core/mcp` | MCP wrapper → `relay-core-probe` |
| `@relay-core/binaries-<platform>-<arch>` | *(automatic)* | Per-platform native binaries |

Platform ids: `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, `win32-x64`.

Binaries are **optional dependencies** (esbuild-style), not downloaded from GitHub in `postinstall`.

## Maintainer: release

Tag `v*` triggers [`.github/workflows/publish-npm.yml`](../.github/workflows/publish-npm.yml):

1. Matrix-build and publish all `@relay-core/binaries-*`
2. `verify-platform-packages.js` — all five must exist on the registry
3. Publish `@relay-core/cli` and `@relay-core/mcp`

Bump versions (including `optionalDependencies`):

```bash
node npm/scripts/set-version.js 0.3.9
```

`cargo release` runs the same sync via `scripts/sync-npm-version.py`.

If a release is incomplete, **bump a new version** — npm cannot republish the same version.

## Maintainer: local dev

```bash
cargo build --release -p relay-core-cli -p relay-core-probe
PKG=npm/packages/binaries-$(node -p "process.platform + '-' + process.arch")
cp target/release/relay-core-cli target/release/relay-core-probe "$PKG/bin/"
```
