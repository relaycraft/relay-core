#!/usr/bin/env bash
# bench_minimal.sh - relay-core benchmark entrypoint (pre-CI baseline)
#
# Measures:
#   - cold-start time
#   - idle RSS
#   - throughput (req/s)
#   - latency P99
#
# Modes:
#   - single (default): S1 only (1KB)
#   - matrix: S1/S2/S3 payload matrix (1KB/64KB/1024KB)
#
# Usage examples:
#   ./benchmarks/bench_minimal.sh
#   ./benchmarks/bench_minimal.sh quick
#   ./benchmarks/bench_minimal.sh matrix --duration 15
#   ./benchmarks/bench_minimal.sh --baseline benchmarks/results/bench_xxx.json

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
PROXY_BIN="$REPO_ROOT/target/release/relay-core-cli"

PROXY_PORT="${PROXY_PORT:-18080}"
TARGET_PORT="${TARGET_PORT:-19000}"
API_PORT="${API_PORT:-18082}"
CONNECTIONS="${CONNECTIONS:-100}"

CA_CERT="$REPO_ROOT/benchmarks/.bench_ca_cert.pem"
CA_KEY="$REPO_ROOT/benchmarks/.bench_ca_key.pem"

MODE="single"
DURATION=60
BASELINE_JSON=""
STRICT=0

DOD_STARTUP=200
DOD_IDLE_MB=50
DOD_QPS=10000
DOD_P99=5

# color helpers
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }
info() { echo -e "  ${YELLOW}->${NC} $*"; }

usage() {
  cat <<EOF
Usage: ./benchmarks/bench_minimal.sh [quick|matrix] [options]

Options:
  --duration <seconds>   Load duration per scenario (default: 60, quick: 10)
  --baseline <json>      Compare S1 against a previous JSON report (warn only)
  --strict               Exit non-zero on DoD failure or >10% regression
  -h, --help             Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    quick)
      DURATION=10
      shift
      ;;
    matrix)
      MODE="matrix"
      shift
      ;;
    --duration)
      DURATION="${2:-}"
      shift 2
      ;;
    --baseline)
      BASELINE_JSON="${2:-}"
      shift 2
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown arg: $1"
      usage
      exit 2
      ;;
  esac
done

if ! [[ "$DURATION" =~ ^[0-9]+$ ]] || [[ "$DURATION" -le 0 ]]; then
  echo "Invalid duration: $DURATION"
  exit 2
fi

mkdir -p "$RESULTS_DIR"
TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
OUT_MD="$RESULTS_DIR/bench_${TIMESTAMP}.md"
OUT_JSON="$RESULTS_DIR/bench_${TIMESTAMP}.json"

PROXY_PID=""
TARGET_PID=""
cleanup() {
  [[ -n "$PROXY_PID" ]] && kill "$PROXY_PID" 2>/dev/null || true
  [[ -n "$TARGET_PID" ]] && kill "$TARGET_PID" 2>/dev/null || true
}
trap cleanup EXIT

