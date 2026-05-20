#!/usr/bin/env bash
# Run once before creating a release tag. Fails fast so we tag only once.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>   e.g. 0.3.12 or v0.3.12" >&2
  exit 2
fi
VERSION="${VERSION#v}"
TAG="v${VERSION}"

echo "==> release preflight for ${TAG}"

if [ -n "$(git status --porcelain)" ]; then
  echo "error: working tree not clean — commit or stash before release" >&2
  git status -sb >&2
  exit 1
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "error: tag ${TAG} already exists locally" >&2
  echo "  Do not delete/retag. Ship ${TAG} fixes as the next version instead." >&2
  exit 1
fi

if git ls-remote --exit-code origin "refs/tags/${TAG}" >/dev/null 2>&1; then
  echo "error: tag ${TAG} already exists on origin" >&2
  exit 1
fi

WS_VERSION="$(grep -E '^version = ' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
if [ "$WS_VERSION" != "$VERSION" ]; then
  echo "error: Cargo.toml workspace version is ${WS_VERSION}, expected ${VERSION}" >&2
  echo "  Run: node npm/scripts/set-version.js ${VERSION} after bumping Cargo.toml" >&2
  exit 1
fi

echo "==> quality gate (same as CI)"
./scripts/ci-check.sh

echo "==> preflight OK — safe to: git push origin main && git tag ${TAG} && git push origin ${TAG}"
