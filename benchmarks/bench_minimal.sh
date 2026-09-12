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
#   - release: multi-round with stats, environment capture, reproducible report
#   - ramp: stepped concurrency load test to find throughput saturation point
#
# Usage examples:
#   ./benchmarks/bench_minimal.sh
#   ./benchmarks/bench_minimal.sh quick
#   ./benchmarks/bench_minimal.sh matrix --duration 15
#   ./benchmarks/bench_minimal.sh --baseline benchmarks/results/bench_xxx.json
#   ./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
#   ./benchmarks/bench_minimal.sh ramp --duration 10
#   ./benchmarks/bench_minimal.sh ramp --tls --duration 10

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"

# Honour CARGO_TARGET_DIR (and cargo's config/default) instead of assuming $REPO_ROOT/target.
# `~/.cargo/config.toml` may redirect target-dir outside the repo, in which case the old
# hardcoded path silently pointed at a binary that was never built there.
cargo_target_dir() {
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    echo "$CARGO_TARGET_DIR"
    return
  fi
  local configured
  configured="$(
    cargo metadata --format-version 1 --no-deps 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory",""))' 2>/dev/null
  )"
  if [[ -n "$configured" ]]; then
    echo "$configured"
  else
    echo "$REPO_ROOT/target"
  fi
}

TARGET_DIR="$(cargo_target_dir)"
PROXY_BIN="${RELAY_CORE_BIN:-$TARGET_DIR/release/relay-core-cli}"

# Defaults deliberately avoid the crowded 18xxx development range: 18080 is a popular app port
# and is already published by a local dev container on some machines, which previously let a
# foreign listener silently serve the benchmark load. Override with PROXY_PORT/API_PORT/TARGET_PORT.
PROXY_PORT="${PROXY_PORT:-18880}"
TARGET_PORT="${TARGET_PORT:-19100}"
API_PORT="${API_PORT:-18882}"
# Concurrency for the load generator.
#
# 100 is still unusable, but NOT for the reason previously recorded here. The old note claimed
# "RelayCore answers with `Connection: close`"; that is false — a socket-level test
# (`wire_matrix_client_connection_is_reused_for_a_second_request`) reuses one client connection for
# two requests through the proxy. The proxy relays the upstream's own close semantics, and the
# symptom was produced by the then-default Python upstream (`echo_server.py`), which speaks HTTP/1.0
# and closes after every response.
#
# Measured at CONNECTIONS=100 with the current Rust upstream (10s): 34,532 req/s, P99 21.7ms,
# success 62.6%, with 439 upstream connect failures and 383 `Circuit breaker OPEN` events in the
# proxy log. So a 0.08% transient upstream error rate becomes a 37% failure rate: 3 connect failures
# open a 30s per-host circuit (`proxy/circuit_breaker.rs:90`), and everything to that host is then
# rejected. That is neither a load-generator artifact nor a proxy regression, and `os error 49` does
# NOT appear — so the INVALID detection below cannot classify it and it will be reported as FAIL.
# See docs/engine-capability-status.md (benchmark root cause).
#
# 25 connections sustains >=45k req/s with ~1% variance and 100% success, so it measures the proxy
# rather than the harness. Raise it deliberately (with a shorter --duration) if a run needs higher
# concurrency; expect port exhaustion beyond roughly 50.
CONNECTIONS="${CONNECTIONS:-25}"

CA_CERT="$REPO_ROOT/benchmarks/.bench_ca_cert.pem"
CA_KEY="$REPO_ROOT/benchmarks/.bench_ca_key.pem"

MODE="single"
DURATION=60
RUNS=5
WARMUP_RUNS=3
TLS_MODE=0
# Upstream implementation: "rust" (fast, plaintext only) or "python" (slow, supports --tls).
UPSTREAM="${UPSTREAM:-rust}"
REPORT_VERSION=""
BASELINE_JSON=""
STRICT=0

# DoD thresholds — calibrated for single-machine localhost testing.
# Apple Silicon (M-series) uses 16KB pages (vs 4KB on x86), inflating RSS ~1.5x.
# P99 is conservative for same-machine (oha + proxy + echo compete for CPU);
# isolated-setup measurements typically achieve <5ms.
#
# Upstream note: the default upstream is `benchmarks/rust_echo_server.rs` (~100k req/s direct on an
# M4 Max), so these numbers describe the proxy. `UPSTREAM=python` or `--tls` falls back to
# `echo_server.py`, which saturates near 2.7k req/s — below the DoD — and will show throughput
# FAIL for upstream-bound reasons. Measured reference (M4 Max, 100 connections, 1KB payload):
# direct upstream ~104k req/s, through RelayCore ~43k req/s.
DOD_STARTUP=200
DOD_IDLE_MB=85
DOD_QPS=10000
DOD_P99=20
# Minimum share of responses that must be 2xx/3xx for a run to count as a valid measurement.
DOD_SUCCESS_RATE=99.0

# color helpers
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }
info() { echo -e "  ${YELLOW}->${NC} $*"; }

usage() {
  cat <<EOF
Usage: ./benchmarks/bench_minimal.sh [quick|matrix|release|ramp] [options]

Options:
  --duration <seconds>   Load duration per round (default: 60, quick: 10)
  --runs <n>             Measurement rounds for release mode (default: 5)
  --warmup-runs <n>      Warmup rounds before measurement (default: 3)
  --baseline <json>      Compare S1 against a previous JSON report (warn only)
  --version <semver>     Label release reports (use target release, e.g. 0.8.2)
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
    ramp)
      MODE="ramp"
      DURATION="${DURATION:-15}"
      shift
      ;;
    release)
      MODE="release"
      DURATION="${DURATION:-30}"
      shift
      ;;
    --duration)
      DURATION="${2:-}"
      shift 2
      ;;
    --runs)
      RUNS="${2:-5}"
      shift 2
      ;;
    --warmup-runs)
      WARMUP_RUNS="${2:-3}"
      shift 2
      ;;
    --baseline)
      BASELINE_JSON="${2:-}"
      shift 2
      ;;
    --version)
      REPORT_VERSION="${2#v}"
      shift 2
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    --tls)
      TLS_MODE=1
      shift
      ;;
    --ramp)
      MODE="ramp"
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
  if ! command -v oha >/dev/null 2>&1; then
    echo ""
    return 1
  fi
  echo "oha"
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