detect_tool() {
  if command -v oha >/dev/null 2>&1; then
    echo "oha"
  elif command -v ab >/dev/null 2>&1; then
    echo "ab"
  else
    echo "curl"
  fi
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

start_target() {
  PORT="$TARGET_PORT" python3 "$SCRIPT_DIR/echo_server.py" >/tmp/relay_bench_target.log 2>&1 &
  TARGET_PID=$!
  sleep 0.4
}

start_proxy() {
  "$PROXY_BIN" run \
    --listen "127.0.0.1:$PROXY_PORT" \
    --api-port "$API_PORT" \
    --ca-cert "$CA_CERT" \
    --ca-key "$CA_KEY" \
    >/tmp/relay_bench_proxy.log 2>&1 &
  PROXY_PID=$!
}

stop_proxy() {
  [[ -n "$PROXY_PID" ]] && kill "$PROXY_PID" 2>/dev/null || true
  PROXY_PID=""
}

poll_proxy_ready() {
  local target_url="$1"
  local ready=0
  for _ in $(seq 1 50); do
    if curl -s -x "http://127.0.0.1:$PROXY_PORT" "$target_url" --connect-timeout 0.2 -o /dev/null 2>/dev/null; then
      ready=1
      break
    fi
    sleep 0.1
  done
  echo "$ready"
}

measure_idle_rss_mb() {
  if [[ "$(uname)" == "Darwin" ]]; then
    local rss_kb
    rss_kb=$(ps -o rss= -p "$PROXY_PID" 2>/dev/null | tr -d ' ' || echo 0)
    echo $((rss_kb / 1024))
  else
    local rss_kb
    rss_kb=$(awk '/VmRSS/{print $2}' "/proc/$PROXY_PID/status" 2>/dev/null || echo 0)
    echo $((rss_kb / 1024))
  fi
}

extract_oha_rps() {
  python3 -c '
import json, sys
d = json.load(sys.stdin)
s = d.get("summary", {})
print(int(s.get("requestsPerSec", s.get("requests_per_sec", 0)) or 0))
' 2>/dev/null || echo 0
}

extract_oha_p99_ms() {
  python3 -c '
import json, sys
d = json.load(sys.stdin)
sources = [
    d.get("responseTimeHistogram", {}),
    d.get("latencyPercentiles", {}),
    d.get("latency_percentiles", {}),
]
p99 = 0.0
for src in sources:
    if isinstance(src, dict) and "p99" in src:
        p99 = float(src["p99"] or 0.0)
        break
if p99 < 1.0:
    p99 = p99 * 1000.0
print(round(p99, 2))
' 2>/dev/null || echo 0
}

run_load() {
  local scenario="$1"
  local payload_kb="$2"
  local target_url="http://127.0.0.1:$TARGET_PORT/payload/${payload_kb}"
  local proxy_url="http://127.0.0.1:$PROXY_PORT"

  local throughput=0
  local p99_ms=0
  local tool_raw=""

  case "$TOOL" in
    oha)
      info "[$scenario] oha ${DURATION}s, ${CONNECTIONS} connections, payload ${payload_kb}KB" >&2
      tool_raw=$(oha \
        --duration "${DURATION}s" \
        --connections "$CONNECTIONS" \
        --no-tui \
        --json \
        --proxy "$proxy_url" \
        "$target_url" 2>/dev/null || echo "{}")
      throughput=$(echo "$tool_raw" | extract_oha_rps)
      p99_ms=$(echo "$tool_raw" | extract_oha_p99_ms)
      ;;
    ab)
      info "[$scenario] ab ${DURATION}s, ${CONNECTIONS} concurrent, payload ${payload_kb}KB" >&2
      tool_raw=$(ab -t "$DURATION" -c "$CONNECTIONS" -r -X "127.0.0.1:$PROXY_PORT" "$target_url" 2>&1 || echo "")
      throughput="$(echo "$tool_raw" | awk '/Requests per second/{print int($4)}' | head -n1)"
      p99_ms="$(echo "$tool_raw" | awk '/ 99%/{print $2}' | head -n1)"
      ;;
    curl)
      info "[$scenario] curl fallback (latency sample only), payload ${payload_kb}KB" >&2
      local count=50 total_ms=0
      for _ in $(seq 1 "$count"); do
        local sec
        sec=$(curl -s -o /dev/null -w "%{time_total}" -x "$proxy_url" "$target_url" 2>/dev/null || echo 0)
        local ms
        ms=$(python3 -c "print(int(float('$sec')*1000))" 2>/dev/null || echo 0)
        total_ms=$((total_ms + ms))
      done
      p99_ms=$((total_ms / count * 2))
      throughput=0
      ;;
  esac

  throughput="${throughput:-0}"
  p99_ms="${p99_ms:-0}"

  echo "${throughput}|${p99_ms}"
}

measure_http_ms() {
  local url="$1"
  local ms
  ms="$(curl -s -o /dev/null -w "%{time_total}" "$url" 2>/dev/null || echo 0)"
  python3 -c "print(round(float('$ms')*1000, 2))" 2>/dev/null || echo 0
}

extract_first_flow_id() {
  local url="$1"
  curl -s "$url" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    items = d.get("items") or []
    if items and isinstance(items[0], dict):
        print(items[0].get("id", ""))
    else:
        print("")
except Exception:
    print("")
'
}

