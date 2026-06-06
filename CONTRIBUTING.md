# Contributing to RelayCore

Thanks for your interest in contributing.

## Setup

```bash
# Prerequisites
# - Rust stable (https://rustup.rs)
# - Python 3 (for test echo server and benchmark scripts)
# - Optional: oha (load generator for benchmarks — brew install oha / cargo install oha)

git clone https://github.com/relaycraft/relay-core.git
cd relay-core

# Build
cargo build --release --package relay-core-cli

# Run the quality gate (fmt + clippy + all tests)
./scripts/ci-check.sh
```

Optional: install the pre-push hook to run quality checks automatically:

```bash
./scripts/install-git-hooks.sh
```

## Development workflow

We follow **Test-Driven Development (TDD)**:

1. Write a failing test that defines the expected behaviour.
2. Write the minimal implementation to make the test pass.
3. Refactor while keeping tests green.

Most core logic is testable without a running proxy — use the offline test suite:

```bash
cargo test --package relay-core-tauri --test offline_dev_tests
```

## Commit conventions

```
<type(scope): subject>
```

| Type | Use |
|------|-----|
| `feat` | New user-facing capability |
| `fix` | Bug fix |
| `perf` | Performance improvement |
| `refactor` | Internal restructuring |
| `test` | Tests |
| `docs` | Documentation |
| `chore` | Tooling, deps, maintenance |

Subject: single intent, ≤72 chars, present tense.  
Multi-file changes (≥5 files): include a body with a brief description of why and impact.

## Before submitting

- [ ] `./scripts/ci-check.sh` passes
- [ ] New tests cover the changes
- [ ] Commit messages follow the convention above

## Performance

Benchmarks are part of the quality gate. If your change could affect performance (hot-path code, TLS, rule engine), run the release benchmark locally before pushing:

```bash
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

Micro-benchmarks run automatically on every PR via CodSpeed — you'll see results in the PR comment.

## Release process

See [RELEASE.md](RELEASE.md).

## Architecture

See [Architecture overview](https://relaycore.dev/en/docs/architecture) for a visual guide to the crate layout and data flow.

## License

MIT — see [LICENSE](LICENSE).