# Build the fast Rust upstream once. The Python echo server saturates near 2.7k req/s, so any
# throughput number measured against it described the upstream rather than the proxy.
UPSTREAM_BIN="$SCRIPT_DIR/.bin/rust_echo_server"
build_upstream() {
  if [[ "$UPSTREAM" == "python" ]]; then
    return 0
  fi
  if [[ -x "$UPSTREAM_BIN" && "$UPSTREAM_BIN" -nt "$SCRIPT_DIR/rust_echo_server.rs" ]]; then
    return 0
  fi
  if ! command -v rustc >/dev/null 2>&1; then
    info "rustc not found; falling back to the Python upstream (~2.7k req/s ceiling)"
    UPSTREAM="python"
    return 0
  fi
  mkdir -p "$SCRIPT_DIR/.bin"
  info "Building Rust upstream (one-off)..."
  if ! rustc -O -o "$UPSTREAM_BIN" "$SCRIPT_DIR/rust_echo_server.rs" 2>/tmp/relay_bench_upstream_build.log; then
    info "Rust upstream build failed; falling back to Python (~2.7k req/s ceiling)"
    UPSTREAM="python"
    return 0
  fi
  return 0
}

start_target() {
  # TLS mode still uses the Python server: the Rust upstream speaks plaintext only.
  if [[ "$TLS_MODE" -eq 1 ]]; then
    local tls_cert="$SCRIPT_DIR/.tls/echo-cert.pem"
    local tls_key="$SCRIPT_DIR/.tls/echo-key.pem"
    if [[ ! -f "$tls_cert" ]]; then
      info "Generating self-signed TLS cert for echo server..."
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout "$tls_key" -out "$tls_cert" -days 365 -nodes \
        -subj "/CN=localhost" 2>/dev/null
    fi
    if [[ "$UPSTREAM" == "rust" ]]; then
      info "TLS mode requires the Python upstream; throughput will reflect the upstream ceiling"
    fi
    PORT="$TARGET_PORT" TLS_PORT="$TARGET_PORT" TLS_CERT="$tls_cert" TLS_KEY="$tls_key" \
      python3 "$SCRIPT_DIR/echo_server.py" >/tmp/relay_bench_target.log 2>&1 &
  elif [[ "$UPSTREAM" == "rust" && -x "$UPSTREAM_BIN" ]]; then
    PORT="$TARGET_PORT" "$UPSTREAM_BIN" >/tmp/relay_bench_target.log 2>&1 &
  else
    PORT="$TARGET_PORT" python3 "$SCRIPT_DIR/echo_server.py" >/tmp/relay_bench_target.log 2>&1 &
  fi
  TARGET_PID=$!
}

# Is anything already listening on this port?
port_in_use() {
  python3 "$SCRIPT_DIR/port_probe.py" "$1" 2>/dev/null
}

# The readiness probe cannot tell OUR proxy apart from whatever else answers on that port, so a
# stale or foreign listener previously made a dead proxy look healthy — and silently served the
# load (observed: OrbStack holding 18080 turned a 55k req/s run into 1.2k req/s "PASS").
require_port_free() {
  local port="$1" label="$2"
  if port_in_use "$port"; then
    fail "$label port $port is already in use — stop the process holding it, or set a different port"
    return 1
  fi
  return 0
}

start_proxy() {
  "$PROXY_BIN" run \
    --listen "127.0.0.1:$PROXY_PORT" \
    --api-port "$API_PORT" \
    --ca-cert "$CA_CERT" \
    --ca-key "$CA_KEY" \
    >/tmp/relay_bench_proxy.log 2>&1 &
  PROXY_PID=$!

  # Catch "the binary exited immediately" (port conflict, bad CA, bad args) instead of waiting
  # for the whole readiness window to time out.
  local waited=0
  while [[ "$waited" -lt 20 ]]; do
    if ! kill -0 "$PROXY_PID" 2>/dev/null; then
      fail "Proxy process exited immediately during startup"
      [[ -f /tmp/relay_bench_proxy.log ]] && tail -n 5 /tmp/relay_bench_proxy.log >&2
      return 1
    fi
    if port_in_use "$PROXY_PORT"; then
      return 0
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
  # Report what actually happened rather than only that startup timed out: an empty log usually
  # means the binary never ran (wrong path, not executable), while output means it failed on its way
  # up. A flush delay avoids racing the child's first write.
  sleep 0.3
  if [[ -s /tmp/relay_bench_proxy.log ]]; then
    fail "Proxy did not start listening on $PROXY_PORT; last output:"
    tail -n 10 /tmp/relay_bench_proxy.log >&2
  else
    fail "Proxy did not start listening on $PROXY_PORT and produced no output — binary did not run?"
    info "Binary: $PROXY_BIN (exists=$([[ -x "$PROXY_BIN" ]] && echo yes || echo no))"
  fi
  return 1
}

stop_proxy() {
  [[ -n "$PROXY_PID" ]] && kill "$PROXY_PID" 2>/dev/null || true
  PROXY_PID=""
}

# Verify the proxy process is alive; a dead proxy previously scored RSS=0MB, which PASSED the
# idle-memory DoD (`0 -le 85`), producing a meaningless "valid" report.
require_proxy_alive() {
  if [[ -z "$PROXY_PID" ]] || ! kill -0 "$PROXY_PID" 2>/dev/null; then
    fail "Proxy process is not running (see /tmp/relay_bench_proxy.log)"
    return 1
  fi
  return 0
}

# Same for the echo target: it was previously started and used after a bare `sleep 0.3`.
wait_target_ready() {
  local ready=0
  for _ in $(seq 1 50); do
    if port_in_use "$TARGET_PORT"; then
      ready=1
      break
    fi
    if [[ -n "${TARGET_PID:-}" ]] && ! kill -0 "$TARGET_PID" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  if [[ "$ready" -ne 1 ]]; then
    fail "Echo target did not become ready on port $TARGET_PORT (see /tmp/relay_bench_target.log)"
    return 1
  fi
  return 0
}

# Readiness must mean "the proxy answered a proxied request successfully", not merely
# "something returned an HTTP status": a 502/500 previously counted as ready because curl was
# invoked without --fail.
poll_proxy_ready() {
  local target_url="$1"
  local ready=0
  for _ in $(seq 1 50); do
    if curl -s -f -k -x "http://127.0.0.1:$PROXY_PORT" "$target_url" --connect-timeout 0.2 -o /dev/null 2>/dev/null; then
      ready=1
      break
    fi
    if [[ -n "${PROXY_PID:-}" ]] && ! kill -0 "$PROXY_PID" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  echo "$ready"
}

# Which tool produced the idle-memory number. `ps`/VmRSS and macOS `footprint` are not the same
# metric, so the report must say which one was used rather than silently mixing them.
MEMORY_METRIC="rss"