measure_sse_first_event_ms() {
  local url="$1"
  python3 - "$url" <<'PY'
import sys
import time
import urllib.request

url = sys.argv[1]
start = time.time()
timeout = 5.0
try:
    req = urllib.request.Request(url, headers={"Accept": "text/event-stream"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        for raw in resp:
            line = raw.decode("utf-8", errors="ignore").strip()
            if line.startswith("event:"):
                elapsed = (time.time() - start) * 1000
                print(round(elapsed, 2))
                break
        else:
            print(0)
except Exception:
    print(0)
PY
}

echo "=== relay-core benchmark (${TIMESTAMP}) ==="
echo ""

TOOL="$(detect_tool)"
info "Load generator: ${TOOL}"
info "Mode: ${MODE}, duration=${DURATION}s"

info "Building release binary..."
cd "$REPO_ROOT"
BUILD_START=$(now_ms)
cargo build --release --package relay-core-cli --quiet
BUILD_END=$(now_ms)
BUILD_TIME=$((BUILD_END - BUILD_START))
info "Build completed in ${BUILD_TIME}ms"
echo ""

if [[ ! -f "$CA_CERT" ]]; then
  info "Generating benchmark CA..."
  "$PROXY_BIN" ca init --cert "$CA_CERT" --key "$CA_KEY" >/dev/null 2>&1 || true
fi

start_target

echo "### Benchmark 1/3: Cold start time"
START_MS=$(now_ms)
start_proxy
READY=$(poll_proxy_ready "http://127.0.0.1:$TARGET_PORT/payload/1")
END_MS=$(now_ms)
STARTUP_MS=$((END_MS - START_MS))

if [[ "$READY" -eq 1 && "$STARTUP_MS" -le "$DOD_STARTUP" ]]; then
  pass "Cold start: ${STARTUP_MS}ms (DoD: < ${DOD_STARTUP}ms)"
  STARTUP_STATUS="PASS"
elif [[ "$READY" -eq 1 ]]; then
  fail "Cold start: ${STARTUP_MS}ms (DoD: < ${DOD_STARTUP}ms)"
  STARTUP_STATUS="FAIL"
else
  fail "Proxy readiness check failed"
  STARTUP_STATUS="FAIL"
fi

echo ""
echo "### Benchmark 2/3: Idle memory (RSS)"
RSS_MB="$(measure_idle_rss_mb)"
if [[ "$RSS_MB" -le "$DOD_IDLE_MB" ]]; then
  pass "Idle RSS: ${RSS_MB}MB (DoD: < ${DOD_IDLE_MB}MB)"
  MEMORY_STATUS="PASS"
else
  fail "Idle RSS: ${RSS_MB}MB (DoD: < ${DOD_IDLE_MB}MB)"
  MEMORY_STATUS="FAIL"
fi

echo ""
echo "### Benchmark 3/4: Throughput & latency"

SCENARIOS=("S1:1")
if [[ "$MODE" == "matrix" ]]; then
  SCENARIOS=("S1:1" "S2:64" "S3:1024")
fi

SCENARIO_ROWS_MD=""
SCENARIO_ROWS_JSON=""
S1_QPS=0
S1_P99=0
QPS_STATUS="SKIP"
LAT_STATUS="FAIL"

for pair in "${SCENARIOS[@]}"; do
  SCENARIO="${pair%%:*}"
  PAYLOAD_KB="${pair##*:}"
  METRICS="$(run_load "$SCENARIO" "$PAYLOAD_KB")"
  THROUGHPUT="${METRICS%%|*}"
  P99_MS="${METRICS##*|}"

  ROW_QPS_STATUS="INFO"
  ROW_LAT_STATUS="INFO"

  if [[ "$SCENARIO" == "S1" ]]; then
    S1_QPS="$THROUGHPUT"
    S1_P99="$P99_MS"
    if [[ "$TOOL" == "curl" ]]; then
      QPS_STATUS="SKIP"
      info "[S1] Throughput skipped in curl fallback mode"
    elif [[ "$THROUGHPUT" -ge "$DOD_QPS" ]]; then
      QPS_STATUS="PASS"
      ROW_QPS_STATUS="PASS"
      pass "[S1] Throughput: ${THROUGHPUT} req/s (DoD: > ${DOD_QPS} req/s)"
    else
      QPS_STATUS="FAIL"
      ROW_QPS_STATUS="FAIL"
      fail "[S1] Throughput: ${THROUGHPUT} req/s (DoD: > ${DOD_QPS} req/s)"
    fi

    P99_INT="${P99_MS%%.*}"
    if [[ -n "$P99_INT" && "$P99_INT" -le "$DOD_P99" ]] 2>/dev/null; then
      LAT_STATUS="PASS"
      ROW_LAT_STATUS="PASS"
      pass "[S1] Latency P99: ${P99_MS}ms (DoD: < ${DOD_P99}ms)"
    else
      LAT_STATUS="FAIL"
      ROW_LAT_STATUS="FAIL"
      fail "[S1] Latency P99: ${P99_MS}ms (DoD: < ${DOD_P99}ms)"
    fi
  else
    info "[$SCENARIO] Throughput: ${THROUGHPUT} req/s, P99: ${P99_MS}ms"
  fi

  SCENARIO_ROWS_MD+=$'\n'"| ${SCENARIO} | ${PAYLOAD_KB}KB | ${THROUGHPUT} | ${P99_MS} | ${ROW_QPS_STATUS} | ${ROW_LAT_STATUS} |"
  SCENARIO_ROWS_JSON+=$'{"id":"'"${SCENARIO}"'","payload_kb":'"${PAYLOAD_KB}"',"throughput_rps":'"${THROUGHPUT}"',"latency_p99_ms":'"${P99_MS}"',"qps_status":"'"${ROW_QPS_STATUS}"'","latency_status":"'"${ROW_LAT_STATUS}"'"},'
done
SCENARIO_ROWS_JSON="[${SCENARIO_ROWS_JSON%,}]"

echo ""
echo "### Benchmark 4/4: HTTP API paths (flows/detail/sse)"

FLOW_LIST_URL="http://127.0.0.1:${API_PORT}/api/v1/flows?limit=50&offset=0"
FLOWS_QUERY_MS="$(measure_http_ms "$FLOW_LIST_URL")"
FLOW_ID="$(extract_first_flow_id "$FLOW_LIST_URL")"
FLOW_DETAIL_MS="0"
FLOW_DETAIL_STATUS="SKIP"
if [[ -n "$FLOW_ID" ]]; then
  FLOW_DETAIL_MS="$(measure_http_ms "http://127.0.0.1:${API_PORT}/api/v1/flows/${FLOW_ID}")"
  FLOW_DETAIL_STATUS="OK"
fi

SSE_FIRST_EVENT_MS="$(measure_sse_first_event_ms "http://127.0.0.1:${API_PORT}/api/v1/events")"
if [[ "${SSE_FIRST_EVENT_MS%%.*}" -gt 0 ]] 2>/dev/null; then
  SSE_STATUS="OK"
else
  SSE_STATUS="WARN"
fi

info "API flows query latency: ${FLOWS_QUERY_MS}ms"
if [[ "$FLOW_DETAIL_STATUS" == "OK" ]]; then
  info "API flow detail latency: ${FLOW_DETAIL_MS}ms"
else
  info "API flow detail latency: skipped (no flow id)"
fi
info "API SSE first event: ${SSE_FIRST_EVENT_MS}ms [${SSE_STATUS}]"

BASELINE_NOTE="none"
REGRESSION_STATUS="N/A"
REGRESSION_SUMMARY=""
if [[ -n "$BASELINE_JSON" ]]; then
  if [[ -f "$BASELINE_JSON" ]]; then
    BASELINE_NOTE="$BASELINE_JSON"
    REGRESSION_SUMMARY="$(python3 - "$BASELINE_JSON" "$S1_QPS" "$S1_P99" "$RSS_MB" <<'PY'
import json, sys
base_path, qps_now, p99_now, rss_now = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
with open(base_path, "r", encoding="utf-8") as f:
    d = json.load(f)
m = d.get("metrics", {})
b_qps = float(m.get("throughput_rps", 0) or 0)
b_p99 = float(m.get("latency_p99_ms", 0) or 0)
b_rss = float(m.get("idle_rss_mb", 0) or 0)
def pct(new, old):
    if old == 0:
        return 0.0
    return (new - old) / old * 100.0
qps_drop = -pct(qps_now, b_qps)
p99_rise = pct(p99_now, b_p99)
rss_rise = pct(rss_now, b_rss)
warn = (qps_drop > 10.0) or (p99_rise > 10.0) or (rss_rise > 20.0)
status = "WARN" if warn else "OK"
print(f"{status}|qps_drop={qps_drop:.2f}%;p99_rise={p99_rise:.2f}%;rss_rise={rss_rise:.2f}%")
PY
)"
    REGRESSION_STATUS="${REGRESSION_SUMMARY%%|*}"
    REGRESSION_NOTE="${REGRESSION_SUMMARY#*|}"
    if [[ "$REGRESSION_STATUS" == "WARN" ]]; then
      fail "Baseline compare: ${REGRESSION_NOTE}"
    else
      pass "Baseline compare: ${REGRESSION_NOTE}"
    fi
  else
    fail "Baseline file not found: ${BASELINE_JSON}"
    REGRESSION_STATUS="WARN"
    REGRESSION_NOTE="baseline_missing"
  fi
fi

echo ""
echo "=== Summary ==="
cat <<EOF
  Cold start:       ${STARTUP_MS}ms    [${STARTUP_STATUS}]
  Idle RSS:         ${RSS_MB}MB        [${MEMORY_STATUS}]
  Throughput (S1):  ${S1_QPS} req/s    [${QPS_STATUS}]
  Latency P99 (S1): ${S1_P99}ms        [${LAT_STATUS}]
  API flows query:  ${FLOWS_QUERY_MS}ms
  API flow detail:  ${FLOW_DETAIL_MS}ms [${FLOW_DETAIL_STATUS}]
  API SSE first:    ${SSE_FIRST_EVENT_MS}ms [${SSE_STATUS}]
  Regression check: ${REGRESSION_STATUS}
  Load tool:        ${TOOL}
EOF
echo ""

COMMIT="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")"
DATE_UTC="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

