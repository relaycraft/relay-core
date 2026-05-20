#!/usr/bin/env bash
# Copy tracked hooks into .git/hooks (not committed by git itself).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/scripts/git-hooks/pre-push"
DST="$ROOT/.git/hooks/pre-push"

if [[ ! -d "$ROOT/.git" ]]; then
  echo "error: not a git repository: $ROOT" >&2
  exit 1
fi

cp "$SRC" "$DST"
chmod +x "$DST" "$ROOT/scripts/ci-check.sh"
echo "Installed pre-push hook -> $DST"
echo "Runs: scripts/ci-check.sh (fmt, clippy, test)"
