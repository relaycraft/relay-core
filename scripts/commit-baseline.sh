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

# A baseline is a promise about performance, so refuse to promote a report whose DoD did not pass.
# Without this check a FAIL-status run (e.g. 0 req/s from a dead proxy) became the official baseline.
dod_failures="$(
  python3 - "$latest_json" <<'PY'
import json, sys

try:
    with open(sys.argv[1], "r", encoding="utf-8") as fh:
        data = json.load(fh)
except Exception as exc:  # unreadable report is itself a failure
    print(f"unreadable ({exc})")
    sys.exit(0)

dod = data.get("dod") or data.get("status") or {}
bad = []
for key, value in dod.items():
    if key == "regression" or value == "WARN":
        continue
    if str(value).upper() not in ("PASS", "OK", "SKIP"):
        bad.append(f"{key}={value}")

if not dod:
    bad.append("no DoD status block found")

print(", ".join(bad))
PY
)"

if [ -n "$dod_failures" ]; then
  echo "error: refusing to promote a report that did not pass its DoD checks:" >&2
  echo "       $dod_failures" >&2
  echo "       report: $latest_json" >&2
  echo "       Re-run: ./benchmarks/bench_minimal.sh release --version ${VERSION} --strict ..." >&2
  exit 1
fi

dest_json="$RESULTS/baseline_v${VERSION}.json"
dest_md="$RESULTS/baseline_v${VERSION}.md"

# Never silently overwrite an existing baseline.
if [ -f "$dest_json" ]; then
  echo "error: $dest_json already exists — bump the version or remove it deliberately" >&2
  exit 1
fi

cp "$latest_json" "$dest_json"
cp "$latest_md" "$dest_md"

echo "==> baseline_v${VERSION}.json  (from $(basename "$latest_json"))"
echo "==> baseline_v${VERSION}.md    (from $(basename "$latest_md"))"
echo ""
echo "Next: git add benchmarks/results/baseline_v${VERSION}.json benchmarks/results/baseline_v${VERSION}.md"
echo "      git commit -m \"perf(bench): add release baseline for v${VERSION}\""
