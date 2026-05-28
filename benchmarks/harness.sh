#!/usr/bin/env bash
# harness.sh - relay-core comprehensive benchmark suite (M7 B1)
# Runs micro-benchmarks (Criterion) and end-to-end throughput.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "  ${YELLOW}->${NC} $*"; }
pass() { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }

MODE="${1:-all}"
E2E_DURATION=15
EXTRA_BENCH_ARGS=""

case "$MODE" in
  quick) E2E_DURATION=5; EXTRA_BENCH_ARGS="--sample-size 10" ;;
  e2e|micro) ;;
  all) ;;
  *) echo "Usage: $0 [all|quick|e2e|micro]" && exit 1 ;;
esac

# ── Phase 1: Micro-benchmarks ──────────────────────────────────────

if [[ "$MODE" != "e2e" ]]; then
  info "Phase 1/2: Micro-benchmarks"

  cd "$REPO_ROOT"

  for bench_name in rule_engine tls_ca scenarios; do
    info "  Running: $bench_name"
    if cargo bench --package relay-core-lib --bench "$bench_name" $EXTRA_BENCH_ARGS > /tmp/relay_bench_${bench_name}.log 2>&1; then
      pass "    $bench_name: OK"
      # Extract summary lines
      grep -E '^\S.*time:\s*\[' /tmp/relay_bench_${bench_name}.log | while read -r line; do
        name=$(echo "$line" | sed 's/  *time:.*//' | xargs)
        avg=$(echo "$line" | awk -F'[][]' '{print $4}' | xargs)
        info "      $name: $avg"
      done || true
    else
      fail "    $bench_name: FAILED"
      tail -5 /tmp/relay_bench_${bench_name}.log
    fi
  done

  pass "Micro-benchmarks complete"
fi

# ── Phase 2: End-to-end throughput ─────────────────────────────────

if [[ "$MODE" != "micro" ]]; then
  info "Phase 2/2: End-to-end throughput (${E2E_DURATION}s)"

  if [[ -f "$SCRIPT_DIR/bench_minimal.sh" ]]; then
    cd "$REPO_ROOT"
    info "  Building release binary..."
    if cargo build --release --package relay-core-cli --quiet 2>/dev/null; then
      pass "  Build: OK"
    else
      fail "  Build: FAILED"
    fi

    info "  Running matrix: S1(1KB) S2(64KB) S3(1024KB)"
    bash "$SCRIPT_DIR/bench_minimal.sh" matrix --duration "$E2E_DURATION" 2>&1 || true

    latest=$(ls -t "$RESULTS_DIR"/bench_*.md 2>/dev/null | head -1)
    if [[ -n "$latest" ]]; then
      pass "  Report: $latest"
    fi
  else
    fail "bench_minimal.sh not found"
  fi
fi

echo ""
pass "Harness complete ($MODE mode)"