measure_idle_rss_mb() {
  local attempt value
  # A process that has just started can report a 0 footprint, and a blocked `ps` can report
  # nothing at all; either would produce a "0MB" reading that PASSES a `< 85MB` DoD. Retry briefly,
  # then report genuinely-unmeasurable as `unknown` so the DoD fails instead of silently passing.
  for attempt in 1 2 3 4 5; do
    value="$(measure_idle_rss_once)"
    if [[ "$value" =~ ^[0-9]+$ ]] && [[ "$value" -gt 0 ]]; then
      echo "$value"
      return
    fi
    sleep 0.3
  done
  echo "unknown"
}

measure_idle_rss_once() {
  if [[ "$(uname)" == "Darwin" ]]; then
    local rss_kb
    # `ps` must be checked via its EXIT STATUS, not just for empty output: when it cannot run at
    # all (restricted/sandboxed environment) it prints nothing, but in some configurations it
    # prints a bare `0` — and `0MB` satisfied the `< 85MB` DoD, certifying an unmeasured proxy.
    if rss_kb=$(ps -o rss= -p "$PROXY_PID" 2>/dev/null) && [[ "$rss_kb" =~ ^[0-9]+$ ]] && [[ "$rss_kb" -gt 0 ]]; then
      MEMORY_METRIC="rss"
      echo $((rss_kb / 1024))
      return
    fi

    # `ps` can be unavailable in restricted/sandboxed environments. Fall back to `footprint`,
    # which reports phys_footprint (includes compressed memory) rather than resident size, and
    # label it so reports never compare the two as if they were the same metric.
    #
    # NOTE: `footprint` switches units with magnitude ("880 KB" below 1 MB, "306 MB" above), so the
    # unit token must be parsed. Reading the number alone silently produced 51/1024 = 0 for a 51 MB
    # proxy, i.e. a "0MB" reading that PASSED the `< 85MB` DoD.
    if command -v footprint >/dev/null 2>&1; then
      local fp_line fp_value fp_unit fp_kb
      fp_line=$(footprint "$PROXY_PID" 2>/dev/null | awk '/phys_footprint:/{print $2, $3; exit}')
      fp_value="${fp_line%% *}"
      fp_unit="${fp_line##* }"
      if [[ "$fp_value" =~ ^[0-9]+$ ]] && [[ "$fp_value" -gt 0 ]]; then
        case "${fp_unit^^}" in
          KB) fp_kb=$fp_value ;;
          MB) fp_kb=$((fp_value * 1024)) ;;
          GB) fp_kb=$((fp_value * 1024 * 1024)) ;;
          *) fp_kb=0 ;;
        esac
        if [[ "$fp_kb" -gt 0 ]]; then
          MEMORY_METRIC="phys_footprint"
          echo $((fp_kb / 1024))
          return
        fi
      fi
    fi

    echo "unknown"
  else
    local rss_kb
    if rss_kb=$(awk '/VmRSS/{print $2}' "/proc/$PROXY_PID/status" 2>/dev/null) \
      && [[ "$rss_kb" =~ ^[0-9]+$ ]] && [[ "$rss_kb" -gt 0 ]]; then
      MEMORY_METRIC="rss"
      echo $((rss_kb / 1024))
      return
    fi
    echo "unknown"
  fi
}

# An unreadable RSS used to be reported as 0MB, which PASSED the `< 85MB` DoD and silently
# certified a dead or unreadable proxy. A measurement we cannot take must FAIL, not pass.
assert_rss_measurable() {
  local rss="$1"
  # A live process never has 0 memory; a 0 reading means the measurement failed, not that the
  # proxy is frugal.
  if [[ "$rss" = "0" ]]; then
    fail "Idle RSS measured as 0MB for pid ${PROXY_PID:-?} — measurement failed, not a frugal proxy"
    return 1
  fi
  if ! [[ "$rss" =~ ^[0-9]+$ ]]; then
    fail "Idle RSS could not be measured for pid ${PROXY_PID:-?} (got '${rss}')"
    return 1
  fi
  return 0
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

# Success rate over all status codes oha observed. Without this, a run where every request
# returned 502 was indistinguishable from a healthy one.
extract_oha_success_rate() {
  python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("0.00")
    sys.exit(0)
dist = d.get("statusCodeDistribution") or {}
total = 0
ok = 0
for code, count in dist.items():
    try:
        n = int(count)
    except (TypeError, ValueError):
        continue
    total += n
    try:
        c = int(code)
    except (TypeError, ValueError):
        continue
    if 200 <= c < 400:
        ok += n
if total == 0:
    print("0.00")
else:
    print(round(ok * 100.0 / total, 2))
' 2>/dev/null || echo "0.00"
}

extract_oha_errors() {
  python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("")
    sys.exit(0)
dist = d.get("errorDistribution") or {}
print("; ".join(f"{k} x{v}" for k, v in dist.items()))
' 2>/dev/null || echo ""
}

run_load() {
  local scenario="$1"
  local payload_kb="$2"
  local scheme="http"
  local oha_tls_flag=""
  if [[ "$TLS_MODE" -eq 1 ]]; then
    scheme="https"
    oha_tls_flag="--insecure"
  fi
  local target_url="${scheme}://127.0.0.1:$TARGET_PORT/payload/${payload_kb}"
  local proxy_url="http://127.0.0.1:$PROXY_PORT"
  local tls_label=""
  [[ "$TLS_MODE" -eq 1 ]] && tls_label=", TLS"

  local throughput=0
  local p99_ms=0
  local success_rate="0.00"
  local errors=""
  local tool_raw=""

  info "[$scenario] oha ${DURATION}s, ${CONNECTIONS} connections, payload ${payload_kb}KB${tls_label}" >&2
  # oha 1.14 rejects NO_COLOR=1 (expects true/false); unset before invoking.
  # NOTE: oha failures are deliberately NOT masked with `|| echo "{}"` — that turned a totally
  # failed run into a silent 0 req/s entry that still produced a "valid" report.
  if ! tool_raw=$(env -u NO_COLOR oha \
    -z "${DURATION}s" \
    -c "$CONNECTIONS" \
    --no-tui \
    --output-format json \
    $oha_tls_flag \
    -x "$proxy_url" \
    "$target_url" 2>/dev/null); then
    fail "    oha failed to run against $target_url via $proxy_url"
    return 1
  fi
  if [[ -z "$tool_raw" ]]; then
    fail "    oha produced no output for $scenario"
    return 1
  fi

  throughput=$(echo "$tool_raw" | extract_oha_rps)
  p99_ms=$(echo "$tool_raw" | extract_oha_p99_ms)
  success_rate=$(echo "$tool_raw" | extract_oha_success_rate)
  errors=$(echo "$tool_raw" | extract_oha_errors)

  throughput="${throughput:-0}"
  p99_ms="${p99_ms:-0}"

  if [[ "$success_rate" == "0.00" && "$throughput" == "0" ]]; then
    fail "    no successful responses (errors: ${errors:-none})"
    return 1
  fi

  echo "${throughput}|${p99_ms}|${success_rate}|${errors}"
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

# ── environment detection ────────────────────────────────────────────────────

detect_environment() {
  local os_name os_ver cpu cpu_cores ram_gb rustc_ver crate_ver tool_ver

  if [[ "$(uname)" == "Darwin" ]]; then
    os_name="$(sw_vers -productName 2>/dev/null || echo 'macOS')"
    os_ver="$(sw_vers -productVersion 2>/dev/null || echo 'unknown')"
    cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'unknown')"
    cpu_cores="$(sysctl -n hw.ncpu 2>/dev/null || echo 'unknown')"
    ram_gb=$(( $(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1024 / 1024 / 1024 ))
  else
    os_name="$(uname -s)"
    os_ver="$(uname -r)"
    cpu="$(lscpu 2>/dev/null | grep 'Model name' | sed 's/.*:[[:space:]]*//' || grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*:[[:space:]]*//' || echo 'unknown')"
    cpu_cores="$(nproc 2>/dev/null || echo 'unknown')"
    ram_gb="$(( $(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 0) / 1024 / 1024 ))"
  fi

  rustc_ver="$(rustc --version 2>/dev/null | awk '{print $2}' || echo 'unknown')"
  crate_ver="$(grep -E '^version[[:space:]]*=' "$REPO_ROOT/Cargo.toml" 2>/dev/null | head -1 | sed 's/.*"\(.*\)".*/\1/' || echo 'unknown')"

  case "$TOOL" in
    oha) tool_ver="$(oha --version 2>/dev/null | head -1 || echo 'unknown')" ;;
    *)   tool_ver="unknown" ;;
  esac

  echo "${os_name} ${os_ver}|${cpu}|${cpu_cores}|${ram_gb}|${rustc_ver}|${crate_ver}|${tool_ver}"
}