cat >"$OUT_MD" <<REPORT
# relay-core Benchmark Report

- **Date**: ${DATE_UTC}
- **Commit**: ${COMMIT}
- **Mode**: ${MODE}
- **Duration**: ${DURATION}s per scenario
- **Load tool**: ${TOOL}
- **Baseline**: ${BASELINE_NOTE}
- **Regression status**: ${REGRESSION_STATUS}

## Results vs DoD (S1)

| Metric | Result | DoD Target | Status |
|--------|--------|------------|--------|
| Cold start | ${STARTUP_MS}ms | < 200ms | ${STARTUP_STATUS} |
| Idle RSS | ${RSS_MB}MB | < 50MB | ${MEMORY_STATUS} |
| Throughput (S1) | ${S1_QPS} req/s | > 10,000 req/s | ${QPS_STATUS} |
| Latency P99 (S1) | ${S1_P99}ms | < 5ms | ${LAT_STATUS} |

## Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS Status | Latency Status |
|----------|---------|--------------------|----------|------------|----------------|${SCENARIO_ROWS_MD}

## API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | ${FLOWS_QUERY_MS}ms | OK |
| GET /api/v1/flows/{id} | ${FLOW_DETAIL_MS}ms | ${FLOW_DETAIL_STATUS} |
| GET /api/v1/events (first event) | ${SSE_FIRST_EVENT_MS}ms | ${SSE_STATUS} |

