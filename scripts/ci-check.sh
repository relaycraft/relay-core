#!/usr/bin/env bash
# Local and CI quality gate — keep in sync with .github/workflows/ci.yml (quality job).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> webui build"
(cd webui && npm ci && npm run build)

echo "==> webui test"
(cd webui && npm test)

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace -- -D warnings

echo "==> cargo test"
cargo test --workspace

echo "==> All checks passed."
