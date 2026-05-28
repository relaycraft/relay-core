#!/usr/bin/env bash
# stability_test.sh - relay-core long-stability validation (M7 B4)
#
# Runs the proxy under sustained load for the configured duration,
# monitoring RSS growth and checking for panics/errors.
#
# Usage:
#   ./benchmarks/stability_test.sh              # 2h (default for M7)
#   ./benchmarks/stability_test.sh --duration 24 # 24h (nightly only)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

PROXY_PORT="${PROXY_PORT:-18080}"
TARGET_PORT="${TARGET_PORT:-19000}"
DURATION_HOURS="${DURATION_HOURS:-2}"
CONNECTIONS=100

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration) DURATION_HOURS="${2:-2}"; shift 2 ;;
    --connections) CONNECTIONS="${2:-100}"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

DURATION_SEC=$((DURATION_HOURS * 3600))
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
OUT_FILE="$RESULTS_DIR/stability_${TIMESTAMP}.csv"
OUT_SUMMARY="$RESULTS_DIR/stability_${TIMESTAMP}.md"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "  ${YELLOW}->${NC} $*"; }
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }

detect_tool() {
  command -v oha >/dev/null 2>&1 && echo "oha" || echo "ab"
}

has_tool() { command -v "$1" >/dev/null 2>&1; }

cleanup() {
  kill "$TARGET_PID" 2>/dev/null || true
  kill "$PROXY_PID" 2>/dev/null || true
}
trap cleanup EXIT

# ── Dependency checks ───────────────────────────────────────────────

if ! has_tool oha && ! has_tool ab; then
  fail "Need oha or ab for load generation"
  exit 1
fi

info "Stability test: ${DURATION_HOURS}h duration"

cd "$REPO_ROOT"

info "Building release binary..."
cargo build --release --package relay-core-cli --quiet 2>/dev/null || {
  fail "Build failed"
  exit 1
}

CA_CERT="$SCRIPT_DIR/.bench_ca_cert.pem"
CA_KEY="$SCRIPT_DIR/.bench_ca_key.pem"

if [[ ! -f "$CA_CERT" ]]; then
  info "Generating benchmark CA..."
  "$REPO_ROOT/target/release/relay-core-cli" ca init --cert "$CA_CERT" --key "$CA_KEY" >/dev/null 2>&1 || true
fi

# ── Start target server ─────────────────────────────────────────────

info "Starting echo server..."
PORT="$TARGET_PORT" python3 "$SCRIPT_DIR/echo_server.py" >/tmp/stability_target.log 2>&1 &
TARGET_PID=$!
sleep 0.5

# ── Start proxy ─────────────────────────────────────────────────────

info "Starting relay-core proxy..."
"$REPO_ROOT/target/release/relay-core-cli" run \
  --listen "127.0.0.1:$PROXY_PORT" \
  --ca-cert "$CA_CERT" --ca-key "$CA_KEY" \
  >/tmp/stability_proxy.log 2>&1 &
PROXY_PID=$!
sleep 1.5

# Verify proxy is alive
check_alive() {
  kill -0 "$PROXY_PID" 2>/dev/null
}

collect_rss_mb() {
  if [[ "$(uname)" == "Darwin" ]]; then
    ps -o rss= -p "$PROXY_PID" 2>/dev/null | tr -d ' ' | awk '{print int($1/1024)}' || echo 0
  else
    awk '/VmRSS/{print int($2/1024)}' "/proc/$PROXY_PID/status" 2>/dev/null || echo 0
  fi
}

# ── CSV header ──────────────────────────────────────────────────────

echo "elapsed_s,rss_mb,proxy_alive" > "$OUT_FILE"

LOAD_TOOL="$(detect_tool)"

info "Load generator: $LOAD_TOOL"
info "Duration: ${DURATION_HOURS}h (${DURATION_SEC}s)"
info "Monitoring RSS every 60s..."

START_TIME=$(date +%s)
PANIC_COUNT=0
SAMPLE_INTERVAL=60
LAST_SAMPLE=0

# Start load in background
(
  while true; do
    case "$LOAD_TOOL" in
      oha)
        oha --duration 60 --connections "$CONNECTIONS" --no-tui \
          --proxy "http://127.0.0.1:$PROXY_PORT" \
          "http://127.0.0.1:$TARGET_PORT/payload/1" \
          >/dev/null 2>&1 || true
        ;;
      ab)
        ab -t 60 -c "$CONNECTIONS" -r \
          -X "127.0.0.1:$PROXY_PORT" \
          "http://127.0.0.1:$TARGET_PORT/payload/1" \
          >/dev/null 2>&1 || true
        ;;
    esac
  done
) &
LOAD_PID=$!