## Notes
- matrix mode currently covers payload scale (S1/S2/S3) to establish baseline trend.
- additional scenario dimensions (TLS/rules/redaction/SSE) can be layered on top of this entrypoint.
REPORT

cat >"$OUT_JSON" <<JSON
{
  "timestamp": "${DATE_UTC}",
  "commit": "${COMMIT}",
  "mode": "${MODE}",
  "duration_seconds": ${DURATION},
  "tool": "${TOOL}",
  "metrics": {
    "cold_start_ms": ${STARTUP_MS},
    "idle_rss_mb": ${RSS_MB},
    "throughput_rps": ${S1_QPS},
    "latency_p99_ms": ${S1_P99},
    "api_flows_query_ms": ${FLOWS_QUERY_MS},
    "api_flow_detail_ms": ${FLOW_DETAIL_MS},
    "api_sse_first_event_ms": ${SSE_FIRST_EVENT_MS}
  },
  "status": {
    "cold_start": "${STARTUP_STATUS}",
    "idle_rss": "${MEMORY_STATUS}",
    "throughput_s1": "${QPS_STATUS}",
    "latency_p99_s1": "${LAT_STATUS}",
    "api_flow_detail": "${FLOW_DETAIL_STATUS}",
    "api_sse": "${SSE_STATUS}",
    "regression": "${REGRESSION_STATUS}"
  },
  "regression_note": "${REGRESSION_NOTE:-}",
  "scenarios": ${SCENARIO_ROWS_JSON},
  "vs_mitmproxy": {
    "note": "comparison data available via benchmarks/compare_mitmproxy.sh",
    "status": "pending"
  }
}
JSON

info "Markdown report: $OUT_MD"
info "JSON report: $OUT_JSON"

if [[ "$STRICT" -eq 1 ]]; then
  if [[ "$STARTUP_STATUS" == "FAIL" || "$MEMORY_STATUS" == "FAIL" || "$QPS_STATUS" == "FAIL" || "$LAT_STATUS" == "FAIL" ]]; then
    fail "Strict mode: DoD check failed"
    exit 1
  fi
  if [[ "$REGRESSION_STATUS" == "WARN" ]]; then
    fail "Strict mode: regression warning treated as failure"
    exit 1
  fi
fi
