#!/usr/bin/env bash
# stability_test.sh - relay-core long-stability validation
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

# Where the binary is, resolved rather than assumed. `$REPO_ROOT/target/release` is only correct when
# cargo's target directory is the default: on a machine that sets `build.target-dir` (this project's
# does, to an external volume) the path is somewhere else entirely, and the script used to fail at
# "starting the proxy" for a reason that has nothing to do with the proxy.
cargo_target_dir() {
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    echo "$CARGO_TARGET_DIR"
    return
  fi
  local configured
  configured="$(cd "$REPO_ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory",""))' 2>/dev/null)"
  if [[ -n "$configured" ]]; then
    echo "$configured"
  else
    echo "$REPO_ROOT/target"
  fi
}
TARGET_DIR="$(cargo_target_dir)"
PROXY_BIN="${RELAY_CORE_BIN:-$TARGET_DIR/release/relay-core-cli}"
UPSTREAM_BIN_DEFAULT="$TARGET_DIR/release/rust_echo_server"

PROXY_PORT="${PROXY_PORT:-18080}"
TARGET_PORT="${TARGET_PORT:-19000}"
DURATION_HOURS="${DURATION_HOURS:-2}"
# Seconds, for a soak that has to fit a CI job. Kept alongside the hours form because the historical
# invocations use hours and the report quotes them.
DURATION_MINUTES=""
CONNECTIONS=100

while [[ $# -gt 0 ]]; do
  case "$1" in
    --duration) DURATION_HOURS="${2:-2}"; shift 2 ;;
    --minutes) DURATION_MINUTES="${2:-}"; shift 2 ;;
    --connections) CONNECTIONS="${2:-100}"; shift 2 ;;
    -h|--help)
      cat <<'EOF'
Usage: ./benchmarks/stability_test.sh [options]

Options:
  --duration <hours>      Soak length in hours (default 2; the historical form)
  --minutes <minutes>     Soak length in minutes, for a CI-sized run
  --connections <n>       Concurrent load connections (default 100)
  -h, --help              Show this help

Reads the proxy's RSS from /proc, so this runs on Linux. A run whose upstream fell back to
the Python echo server is upstream-bound and says so in its report: it is then not a statement
about the proxy's memory behaviour.
EOF
      exit 0
      ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

if [[ -n "$DURATION_MINUTES" ]]; then
  DURATION_SEC=$((DURATION_MINUTES * 60))
  DURATION_LABEL="${DURATION_MINUTES}m"
else
  DURATION_SEC=$((DURATION_HOURS * 3600))
  DURATION_LABEL="${DURATION_HOURS}h"
fi
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
OUT_FILE="$RESULTS_DIR/stability_${TIMESTAMP}.csv"
OUT_SUMMARY="$RESULTS_DIR/stability_${TIMESTAMP}.md"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "  ${YELLOW}->${NC} $*"; }
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }

detect_tool() {
  command -v oha >/dev/null 2>&1 && echo "oha" || echo ""
}

has_tool() { command -v "$1" >/dev/null 2>&1; }

cleanup() {
  kill "$TARGET_PID" 2>/dev/null || true
  kill "$PROXY_PID" 2>/dev/null || true
}
trap cleanup EXIT

if ! has_tool oha; then
  fail "oha is required. Install: brew install oha  or  cargo install oha"
  exit 1
fi

info "Stability test: ${DURATION_LABEL} duration"

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
  "$PROXY_BIN" ca generate --ca-cert "$CA_CERT" --ca-key "$CA_KEY" >/dev/null 2>&1 || true
fi

# ── An isolated data directory ──────────────────────────────────────
#
# `relay-core-cli run` is a daemon since 0.13: it takes a lock in the data directory and refuses to
# start when one is already owned there. A soak that shares `~/.relay-core` therefore fails whenever
# the person running it also has a daemon open — and it would record its load traffic in that
# person's history. The benchmark harness solved the same problem the same way; this script had not.
if [[ -z "${RELAY_DATA_DIR:-}" ]]; then
  RELAY_DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/relay-soak-XXXXXX")"
  export RELAY_DATA_DIR
  SOAK_DATA_DIR_CREATED=1
fi

cleanup() {
  [[ -n "${PROXY_PID:-}" ]] && kill "$PROXY_PID" 2>/dev/null || true
  [[ -n "${TARGET_PID:-}" ]] && kill "$TARGET_PID" 2>/dev/null || true
  if [[ "${SOAK_DATA_DIR_CREATED:-0}" == "1" ]]; then
    rm -rf "$RELAY_DATA_DIR"
  fi
}
trap cleanup EXIT

# ── Start target server ─────────────────────────────────────────────

# Which upstream, and why it matters here more than anywhere: `echo_server.py` saturates near 2.7k
# req/s (it is HTTP/1.0 and closes every connection), so a soak against it measures *the upstream*, and
# the proxy's own memory behaviour under sustained load — the thing a soak exists to see — never
# happens. `benchmarks/rust_echo_server.rs` is the upstream the benchmark harness already uses for the
# same reason. Falling back to Python is allowed but said out loud, because a result that describes
# something else must not look like a result about the proxy.
UPSTREAM_BIN="${UPSTREAM_BIN:-$UPSTREAM_BIN_DEFAULT}"
if [[ -x "$UPSTREAM_BIN" ]]; then
  info "Starting the Rust echo server (the proxy is what is under test)"
  PORT="$TARGET_PORT" "$UPSTREAM_BIN" >/tmp/stability_target.log 2>&1 &
  UPSTREAM_DESCRIPTION="rust (native, ~100k req/s direct)"
