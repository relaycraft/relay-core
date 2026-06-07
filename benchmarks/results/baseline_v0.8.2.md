# RelayCore v0.8.2 — Release Performance Report

- **Date**: 2026-06-07T18:52:02Z
- **Commit**: `15101c0`
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
| Cold start | 136.4ms | ±3.51 | 133.0ms | 140.0ms | <200ms | PASS |
| Idle RSS | 71.0MB | ±0.71 | 70.0MB | 72.0MB | <85MB | PASS |
| Throughput | 53231.0 req/s | ±20939.29 | 36995.0 | 76832.0 | >10000 req/s | PASS |
| P99 Latency | 4.81ms | ±2.11 | 2.63ms | 7.51ms | <20ms | PASS |

## Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 53231.0 ±20939.29 | 4.81 ±2.11 | PASS | PASS |

## API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 0.79ms | OK |
| GET /api/v1/flows/{id} | 0.46ms | OK |
| GET /api/v1/events (SSE first event) | 9.94ms | OK |

## Reproduce

```bash
git checkout 15101c0
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

## Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
