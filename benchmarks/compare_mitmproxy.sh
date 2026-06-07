#!/usr/bin/env bash
# compare_mitmproxy.sh - relay-core vs mitmproxy baseline comparison
#
# Runs the same HTTP throughput benchmark against both relay-core and mitmproxy,
# producing a comparison report with `vs_mitmproxy` fields.
#
# Prerequisites:
#   - relay-core-cli built (release)
#   - mitmproxy installed (pip install mitmproxy)
#   - oha or ab for load generation
#
# Usage:
#   ./benchmarks/compare_mitmproxy.sh [--duration 30] [--connections 100]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

PROXY_PORT="${PROXY_PORT:-18080}"
RELAY_PORT="${RELAY_PORT:-18081}"
TARGET_PORT="${TARGET_PORT:-19000}"
CONNECTIONS="${CONNECTIONS:-100}"
DURATION="${DURATION:-30}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration) DURATION="${2:-30}"; shift 2 ;;
    --connections) CONNECTIONS="${2:-100}"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
OUT_JSON="$RESULTS_DIR/vs_mitmproxy_${TIMESTAMP}.json"
OUT_MD="$RESULTS_DIR/vs_mitmproxy_${TIMESTAMP}.md"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "  ${YELLOW}->${NC} $*"; }
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }

has_tool() { command -v "$1" >/dev/null 2>&1; }

# ── Tool detection ──────────────────────────────────────────────────

LOAD_TOOL=""
if has_tool oha; then
  LOAD_TOOL="oha"
elif has_tool ab; then
  LOAD_TOOL="ab"
else
  fail "No load generator found (oha or ab required)"
  exit 1
fi

if ! has_tool mitmdump; then
  fail "mitmdump not found — install mitmproxy: pip install mitmproxy"
  fail "Running relay-core baseline only (no comparison)"
  MITM_AVAILABLE=0
else
  MITM_AVAILABLE=1
fi

# ── Helpers ─────────────────────────────────────────────────────────

run_oha() {
  local proxy_port="$1"
  local target="http://127.0.0.1:${TARGET_PORT}/payload/1"
  env -u NO_COLOR oha -z "${DURATION}s" -c "$CONNECTIONS" --no-tui --output-format json \
    -x "http://127.0.0.1:${proxy_port}" "$target" 2>/dev/null || echo "{}"
}

extract_rps() {
  python3 -c '
import json, sys
d = json.load(sys.stdin)
s = d.get("summary", {})
print(int(s.get("requestsPerSec", 0) or 0))
' 2>/dev/null || echo 0
}

extract_p99() {
  python3 -c '
import json, sys
d = json.load(sys.stdin)
srcs = [
    d.get("responseTimeHistogram", {}),
    d.get("latencyPercentiles", {}),
]
for src in srcs:
    if isinstance(src, dict) and "p99" in src:
        p99 = float(src["p99"] or 0)
        if p99 < 1.0: p99 *= 1000
        print(round(p99, 2))
        break
' 2>/dev/null || echo 0
}

# ── Target server ───────────────────────────────────────────────────

cleanup() {
  kill "$TARGET_PID" 2>/dev/null || true
  kill "$RELAY_PID" 2>/dev/null || true
  kill "$MITM_PID" 2>/dev/null || true
}
trap cleanup EXIT

info "Starting echo server..."
python3 "$SCRIPT_DIR/echo_server.py" >/tmp/vs_target.log 2>&1 &
TARGET_PID=$!
sleep 0.5

# ── RelayCore baseline ──────────────────────────────────────────────

info "Running relay-core (port $RELAY_PORT)..."
CA_CERT="$SCRIPT_DIR/.bench_ca_cert.pem"
CA_KEY="$SCRIPT_DIR/.bench_ca_key.pem"

"$REPO_ROOT/target/release/relay-core-cli" run \
  --listen "127.0.0.1:$RELAY_PORT" \
  --ca-cert "$CA_CERT" --ca-key "$CA_KEY" \
  >/tmp/vs_relay.log 2>&1 &
RELAY_PID=$!
sleep 1.5

info "RelayCore benchmark (${DURATION}s, $CONNECTIONS conn)..."
relay_raw=$(run_oha "$RELAY_PORT")
relay_rps=$(echo "$relay_raw" | extract_rps)
relay_p99=$(echo "$relay_raw" | extract_p99)
pass "  RelayCore: ${relay_rps} req/s, P99: ${relay_p99}ms"

kill "$RELAY_PID" 2>/dev/null || true

# ── mitmproxy baseline ──────────────────────────────────────────────

mitm_rps=0
mitm_p99=0

if [[ "$MITM_AVAILABLE" -eq 1 ]]; then
  info "Running mitmproxy (port $PROXY_PORT)..."
  mitmdump --listen-port "$PROXY_PORT" -q >/tmp/vs_mitm.log 2>&1 &
  MITM_PID=$!
  sleep 1.5

  info "mitmproxy benchmark (${DURATION}s, $CONNECTIONS conn)..."
  mitm_raw=$(run_oha "$PROXY_PORT")
  mitm_rps=$(echo "$mitm_raw" | extract_rps)
  mitm_p99=$(echo "$mitm_raw" | extract_p99)
  pass "  mitmproxy: ${mitm_rps} req/s, P99: ${mitm_p99}ms"

  kill "$MITM_PID" 2>/dev/null || true

  # Calculate ratios
  if [[ "$mitm_rps" -gt 0 ]]; then
    rps_ratio=$(python3 -c "print(round(${relay_rps}/${mitm_rps}, 2))")
    p99_ratio=$(python3 -c "print(round(${relay_p99}/${mitm_p99}, 2))")
    pass "  RelayCore/mitmproxy throughput ratio: ${rps_ratio}x"
    pass "  RelayCore/mitmproxy P99 latency ratio: ${p99_ratio}x"
  fi
else
  info "  mitmproxy not installed — comparison skipped"
  rps_ratio="N/A"
  p99_ratio="N/A"
fi

# ── Report ──────────────────────────────────────────────────────────

python3 -c "
import json
report = {
    'timestamp': '$TIMESTAMP',
    'duration_s': $DURATION,
    'connections': $CONNECTIONS,
    'relay_core': {
        'throughput_rps': $relay_rps,
        'latency_p99_ms': $relay_p99,
    },
    'mitmproxy': {
        'throughput_rps': $mitm_rps,
        'latency_p99_ms': $mitm_p99,
    },
    'vs_mitmproxy': {
        'throughput_ratio': '$rps_ratio',
        'latency_p99_ratio': '$p99_ratio',
        'note': 'mitmproxy not installed — placeholder' if $MITM_AVAILABLE == 0 else 'direct comparison',
    },
}
with open('$OUT_JSON', 'w') as f:
    json.dump(report, f, indent=2)
print(f'Report: $OUT_JSON')
"

cat > "$OUT_MD" <<EOF
# RelayCore vs mitmproxy ($TIMESTAMP)

| Metric | RelayCore | mitmproxy | Ratio |
|---|---|---|---|
| Throughput (req/s) | $relay_rps | $mitm_rps | ${rps_ratio}x |
| P99 Latency (ms) | $relay_p99 | $mitm_p99 | ${p99_ratio}x |

Duration: ${DURATION}s, Connections: $CONNECTIONS
EOF

echo ""
pass "Comparison complete → $OUT_JSON"
cat "$OUT_JSON"
