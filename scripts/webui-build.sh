#!/usr/bin/env bash
# Build webui and stage assets inside relay-core-http for rust-embed + crates.io publish.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EMBED_DIR="$ROOT/relay-core-http/embed/webui"

cd "$ROOT/webui"
npm ci
npm run build

rm -rf "$EMBED_DIR"
mkdir -p "$EMBED_DIR"
cp -R dist/. "$EMBED_DIR/"