# ── statistics helper ────────────────────────────────────────────────────────

calc_stats() {
  local label="$1"
  python3 -c "
import json, sys, statistics
data = [float(line.strip()) for line in sys.stdin if line.strip()]
if len(data) < 2:
    std = 0.0
else:
    std = statistics.stdev(data)
m = statistics.mean(data)
print(json.dumps({
    'label': '${label}',
    'mean': round(m, 2),
    'stddev': round(std, 2),
    'min': round(min(data), 2),
    'max': round(max(data), 2),
    'values': [round(x, 2) for x in data],
    'n': len(data)
}))
"
}

# ── release mode runner ──────────────────────────────────────────────────────

run_release_mode() {
  local DURATION="${1:-30}"
  local RUNS="${2:-5}"
  local WARMUP_RUNS="${3:-3}"
  local COLD_START_VALS RSS_VALS cs rss
  local cs_mean rss_mean round ready
  local TPUT_VALS P99_VALS SUCCESS_VALS tput p99 tput_mean p99_mean
  local success_rate success_mean
  local SUCCESS_STATUS="PASS"
  local LOADGEN_STATUS="OK"

  echo "=== relay-core Release Benchmark (${TIMESTAMP}) ==="
  echo ""

  TOOL="$(detect_tool)"
  if [[ "$TOOL" != "oha" ]]; then
    fail "Release mode requires oha for accurate concurrent load testing."
    fail "Install: brew install oha  or  cargo install oha"
    exit 1
  fi
  ENV_INFO="$(detect_environment)"
  IFS='|' read -r OS_FULL CPU CPU_CORES RAM_GB RUSTC_VER CRATE_VER TOOL_VER <<< "$ENV_INFO"
  if [[ -n "$REPORT_VERSION" ]]; then
    CRATE_VER="$REPORT_VERSION"
  fi

  info "Version:      ${CRATE_VER}"
  info "Environment:  ${OS_FULL} | ${CPU} (${CPU_CORES} cores) | ${RAM_GB}GB RAM"
  info "Rust:         ${RUSTC_VER}"
  info "Load tool:    ${TOOL_VER} (${TOOL})"
  info "Methodology:  ${WARMUP_RUNS} warmup + ${RUNS} measurement rounds, ${DURATION}s each, ${CONNECTIONS} connections"
  echo ""

  # Build (skip if binary already exists)
  if [[ -f "$PROXY_BIN" ]]; then
    pass "Using existing binary: $PROXY_BIN"
  else
    info "Building release binary..."
    cd "$REPO_ROOT"
    local build_start build_end build_time
    build_start=$(now_ms)
    cargo build --release --package relay-core-cli --quiet
    build_end=$(now_ms)
    build_time=$((build_end - build_start))
    pass "Build completed in ${build_time}ms"
  fi
  echo ""

  if [[ ! -f "$CA_CERT" ]]; then
    info "Generating benchmark CA..."
    "$PROXY_BIN" ca generate --ca-cert "$CA_CERT" --ca-key "$CA_KEY" >/dev/null 2>&1 || true
  fi

  require_port_free "$PROXY_PORT" "PROXY" || return 1
  require_port_free "$API_PORT" "API" || return 1
  require_port_free "$TARGET_PORT" "TARGET" || return 1

  build_upstream
  info "Upstream: $UPSTREAM"
  start_target
  wait_target_ready || return 1

  # ── Phase 1: Cold start + idle RSS (N rounds, fresh proxy each time) ──
  echo "### Phase 1/3: Cold start + Idle RSS (${RUNS} rounds)"
  echo ""

  # Throwaway warmup round — macOS code-signing / dyld cache warm on first launch
  info "  Throwaway warmup round (OS-level, discarded)"
  if ! start_proxy; then
    stop_proxy
    return 1
  fi
  poll_proxy_ready "http://127.0.0.1:$TARGET_PORT/payload/1" > /dev/null
  stop_proxy
  sleep 0.3

  for round in $(seq 1 "$RUNS"); do
    local start_ms end_ms
    start_ms=$(now_ms)
    if ! start_proxy; then
      stop_proxy
      return 1
    fi
    ready="$(poll_proxy_ready "http://127.0.0.1:$TARGET_PORT/payload/1")"
    end_ms=$(now_ms)
    cs=$((end_ms - start_ms))
    if ! require_proxy_alive; then
      stop_proxy
      return 1
    fi
    rss="$(measure_idle_rss_mb)"
    if ! assert_rss_measurable "$rss"; then
      stop_proxy
      return 1
    fi
    stop_proxy
    sleep 0.3

    COLD_START_VALS+="${cs}"$'\n'
    RSS_VALS+="${rss}"$'\n'
    info "  Round ${round}/${RUNS}: cold_start=${cs}ms, idle_rss=${rss}MB"
  done

  CS_STATS="$(echo "$COLD_START_VALS" | calc_stats "cold_start_ms")"
  RSS_STATS="$(echo "$RSS_VALS" | calc_stats "idle_rss_mb")"
  cs_mean="$(echo "$CS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['mean'])")"
  rss_mean="$(echo "$RSS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['mean'])")"

  echo ""
  if (( $(python3 -c "print(1 if ${cs_mean} < ${DOD_STARTUP} else 0)") )); then
    pass "Cold start: mean=${cs_mean}ms (DoD: <${DOD_STARTUP}ms)  PASS"
    CS_STATUS="PASS"
  else
    fail "Cold start: mean=${cs_mean}ms (DoD: <${DOD_STARTUP}ms)  FAIL"
    CS_STATUS="FAIL"
  fi
  if (( $(python3 -c "print(1 if ${rss_mean} < ${DOD_IDLE_MB} else 0)") )); then
    pass "Idle RSS:   mean=${rss_mean}MB (DoD: <${DOD_IDLE_MB}MB)  PASS"
    RSS_STATUS="PASS"
  else
    fail "Idle RSS:   mean=${rss_mean}MB (DoD: <${DOD_IDLE_MB}MB)  FAIL"
    RSS_STATUS="FAIL"
  fi
  echo ""

  # ── Phase 2: Throughput & latency ──────────────────────────────────────
  echo "### Phase 2/3: Throughput & Latency"
  echo ""

  info "Starting proxy for load testing..."
  if ! start_proxy; then
    stop_proxy
    return 1
  fi
  ready=$(poll_proxy_ready "http://127.0.0.1:$TARGET_PORT/payload/1")
  if [[ "$ready" -ne 1 ]]; then
    fail "Proxy not ready for load testing"
    return 1
  fi

  QPS_STATUS="PASS"
  LAT_STATUS="PASS"
  LOAD_MD_ROWS=""
  LOAD_JSON_ITEMS=""
  SCENARIOS=("S1:1")

  for pair in "${SCENARIOS[@]}"; do
    local scenario payload_kb
    scenario="${pair%%:*}"
    payload_kb="${pair##*:}"
    echo "  --- ${scenario} (${payload_kb}KB) ---"

    if [[ "$WARMUP_RUNS" -gt 0 ]]; then
      info "    Warmup: ${WARMUP_RUNS} round(s) (discarded)"
      for _ in $(seq 1 "$WARMUP_RUNS"); do
        run_load "$scenario" "$payload_kb" > /dev/null
      done
    fi

    TPUT_VALS=""
    P99_VALS=""
    SUCCESS_VALS=""
    ERRORS_SEEN=""
    info "    Measurement: ${RUNS} round(s)"
    for round in $(seq 1 "$RUNS"); do
      local metrics
      if ! metrics="$(run_load "$scenario" "$payload_kb")"; then
        fail "    Measurement round ${round}/${RUNS} failed"
        return 1
      fi
      IFS='|' read -r tput p99 success_rate round_errors <<< "$metrics"
      TPUT_VALS+="${tput}"$'\n'
      P99_VALS+="${p99}"$'\n'
      SUCCESS_VALS+="${success_rate}"$'\n'
      if [[ -n "$round_errors" ]]; then
        ERRORS_SEEN="${ERRORS_SEEN}${round_errors}; "
      fi
      info "      Round ${round}/${RUNS}: ${tput} req/s, P99=${p99}ms, success=${success_rate}%${round_errors:+ (${round_errors})}"
    done

    local tput_stats p99_stats success_stats
    tput_stats="$(echo "$TPUT_VALS" | calc_stats "throughput_${scenario}")"
    p99_stats="$(echo "$P99_VALS" | calc_stats "p99_${scenario}")"
    success_stats="$(echo "$SUCCESS_VALS" | calc_stats "success_rate_${scenario}")"
    tput_mean="$(echo "$tput_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['mean'])")"
    p99_mean="$(echo "$p99_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['mean'])")"
    success_mean="$(echo "$success_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['mean'])")"

    local row_qps_status="INFO" row_lat_status="INFO"

    # Distinguish a proxy failure from a load-generator artifact. Exhausting the client's ephemeral
    # port range (macOS `os error 49`) or hitting oha's own deadline shows up as a collapsed success
    # rate while the proxy is healthy — reporting that as a proxy regression is a false signal.
    if [[ "$ERRORS_SEEN" == *"Can't assign requested address"* ]]; then
      LOADGEN_STATUS="FAIL"
      fail "    Load generator exhausted ephemeral ports (os error 49) — result is not a proxy measurement"
      info "    Set CONNECTIONS lower (currently ${CONNECTIONS}) or reduce --duration; see §15-1"
    fi

    if (( $(python3 -c "print(1 if ${success_mean} >= ${DOD_SUCCESS_RATE} else 0)") )); then
      pass "    Success rate: mean=${success_mean}% (DoD: >=${DOD_SUCCESS_RATE}%)"
      SUCCESS_STATUS="PASS"
    elif [[ "$LOADGEN_STATUS" == "FAIL" ]]; then
      # Already reported as a harness artifact above; do not also blame the proxy.
      SUCCESS_STATUS="INVALID"
    else
      fail "    Success rate: mean=${success_mean}% (DoD: >=${DOD_SUCCESS_RATE}%)"
      SUCCESS_STATUS="FAIL"
    fi

    if [[ "$scenario" == "S1" ]]; then
      if (( $(python3 -c "print(1 if ${tput_mean} >= ${DOD_QPS} else 0)") )); then
        pass "    Throughput: mean=${tput_mean} req/s (DoD: >${DOD_QPS})"
        row_qps_status="PASS"
      else
        fail "    Throughput: mean=${tput_mean} req/s (DoD: >${DOD_QPS})"
        row_qps_status="FAIL"
        QPS_STATUS="FAIL"
      fi
      local p99_int="${p99_mean%%.*}"
      if [[ -n "$p99_int" && "$p99_int" -le "$DOD_P99" ]] 2>/dev/null; then
        pass "    P99 Latency: mean=${p99_mean}ms (DoD: <${DOD_P99})"
        row_lat_status="PASS"
      else
        fail "    P99 Latency: mean=${p99_mean}ms (DoD: <${DOD_P99})"
        row_lat_status="FAIL"
        LAT_STATUS="FAIL"
      fi
    else
      info "    Throughput: mean=${tput_mean} req/s, P99: mean=${p99_mean}ms"
    fi

    LOAD_MD_ROWS+=$'\n'"| ${scenario} | ${payload_kb}KB | ${tput_mean} ±$(echo "$tput_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | ${p99_mean} ±$(echo "$p99_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | ${row_qps_status} | ${row_lat_status} |"
    LOAD_JSON_ITEMS+="{\"id\":\"${scenario}\",\"payload_kb\":${payload_kb},\"throughput\":$(echo "$tput_stats" | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps({k: d[k] for k in ['mean','stddev','min','max','values']}))"),\"latency_p99\":$(echo "$p99_stats" | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps({k: d[k] for k in ['mean','stddev','min','max','values']}))")},"
  done
  LOAD_JSON_ITEMS="[${LOAD_JSON_ITEMS%,}]"

  echo ""

  # ── Phase 3: API path latencies ───────────────────────────────────────
  echo "### Phase 3/3: HTTP API paths"
  echo ""

  local flows_query_ms flow_detail_ms sse_first_ms flow_detail_status sse_status flow_id

  flows_query_ms="$(measure_http_ms "http://127.0.0.1:${API_PORT}/api/v1/flows?limit=50&offset=0")"
  flow_id="$(extract_first_flow_id "http://127.0.0.1:${API_PORT}/api/v1/flows?limit=50&offset=0")"
  flow_detail_ms="0"
  flow_detail_status="SKIP"
  if [[ -n "$flow_id" ]]; then
    flow_detail_ms="$(measure_http_ms "http://127.0.0.1:${API_PORT}/api/v1/flows/${flow_id}")"
    flow_detail_status="OK"
  fi

  sse_first_ms="$(measure_sse_first_event_ms "http://127.0.0.1:${API_PORT}/api/v1/events")"
  if [[ "${sse_first_ms%%.*}" -gt 0 ]] 2>/dev/null; then
    sse_status="OK"
  else
    sse_status="WARN"
  fi

  info "GET /api/v1/flows:            ${flows_query_ms}ms"
  info "GET /api/v1/flows/{id}:       ${flow_detail_ms}ms [${flow_detail_status}]"
  info "GET /api/v1/events (SSE):     ${sse_first_ms}ms [${sse_status}]"

  stop_proxy

  echo ""

  # ── Summary ───────────────────────────────────────────────────────────
  echo "=== Summary ==="
  cat <<EOF
  Cold start:       ${cs_mean}ms ±$(echo "$CS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])")   [${CS_STATUS}]
  Idle RSS:         ${rss_mean}MB ±$(echo "$RSS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])")   [${RSS_STATUS}]
  Throughput (S1):  ${tput_mean} req/s    [${QPS_STATUS}]
  Latency P99 (S1): ${p99_mean}ms        [${LAT_STATUS}]
  Success rate:     ${success_mean}%           [${SUCCESS_STATUS}]
  API flows query:  ${flows_query_ms}ms
  API flow detail:  ${flow_detail_ms}ms [${flow_detail_status}]
  API SSE first:    ${sse_first_ms}ms [${sse_status}]
