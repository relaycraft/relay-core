---
name: relay-core-release
description: >-
  RelayCore version release discipline: quality gate, benchmarks, version bump,
  preflight, tag, and verification across crates.io, npm, and GitHub Release.
  Use when releasing, tagging, bumping versions, publishing crates/npm, or
  fixing failed release workflows in relay-core.
---

# RelayCore Release

**Repo doc:** `RELEASE.md` at relay-core workspace root  
**Pitfalls:** [pitfalls.md](pitfalls.md)

## Three publish paths (verify each separately)

| Workflow | Delivers | Success check |
|----------|----------|---------------|
| `publish.yml` | crates.io (`cargo install`) | `cargo search relay-core-cli` shows version |
| `publish-npm.yml` | `@relay-core/cli`, binaries | `npm view @relay-core/cli@X.Y.Z version` |
| `release.yml` | GitHub Release binaries | `gh release view vX.Y.Z` has assets |

One path succeeding does **not** mean the release is complete. Report all three.

## Release checklist

```
- [ ] ./scripts/ci-check.sh green locally
- [ ] Benchmark baseline committed (if shipping perf claims)
- [ ] chore: bump version to X.Y.Z → push → main CI green
- [ ] WebUI publish dry-run (if webui feature changed)
- [ ] ./scripts/release-preflight.sh X.Y.Z
- [ ] git tag vX.Y.Z && git push origin vX.Y.Z
- [ ] All three tag workflows green
- [ ] Smoke: cargo install / npm i -g / gh release assets
```

## Standard flow

### 1. Quality gate

```bash
./scripts/ci-check.sh
```

### 2. Performance baseline (before version bump)

```bash
./benchmarks/bench_minimal.sh release --version X.Y.Z --runs 5 --warmup-runs 3 --duration 30
./scripts/commit-baseline.sh X.Y.Z
rm -f benchmarks/results/release_*.json benchmarks/results/release_*.md
git add benchmarks/results/
git commit -m "perf(bench): add release baseline for vX.Y.Z"
```

### 3. Version bump

```bash
node npm/scripts/set-version.js X.Y.Z
cargo generate-lockfile
git commit -m "chore: bump version to X.Y.Z"
git push origin main
# WAIT for CI green
```

### 4. WebUI / crates.io dry-run

```bash
./scripts/webui-build.sh
test -f relay-core-http/embed/webui/index.html
./scripts/cargo-retry.sh package -p relay-core-http --features webui --allow-dirty --no-verify
tar tzf target/package/relay-core-http-*.crate | grep embed/webui
```

### 5. Preflight + tag once

```bash
./scripts/release-preflight.sh X.Y.Z
git tag vX.Y.Z && git push origin vX.Y.Z
```

### 6. Post-tag verification

```bash
gh run list --limit 6
cargo search relay-core-cli --limit 1
npm view @relay-core/cli@X.Y.Z version
gh release view vX.Y.Z
```

## If tag workflows fail

1. Do not delete or force-retag.
2. Fix on `main`, bump next patch, repeat.
3. GitHub Release only failed → `gh workflow run release.yml --ref main -f ref=vX.Y.Z`

## Agent rules

- Never tag before preflight + CI green on bump commit.
- Never claim cargo install is fine when `publish.yml` fails.
- Never chain patch bumps without fixing root cause.
- Suggest second model review before tag on release-critical changes.
- No force-push / retag without explicit user approval.

## Additional resources

- [pitfalls.md](pitfalls.md)
- `RELEASE.md` in relay-core repo
