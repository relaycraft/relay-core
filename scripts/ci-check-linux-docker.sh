#!/usr/bin/env bash
# Run the same Rust quality gate as GitHub Actions (ubuntu-latest) inside Docker.
# Used from ci-check.sh on macOS so platform-specific dead-code is caught before push.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${RELAY_CI_LINUX_IMAGE:-rust:1-bookworm}"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker not found — install Docker Desktop or run CI on Linux" >&2
  exit 1
fi

echo "==> Linux parity via Docker (${IMAGE})"

docker run --rm \
  -v "$ROOT:/work" \
  -w /work \
  -e CARGO_TARGET_DIR=/work/target/linux-ci-target \
  "$IMAGE" \
  bash -lc '
    set -euo pipefail
    export PATH="/usr/local/cargo/bin:${PATH}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
      >/dev/null
    rustup component add rustfmt clippy >/dev/null 2>&1 || true
    ./scripts/ci-check.sh --host-only
  '

echo "==> Linux parity passed"