else
  if command -v rustc >/dev/null 2>&1; then
    info "Building the Rust echo server (one-off)..."
    mkdir -p "$(dirname "$UPSTREAM_BIN")"
    rustc -O -o "$UPSTREAM_BIN" "$SCRIPT_DIR/rust_echo_server.rs" >/dev/null 2>&1 || true
  fi
  if [[ -x "$UPSTREAM_BIN" ]]; then
    info "Starting the Rust echo server (the proxy is what is under test)"
    PORT="$TARGET_PORT" "$UPSTREAM_BIN" >/tmp/stability_target.log 2>&1 &
    UPSTREAM_DESCRIPTION="rust (native, ~100k req/s direct)"
  else
    info "Falling back to the Python upstream: this run measures the upstream ceiling, not the proxy"
    PORT="$TARGET_PORT" python3 "$SCRIPT_DIR/echo_server.py" >/tmp/stability_target.log 2>&1 &
    UPSTREAM_DESCRIPTION="python (saturates near 2.7k req/s — the run is upstream-bound)"
  fi
fi
TARGET_PID=$!
sleep 0.5

# ── Start proxy ─────────────────────────────────────────────────────

info "Starting relay-core proxy..."
"$PROXY_BIN" run \
  --listen "127.0.0.1:$PROXY_PORT" \
  --ca-cert "$CA_CERT" --ca-key "$CA_KEY" \
  >/tmp/stability_proxy.log 2>&1 &
PROXY_PID=$!
sleep 1.5

# Say so now, with the reason, rather than discovering it 60 seconds later as "the process died" —
# a failed start and a crash under load are different findings and must not look the same.
if ! kill -0 "$PROXY_PID" 2>/dev/null; then
  fail "The proxy did not start. Its log says:"
  sed 's/^/    /' /tmp/stability_proxy.log 2>/dev/null | tail -5 || true
  exit 1
fi

# Verify proxy is alive
check_alive() {
  kill -0 "$PROXY_PID" 2>/dev/null
}

# Does the measurement work at all? A `ps`/`/proc` read that fails yields 0, and a verdict computed
# from a start of 0 reports "no growth" — the same defect `bench_minimal.sh` fixed with its own
# `assert_rss_measurable`: a number nobody could read must never look like a number that was fine.
# Measurements actually written, not lines: the CSV has a header, and counting it as a sample is how
# a run with no samples at all still produced a verdict — from the header row's second field.
data_rows() {
  tail -n +2 "$OUT_FILE" 2>/dev/null | wc -l | tr -d ' '
}

assert_rss_measurable() {
  local rss="$1"
  if [[ "${rss:-0}" -le 0 ]]; then
    fail "RSS could not be read for pid ${PROXY_PID:-?}: a soak whose memory is unmeasurable cannot report a verdict"
    return 1
  fi
  return 0
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
info "Duration: ${DURATION_LABEL} (${DURATION_SEC}s)"
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
        oha -z 60 -c "$CONNECTIONS" --no-tui \
          -x "http://127.0.0.1:$PROXY_PORT" \
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
      # The first sample decides whether a verdict is possible at all: once the loop is running, a
      # zero would be written, analysed, and reported as no growth.
      if [[ "$(data_rows)" -eq 0 ]] && ! assert_rss_measurable "$RSS"; then
        kill "$PROXY_PID" 2>/dev/null || true
        kill "$TARGET_PID" 2>/dev/null || true
        exit 1
      fi
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

  # A trend needs two points at least. With fewer, there is nothing to judge, and the previous
  # behaviour — compute from whatever is in the file and call it a pass — turned "the run measured
  # nothing" into "the run found no problem". The header row is not a measurement.
  if [[ "$(data_rows)" -lt 2 ]]; then
    cat > "$OUT_SUMMARY" <<EOF
# Stability Test Report ($TIMESTAMP)

- **Duration**: ${ELAPSED_TOTAL}s (target: ${DURATION_LABEL})
- **Upstream**: ${UPSTREAM_DESCRIPTION}
- **Samples**: $(data_rows)

## Verdict

**FAIL**: no usable measurement. A soak needs at least two RSS samples to say anything about
growth, and this run produced $(data_rows). Sampling happens every 60s, so a duration shorter than
that cannot produce a trend — run it for minutes, not seconds. No conclusion about the proxy is
drawn from this run.
EOF
    fail "Stability: FAIL — no usable measurement ($(data_rows) sample(s))"
    info "Report: $OUT_SUMMARY"
    exit 1
  fi

  # Check for monotonic growth (>20% increase over duration)
  RSS_GROWTH_PCT=0
  if [[ "$RSS_START" -gt 0 ]]; then
    RSS_GROWTH_PCT=$(python3 -c "print(round(($RSS_END - $RSS_START) / $RSS_START * 100, 1))" 2>/dev/null || echo 0)
  fi

  cat > "$OUT_SUMMARY" <<EOF
# Stability Test Report ($TIMESTAMP)

- **Duration**: ${ELAPSED_TOTAL}s (target: ${DURATION_LABEL})
- **Upstream**: ${UPSTREAM_DESCRIPTION}
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
  if ! assert_rss_measurable "$RSS_END"; then
    echo "**FAIL**: the final RSS sample could not be read, so no trend was measured" >> "$OUT_SUMMARY"
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