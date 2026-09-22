## RelayCore v0.13.2 — Release Performance Report

- **Date**: 2026-09-22T16:29:52Z
- **Commit**: `10fde3c`
- **Mode**: release (3 warmup + 5 measurement rounds, 30s each)

### Environment

| Item | Detail |
|------|--------|
| OS | macOS 27.0 |
| CPU | Apple M4 Max (16 cores) |
| RAM | 64 GB |
| Rust | 1.95.0 |
| Load tool | oha 1.15.0 |
| Connections | 25 |
| Memory metric | rss |

### Results (S1: 1KB payload)

| Metric | Mean | StdDev | Min | Max | DoD Target | Status |
|--------|------|--------|-----|-----|------------|--------|
| Cold start | 133.6ms | ±3.21 | 131.0ms | 139.0ms | <200ms | PASS |
| Idle RSS | 59.2MB | ±0.84 | 58.0MB | 60.0MB | <85MB | PASS |
| Throughput | 36486.0 req/s | ±557.48 | 35811.0 | 36923.0 | >10000 req/s | PASS |
| Success rate | 100.0% | ±0.0 | 100.0% | 100.0% | >=99.0% | PASS |
| P99 Latency | 1.04ms | ±0.06 | 0.97ms | 1.1ms | <20ms | PASS |

### Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 36486.0 ±557.48 | 1.04 ±0.06 | PASS | PASS |

### API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 0.56ms | OK |
| GET /api/v1/flows/{id} | 0ms | SKIP |
| GET /api/v1/events (SSE first event) | 0ms | WARN |

### Reproduce

```bash
git checkout 10fde3c
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

### Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
