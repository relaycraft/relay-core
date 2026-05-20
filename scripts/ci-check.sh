#!/usr/bin/env bash
# Local and CI quality gate — keep in sync with .github/workflows/ci.yml (quality job).
#
# On macOS, also runs the same checks inside Linux Docker (see ci-check-linux-docker.sh)
# so cfg/dead-code issues visible only on ubuntu-latest are caught before push.
#
# Usage:
#   ./scripts/ci-check.sh              # host + Linux parity on Darwin
#   ./scripts/ci-check.sh --host-only  # host only (used inside Docker)
#   RELAY_SKIP_LINUX_PARITY=1 ./scripts/ci-check.sh
set -euo pipefail

HOST_ONLY=false
for arg in "$@"; do
  case "$arg" in
    --host-only) HOST_ONLY=true ;;
    *)
      echo "error: unknown argument: $arg" >&2
      echo "usage: $0 [--host-only]" >&2
      exit 2
      ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (host: $(rustc -vV | sed -n 's/^host: //p'))"
cargo clippy --workspace -- -D warnings

echo "==> cargo test"
cargo test --workspace

if [ "$HOST_ONLY" = false ] && [ "$(uname -s)" = "Darwin" ] && [ "${RELAY_SKIP_LINUX_PARITY:-}" != "1" ]; then
  if command -v docker >/dev/null 2>&1; then
    "$ROOT/scripts/ci-check-linux-docker.sh"
  else
    echo "" >&2
    echo "warning: Docker not installed — skipping Linux CI parity on macOS." >&2
    echo "  Push may still fail on ubuntu-latest (e.g. dead_code on platform-only symbols)." >&2
    echo "  Install Docker Desktop, or run: RELAY_SKIP_LINUX_PARITY=1 $0" >&2
    echo "" >&2
    exit 1
  fi
fi

echo "==> All checks passed."
