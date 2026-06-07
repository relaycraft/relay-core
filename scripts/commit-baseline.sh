#!/usr/bin/env bash
# Copy the latest release benchmark report to versioned baseline files.
# Run after: ./benchmarks/bench_minimal.sh release --version X.Y.Z ...
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="$ROOT/benchmarks/results"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>   e.g. 0.8.2 or v0.8.2" >&2
  exit 2
fi
VERSION="${VERSION#v}"

latest_json="$(find "$RESULTS" -maxdepth 1 -name 'release_*.json' -type f ! -name 'baseline_*' -print 2>/dev/null | sort | tail -1)"
if [ -z "$latest_json" ] || [ ! -f "$latest_json" ]; then
  echo "error: no benchmarks/results/release_*.json found — run bench_minimal.sh release first" >&2
  exit 1
fi

base="$(basename "$latest_json" .json)"
latest_md="$RESULTS/${base}.md"
if [ ! -f "$latest_md" ]; then
  echo "error: missing companion markdown: $latest_md" >&2
  exit 1
fi

dest_json="$RESULTS/baseline_v${VERSION}.json"
dest_md="$RESULTS/baseline_v${VERSION}.md"

cp "$latest_json" "$dest_json"
cp "$latest_md" "$dest_md"

echo "==> baseline_v${VERSION}.json  (from $(basename "$latest_json"))"
echo "==> baseline_v${VERSION}.md    (from $(basename "$latest_md"))"
echo ""
echo "Next: git add benchmarks/results/baseline_v${VERSION}.json benchmarks/results/baseline_v${VERSION}.md"
echo "      git commit -m \"perf(bench): add release baseline for v${VERSION}\""
