#!/usr/bin/env bash
# Local and CI quality gate — keep in sync with .github/workflows/ci.yml (quality job).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> webui build"
"$ROOT/scripts/webui-build.sh"

echo "==> webui test"
(cd webui && npm test)

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

# Benches, examples and other non-test targets are NOT built by `cargo test`, so a change to a shared
# struct can break them without the gate noticing — which is exactly what happened when two new
# `Flow` fields broke `benches/` and only CodSpeed (which compiles them) caught it. Compile-only, so
# it costs no benchmark time.
echo "==> cargo check --all-targets"
cargo check --workspace --all-targets

# Adding a field to a widely-constructed struct breaks call sites that the host platform never
# compiles — a Linux-only literal in udp.rs passed locally and failed in CI — so check by reading the
# source rather than by asking one platform's compiler.
echo "==> struct literal completeness"
python3 scripts/check-struct-literals.py --all

echo "==> cargo test"
cargo test --workspace

echo "==> All checks passed."
