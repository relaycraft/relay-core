# RelayCore npm distribution

Native binaries are published as optional platform packages (esbuild-style), not downloaded from GitHub in `postinstall`.

## Packages

| Package | Role |
|---------|------|
| `@relay-core/cli` | CLI/TUI wrapper (`relay-core` bin → `relay-core-cli`) |
| `@relay-core/mcp` | MCP wrapper (`relay-core-probe` bin → `relay-core-probe`) |
| `@relay-core/binaries-<platform>-<arch>` | Per-platform `relay-core-cli` + `relay-core-probe` binaries |

Platform ids match Node: `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, `win32-x64`.

## Release (CI)

On tag `v*`, `.github/workflows/publish-npm.yml`:

1. Matrix-build Rust artifacts (same targets as GitHub Releases); `fail-fast: true` cancels sibling jobs if one platform fails.
2. Publish each `@relay-core/binaries-*` package with binaries under `bin/`.
3. **`verify-platform-packages.js`** — all 5 platform packages must exist on the registry at this version before wrappers ship.
4. Publish `@relay-core/cli` and `@relay-core/mcp` (JS only; `optionalDependencies` pin platform package versions).

If step 2 is incomplete, step 4 does not run (no broken wrapper release). Note: npm cannot republish the same version — fix failed matrix jobs and use a **new** tag if some platform packages already landed.

## Bump version locally

```bash
node npm/scripts/set-version.js 0.3.9
```

Updates all `npm/packages/*/package.json`, `npm/cli`, and `npm/mcp` (including `optionalDependencies` ranges).

## Local dev (link platform package)

```bash
cargo build --release -p relay-core-cli -p relay-core-probe
PKG=npm/packages/binaries-$(node -p "process.platform + '-' + process.arch")
cp target/release/relay-core-cli target/release/relay-core-probe "$PKG/bin/"
cd npm/cli && npm link && cd ../packages/binaries-darwin-arm64 && npm link
# install @relay-core/cli with linked optional dep as needed
```