# ── Monitoring loop ─────────────────────────────────────────────────

while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START_TIME))

  if [[ "$ELAPSED" -ge "$DURATION_SEC" ]]; then
    break
  fi

  if [[ $((ELAPSED - LAST_SAMPLE)) -ge "$SAMPLE_INTERVAL" ]]; then
    LAST_SAMPLE=$ELAPSED

    if check_alive; then
      RSS=$(collect_rss_mb)
      echo "${ELAPSED},${RSS},1" >> "$OUT_FILE"
      ELAPSED_MIN=$((ELAPSED / 60))
      ELAPSED_H=$((ELAPSED_MIN / 60))
      ELAPSED_M=$((ELAPSED_MIN % 60))
      info "  ${ELAPSED_H}h${ELAPSED_M}m: RSS ${RSS}MB, alive=yes"
    else
      PANIC_COUNT=$((PANIC_COUNT + 1))
      echo "${ELAPSED},0,0" >> "$OUT_FILE"
      fail "  Proxy process died at ${ELAPSED}s!"

      # Check log for panic
      if grep -q "panic" /tmp/stability_proxy.log 2>/dev/null; then
        fail "  PANIC detected in proxy log:"
        grep -A 5 "panic" /tmp/stability_proxy.log | tail -10
      fi
      break
    fi
  fi

  sleep 5
done

# ── Cleanup ─────────────────────────────────────────────────────────

kill "$LOAD_PID" 2>/dev/null || true
ELAPSED_TOTAL=$(($(date +%s) - START_TIME))

# ── Analysis ────────────────────────────────────────────────────────

if [[ -f "$OUT_FILE" ]]; then
  RSS_START=$(awk -F',' 'NR==2{print $2}' "$OUT_FILE")
  RSS_END=$(tail -1 "$OUT_FILE" | awk -F',' '{print $2}')
  RSS_MIN=$(awk -F',' 'NR>1{print $2}' "$OUT_FILE" | sort -n | head -1)
  RSS_MAX=$(awk -F',' 'NR>1{print $2}' "$OUT_FILE" | sort -n | tail -1)

  # Check for monotonic growth (>20% increase over duration)
  RSS_GROWTH_PCT=0
  if [[ "$RSS_START" -gt 0 ]]; then
    RSS_GROWTH_PCT=$(python3 -c "print(round(($RSS_END - $RSS_START) / $RSS_START * 100, 1))" 2>/dev/null || echo 0)
  fi

  cat > "$OUT_SUMMARY" <<EOF
# Stability Test Report ($TIMESTAMP)

- **Duration**: ${ELAPSED_TOTAL}s (target: ${DURATION_HOURS}h)
- **Panics**: ${PANIC_COUNT}
- **Proxy alive**: $(if [[ "$PANIC_COUNT" -eq 0 ]]; then echo "yes"; else echo "no"; fi)

## Memory (RSS)

| Metric | Value |
|---|---|
| Start | ${RSS_START} MB |
| End | ${RSS_END} MB |
| Min | ${RSS_MIN} MB |
| Max | ${RSS_MAX} MB |
| Growth | ${RSS_GROWTH_PCT}% |

## Verdict

EOF

  STABILITY_PASS=1
  if [[ "$PANIC_COUNT" -gt 0 ]]; then
    echo "**FAIL**: ${PANIC_COUNT} panic(s) detected" >> "$OUT_SUMMARY"
    STABILITY_PASS=0
  fi

  # Check RSS growth: if growth > 20%, it's a warning; if > 50%, it's a failure
  RSS_GROWTH_NUM=$(echo "$RSS_GROWTH_PCT" | sed 's/%//')
  if (( $(echo "$RSS_GROWTH_NUM > 50" | bc -l 2>/dev/null) )); then
    echo "**FAIL**: RSS growth ${RSS_GROWTH_PCT}% exceeds 50% threshold" >> "$OUT_SUMMARY"
    STABILITY_PASS=0
  elif (( $(echo "$RSS_GROWTH_NUM > 20" | bc -l 2>/dev/null) )); then
    echo "**WARN**: RSS growth ${RSS_GROWTH_PCT}% exceeds 20% (monotonic increase suspected)" >> "$OUT_SUMMARY"
  else
    echo "**PASS**: No panics, RSS growth within limits" >> "$OUT_SUMMARY"
  fi

  if [[ "$STABILITY_PASS" -eq 1 ]]; then
    pass "Stability: PASS (0 panics, RSS ${RSS_START}→${RSS_END}MB, growth ${RSS_GROWTH_PCT}%)"
  else
    fail "Stability: FAIL"
  fi

  info "Report: $OUT_SUMMARY"
  info "Raw data: $OUT_FILE"
fi