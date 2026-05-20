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
  echo "  Do not delete/retag. Ship fixes as the next version instead." >&2
  exit 1
fi

if git ls-remote --exit-code origin "refs/tags/${TAG}" >/dev/null 2>&1; then
  echo "error: tag ${TAG} already exists on origin" >&2
  exit 1
fi

WS_VERSION="$(grep -E '^version = ' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
if [ "$WS_VERSION" != "$VERSION" ]; then
  echo "error: Cargo.toml workspace version is ${WS_VERSION}, expected ${VERSION}" >&2
  echo "  Bump Cargo.toml, then: node npm/scripts/set-version.js ${VERSION}" >&2
  exit 1
fi

echo "==> local quality gate (host; Linux checks run on GitHub CI)"
./scripts/ci-check.sh

echo "==> origin/main is up to date"
git fetch origin main
HEAD_SHA="$(git rev-parse HEAD)"
ORIGIN_SHA="$(git rev-parse origin/main)"
if [ "$HEAD_SHA" != "$ORIGIN_SHA" ]; then
  echo "error: HEAD is not on origin/main — push first:" >&2
  echo "  git push origin main" >&2
  git status -sb >&2
  exit 1
fi

echo "==> GitHub CI green for ${HEAD_SHA:0:7}"
if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh CLI required — install and auth, or confirm CI green manually before tagging" >&2
  exit 1
fi

RUN_STATUS="$(gh run list --commit "$HEAD_SHA" --workflow ci.yml --limit 1 --json status -q '.[0].status' 2>/dev/null || true)"
RUN_CONCLUSION="$(gh run list --commit "$HEAD_SHA" --workflow ci.yml --limit 1 --json conclusion -q '.[0].conclusion' 2>/dev/null || true)"

if [ -z "$RUN_STATUS" ] || [ "$RUN_STATUS" = "null" ]; then
  echo "error: CI workflow has not run on this commit" >&2
  echo "  Push to main and wait for https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/actions" >&2
  exit 1
fi

if [ "$RUN_STATUS" != "completed" ]; then
  echo "error: CI is ${RUN_STATUS} — wait for completion, then re-run preflight" >&2
  exit 1
fi

if [ "$RUN_CONCLUSION" != "success" ]; then
  echo "error: CI conclusion is ${RUN_CONCLUSION} (expected success)" >&2
  exit 1
fi

echo "==> preflight OK — tag once: git tag ${TAG} && git push origin ${TAG}"