EOF
  echo ""

  local COMMIT DATE_UTC
  COMMIT="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")"
  DATE_UTC="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

  local MEMORY_METRIC_NOTE=""
  if [[ "$MEMORY_METRIC" != "rss" ]]; then
    MEMORY_METRIC_NOTE=" (phys_footprint, NOT resident size — do not compare against rss baselines)"
  fi

  # ── Reports ───────────────────────────────────────────────────────────
  local report_prefix="release_v${CRATE_VER}_${TIMESTAMP}"
  local OUT_MD="$RESULTS_DIR/${report_prefix}.md"
  local OUT_JSON="$RESULTS_DIR/${report_prefix}.json"

  cat >"$OUT_MD" <<REPORT
## RelayCore v${CRATE_VER} — Release Performance Report

- **Date**: ${DATE_UTC}
- **Commit**: \`${COMMIT}\`
- **Mode**: release (${WARMUP_RUNS} warmup + ${RUNS} measurement rounds, ${DURATION}s each)

### Environment

| Item | Detail |
|------|--------|
| OS | ${OS_FULL} |
| CPU | ${CPU} (${CPU_CORES} cores) |
| RAM | ${RAM_GB} GB |
| Rust | ${RUSTC_VER} |
| Load tool | ${TOOL_VER} |
| Connections | ${CONNECTIONS} |
| Memory metric | ${MEMORY_METRIC}${MEMORY_METRIC_NOTE} |

### Results (S1: 1KB payload)

| Metric | Mean | StdDev | Min | Max | DoD Target | Status |
|--------|------|--------|-----|-----|------------|--------|
| Cold start | ${cs_mean}ms | ±$(echo "$CS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | $(echo "$CS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['min'])")ms | $(echo "$CS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['max'])")ms | <${DOD_STARTUP}ms | ${CS_STATUS} |
| Idle RSS | ${rss_mean}MB | ±$(echo "$RSS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | $(echo "$RSS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['min'])")MB | $(echo "$RSS_STATS" | python3 -c "import json,sys; print(json.load(sys.stdin)['max'])")MB | <${DOD_IDLE_MB}MB | ${RSS_STATUS} |
| Throughput | ${tput_mean} req/s | ±$(echo "$tput_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | $(echo "$tput_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['min'])") | $(echo "$tput_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['max'])") | >${DOD_QPS} req/s | ${QPS_STATUS} |
| Success rate | ${success_mean}% | ±$(echo "$success_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | $(echo "$success_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['min'])")% | $(echo "$success_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['max'])")% | >=${DOD_SUCCESS_RATE}% | ${SUCCESS_STATUS} |
| P99 Latency | ${p99_mean}ms | ±$(echo "$p99_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['stddev'])") | $(echo "$p99_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['min'])")ms | $(echo "$p99_stats" | python3 -c "import json,sys; print(json.load(sys.stdin)['max'])")ms | <${DOD_P99}ms | ${LAT_STATUS} |

### Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|${LOAD_MD_ROWS}

### API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | ${flows_query_ms}ms | OK |
| GET /api/v1/flows/{id} | ${flow_detail_ms}ms | ${flow_detail_status} |
| GET /api/v1/events (SSE first event) | ${sse_first_ms}ms | ${sse_status} |

### Reproduce

\`\`\`bash
git checkout ${COMMIT}
./benchmarks/bench_minimal.sh release --runs ${RUNS} --warmup-runs ${WARMUP_RUNS} --duration ${DURATION}
\`\`\`

### Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
REPORT

  cat >"$OUT_JSON" <<JSON
{
  "report_type": "release",
  "version": "${CRATE_VER}",
  "commit": "${COMMIT}",
  "timestamp": "${DATE_UTC}",
  "environment": {
    "os": "${OS_FULL}",
    "cpu": "${CPU}",
    "cpu_cores": ${CPU_CORES},
    "ram_gb": ${RAM_GB},
    "rustc": "${RUSTC_VER}",
    "load_tool": "${TOOL_VER}",
    "memory_metric": "${MEMORY_METRIC}"
  },
  "methodology": {
    "warmup_rounds": ${WARMUP_RUNS},
    "measurement_rounds": ${RUNS},
    "duration_per_round_s": ${DURATION},
    "connections": ${CONNECTIONS}
  },
  "results": {
    "cold_start_ms": $(echo "$CS_STATS" | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps({k: d[k] for k in ['mean','stddev','min','max','values','n']}))"),
    "idle_rss_mb": $(echo "$RSS_STATS" | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps({k: d[k] for k in ['mean','stddev','min','max','values','n']}))"),
    "api_flows_query_ms": ${flows_query_ms},
    "api_flow_detail_ms": ${flow_detail_ms},
    "api_sse_first_event_ms": ${sse_first_ms}
  },
  "load_errors": "${ERRORS_SEEN}",
  "scenarios": ${LOAD_JSON_ITEMS},
  "dod": {
    "cold_start": "${CS_STATUS}",
    "idle_rss": "${RSS_STATUS}",
    "throughput_s1": "${QPS_STATUS}",
    "latency_p99_s1": "${LAT_STATUS}",
    "success_rate_s1": "${SUCCESS_STATUS}",
    "api_flow_detail": "${flow_detail_status}",
    "api_sse": "${sse_status}"
  }
}
JSON

  # Gate BEFORE announcing/reporting: a run whose DoD failed must not be treated as a valid
  # report (previously `--strict` was unreachable in release mode because of the early exit).
  if [[ "$SUCCESS_STATUS" == "INVALID" ]]; then
    fail "Release run is INVALID: the load generator, not the proxy, limited the measurement"
    if [[ "$STRICT" -eq 1 ]]; then
      fail "Strict mode: refusing to publish an invalid measurement"
      return 1
    fi
  fi

  if [[ "$CS_STATUS" == "FAIL" || "$RSS_STATUS" == "FAIL" || "$QPS_STATUS" == "FAIL" || "$LAT_STATUS" == "FAIL" || "$SUCCESS_STATUS" == "FAIL" ]]; then
    fail "Release run produced a FAIL DoD status; report written for diagnosis only: $OUT_MD"
    if [[ "$STRICT" -eq 1 ]]; then
      fail "Strict mode: DoD check failed"
      return 1
    fi
  fi

  pass "Markdown report: $OUT_MD"
  pass "JSON report:    $OUT_JSON"
  echo ""
}

# ── ramp mode runner ──────────────────────────────────────────────────────────

CONCURRENCY_LEVELS=(10 50 100 200 500)

run_ramp_mode() {
  local payload_kb="${1:-1}"

  if [[ "$TLS_MODE" -eq 1 ]]; then
    echo "=== relay-core Ramp-up Benchmark (TLS) (${TIMESTAMP}) ==="
  else
    echo "=== relay-core Ramp-up Benchmark (${TIMESTAMP}) ==="
  fi
  echo ""

  TOOL="$(detect_tool)"
  if [[ -z "$TOOL" ]]; then
    echo "Error: oha is required."
    exit 1
  fi

  echo "  Concurrency levels: ${CONCURRENCY_LEVELS[*]}"
  echo "  Duration per step:  ${DURATION}s"
  echo "  Payload:             ${payload_kb}KB"
  [[ "$TLS_MODE" -eq 1 ]] && echo "  Mode:                TLS (MITM re-encrypt)"
  echo ""

  if [[ -f "$PROXY_BIN" ]]; then
    info "Using existing binary: $PROXY_BIN"
  else
    info "Building release binary..."
    cd "$REPO_ROOT"
    cargo build --release --package relay-core-cli --quiet
    echo ""
  fi

  if [[ ! -f "$CA_CERT" ]]; then
    info "Generating benchmark CA..."
    "$PROXY_BIN" ca generate --ca-cert "$CA_CERT" --ca-key "$CA_KEY" >/dev/null 2>&1 || true
  fi

  build_upstream
  info "Upstream: $UPSTREAM"
  start_target
  wait_target_ready || return 1
  info "Starting proxy..."
  start_proxy || return 1
  require_proxy_alive || return 1
  ready=$(poll_proxy_ready "http://127.0.0.1:$TARGET_PORT/payload/1")
  if [[ "$ready" -ne 1 ]]; then
    fail "Proxy not ready (no successful proxied response)"
    return 1
  fi

  echo ""
  printf "%-6s %12s %10s %10s\n" "Conn" "QPS" "P99(ms)" "Success%"
  printf "%-6s %12s %10s %10s\n" "------" "------------" "----------" "--------"

  local ramp_csv="Conn,QPS,P99_ms,Success_pct"
  local first_row=1

  for conn in "${CONCURRENCY_LEVELS[@]}"; do
    local saved_conn="$CONNECTIONS"
    CONNECTIONS="$conn"

    local warmup_needed=1
    if [[ "$first_row" -eq 1 ]]; then
      first_row=0
      warmup_needed=0
    fi

    # Warmup pass (discard)
    run_load "RAMP" "$payload_kb" > /dev/null

    # Measurement pass
    local metrics
    if ! metrics="$(run_load "RAMP" "$payload_kb")"; then
      fail "Ramp step ${conn} connections failed"
      CONNECTIONS="$saved_conn"
      return 1
    fi
    local tput p99 success_rate ramp_errors
    IFS='|' read -r tput p99 success_rate ramp_errors <<< "$metrics"
    printf "%-6s %12s %10s %10s\n" "$conn" "$tput" "$p99" "$success_rate"
    ramp_csv+=$'\n'"${conn},${tput},${p99},${success_rate}"

    CONNECTIONS="$saved_conn"
  done

  stop_proxy

  echo ""
  echo "=== Ramp-up Summary ==="
  echo "$ramp_csv"

  local OUT_CSV="$RESULTS_DIR/ramp_${TIMESTAMP}.csv"
  echo "$ramp_csv" > "$OUT_CSV"
  info "CSV report: $OUT_CSV"
}

# ── main ──────────────────────────────────────────────────────────────────────

echo "=== relay-core benchmark (${TIMESTAMP}) ==="
echo ""

TOOL="$(detect_tool)"
if [[ -z "$TOOL" ]]; then
  echo "Error: oha is required. Install: brew install oha  or  cargo install oha"
  exit 1
fi
info "Load generator: ${TOOL}"
info "Mode: ${MODE}, duration=${DURATION}s"

if [[ "$MODE" == "release" ]]; then
  if ! run_release_mode "$DURATION" "$RUNS" "$WARMUP_RUNS"; then
    exit 1
  fi
  exit 0
fi

if [[ "$MODE" == "ramp" ]]; then
  run_ramp_mode
  exit 0
fi

TLS_SCHEME="$([[ "$TLS_MODE" -eq 1 ]] && echo "https" || echo "http")"
SCENARIOS=("S1:1")
if [[ "$MODE" == "matrix" ]]; then
  SCENARIOS=("S1:1" "S2:64" "S3:1024")
fi

if [[ -f "$PROXY_BIN" ]]; then
  info "Using existing binary: $PROXY_BIN"
else
  info "Building release binary..."
  cd "$REPO_ROOT"
  BUILD_START=$(now_ms)
  cargo build --release --package relay-core-cli --quiet
  BUILD_END=$(now_ms)
  BUILD_TIME=$((BUILD_END - BUILD_START))
  info "Build completed in ${BUILD_TIME}ms"
fi
echo ""

if [[ ! -f "$CA_CERT" ]]; then
  info "Generating benchmark CA..."
  "$PROXY_BIN" ca generate --ca-cert "$CA_CERT" --ca-key "$CA_KEY" >/dev/null 2>&1 || true
fi

require_port_free "$PROXY_PORT" "PROXY" || exit 1
require_port_free "$API_PORT" "API" || exit 1
require_port_free "$TARGET_PORT" "TARGET" || exit 1

build_upstream
info "Upstream: $UPSTREAM"
start_target
wait_target_ready || exit 1

echo "### Benchmark 1/3: Cold start time"
START_MS=$(now_ms)
start_proxy || exit 1
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
if ! assert_rss_measurable "$RSS_MB"; then
  MEMORY_STATUS="FAIL"
elif [[ "$RSS_MB" -le "$DOD_IDLE_MB" ]]; then
  pass "Idle RSS: ${RSS_MB}MB (DoD: < ${DOD_IDLE_MB}MB)"
  MEMORY_STATUS="PASS"
else
  fail "Idle RSS: ${RSS_MB}MB (DoD: < ${DOD_IDLE_MB}MB)"
  MEMORY_STATUS="FAIL"
fi

echo ""
echo "### Benchmark 3/4: Throughput & latency"

SCENARIO_ROWS_MD=""
SCENARIO_ROWS_JSON=""
S1_QPS=0
S1_P99=0
QPS_STATUS="SKIP"
LAT_STATUS="FAIL"
SUCCESS_STATUS="FAIL"
SUCCESS_RATE="0.00"
SCENARIO_ERRORS=""
REPORT_ABORTED=0

for pair in "${SCENARIOS[@]}"; do
  SCENARIO="${pair%%:*}"
  PAYLOAD_KB="${pair##*:}"
  if ! METRICS="$(run_load "$SCENARIO" "$PAYLOAD_KB")"; then
    fail "[$SCENARIO] measurement failed"
    REPORT_ABORTED=1
    break
  fi
  IFS='|' read -r THROUGHPUT P99_MS SUCCESS_RATE SCENARIO_ERRORS <<< "$METRICS"

  ROW_QPS_STATUS="INFO"
  ROW_LAT_STATUS="INFO"

  if [[ "$SCENARIO" == "S1" ]]; then
    S1_QPS="$THROUGHPUT"
    S1_P99="$P99_MS"
    if [[ "$THROUGHPUT" -ge "$DOD_QPS" && "$(python3 -c "print(1 if ${SUCCESS_RATE:-0} > 0 else 0)")" == "1" ]]; then
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

    # A throughput number is only meaningful if the responses actually succeeded.
    if (( $(python3 -c "print(1 if ${SUCCESS_RATE:-0} >= ${DOD_SUCCESS_RATE} else 0)") )); then
      SUCCESS_STATUS="PASS"
      pass "[S1] Success rate: ${SUCCESS_RATE}% (DoD: >= ${DOD_SUCCESS_RATE}%)"
    else
      SUCCESS_STATUS="FAIL"
      fail "[S1] Success rate: ${SUCCESS_RATE}% (DoD: >= ${DOD_SUCCESS_RATE}%)"
    fi
  else
    info "[$SCENARIO] Throughput: ${THROUGHPUT} req/s, P99: ${P99_MS}ms, success: ${SUCCESS_RATE}%"
  fi

  SCENARIO_ROWS_MD+=$'\n'"| ${SCENARIO} | ${PAYLOAD_KB}KB | ${THROUGHPUT} | ${P99_MS} | ${ROW_QPS_STATUS} | ${ROW_LAT_STATUS} |"
  SCENARIO_ROWS_JSON+=$'{"id":"'"${SCENARIO}"'","payload_kb":'"${PAYLOAD_KB}"',"throughput_rps":'"${THROUGHPUT}"',"latency_p99_ms":'"${P99_MS}"',"success_rate_pct":'"${SUCCESS_RATE:-0}"',"qps_status":"'"${ROW_QPS_STATUS}"'","latency_status":"'"${ROW_LAT_STATUS}"'"},'
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
    "success_rate_pct": ${SUCCESS_RATE},
    "api_flows_query_ms": ${FLOWS_QUERY_MS},
    "api_flow_detail_ms": ${FLOW_DETAIL_MS},
    "api_sse_first_event_ms": ${SSE_FIRST_EVENT_MS}
  },
  "status": {
    "cold_start": "${STARTUP_STATUS}",
    "idle_rss": "${MEMORY_STATUS}",
    "throughput_s1": "${QPS_STATUS}",
    "latency_p99_s1": "${LAT_STATUS}",
    "success_rate_s1": "${SUCCESS_STATUS}",
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
info "JSON report:    $OUT_JSON"

# A report is emitted for diagnosis, but a FAILED run must not look successful.
if [[ "$REPORT_ABORTED" -eq 1 ]]; then
  fail "Measurement aborted (oha or proxy failure); report is diagnostic only"
  exit 1
fi

if [[ "$STRICT" -eq 1 ]]; then
  if [[ "$STARTUP_STATUS" == "FAIL" || "$MEMORY_STATUS" == "FAIL" || "$QPS_STATUS" == "FAIL" || "$LAT_STATUS" == "FAIL" || "$SUCCESS_STATUS" == "FAIL" ]]; then
    fail "Strict mode: DoD check failed"
    exit 1
  fi
  if [[ "$REGRESSION_STATUS" == "WARN" ]]; then
    fail "Strict mode: regression warning treated as failure"
    exit 1
  fi
fi
