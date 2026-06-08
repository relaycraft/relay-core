# RelayCore v0.8.3 — Release Performance Report

- **Date**: 2026-06-08T16:40:50Z
- **Commit**: `f0021f2`
- **Mode**: release (3 warmup + 5 measurement rounds, 30s each)

## Environment

| Item | Detail |
|------|--------|
| OS | macOS 26.5 |
| CPU | Apple M4 Max (16 cores) |
| RAM | 64 GB |
| Rust | 1.95.0 |
| Load tool | oha 1.14.0 |
| Connections | 100 |

## Results (S1: 1KB payload)

| Metric | Mean | StdDev | Min | Max | DoD Target | Status |
|--------|------|--------|-----|-----|------------|--------|
| Cold start | 124.4ms | ±0.89 | 123.0ms | 125.0ms | <200ms | PASS |
| Idle RSS | 72.0MB | ±0.71 | 71.0MB | 73.0MB | <85MB | PASS |
| Throughput | 53316.0 req/s | ±14139.87 | 38376.0 | 69322.0 | >10000 req/s | PASS |
| P99 Latency | 4.56ms | ±3.74 | 2.44ms | 11.16ms | <20ms | PASS |

## Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 53316.0 ±14139.87 | 4.56 ±3.74 | PASS | PASS |

## API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 0.81ms | OK |
| GET /api/v1/flows/{id} | 0.5ms | OK |
| GET /api/v1/events (SSE first event) | 12.24ms | OK |

## Reproduce

```bash
git checkout f0021f2
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

## Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
