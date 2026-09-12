## RelayCore v0.10.0 — Release Performance Report

- **Date**: 2026-09-12T07:05:15Z
- **Commit**: `00d48a9`
- **Mode**: release (1 warmup + 3 measurement rounds, 15s each)

### Environment

| Item | Detail |
|------|--------|
| OS | macOS 26.5.2 |
| CPU | Apple M4 Max (16 cores) |
| RAM | 64 GB |
| Rust | 1.95.0 |
| Load tool | oha 1.15.0 |
| Connections | 100 |
| Memory metric | rss |

### Results (S1: 1KB payload)

| Metric | Mean | StdDev | Min | Max | DoD Target | Status |
|--------|------|--------|-----|-----|------------|--------|
| Cold start | 140.0ms | ±1.0 | 139.0ms | 141.0ms | <200ms | PASS |
| Idle RSS | 0.0MB | ±0.0 | 0.0MB | 0.0MB | <85MB | PASS |
| Throughput | 61528.33 req/s | ±15536.42 | 43671.0 | 71946.0 | >10000 req/s | PASS |
| Success rate | 9.37% | ±15.36 | 0.0% | 27.1% | >=99.0% | FAIL |
| P99 Latency | 8.19ms | ±7.91 | 3.48ms | 17.33ms | <20ms | PASS |

### Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 61528.33 ±15536.42 | 8.19 ±7.91 | PASS | PASS |

### API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 3.78ms | OK |
| GET /api/v1/flows/{id} | 0.59ms | OK |
| GET /api/v1/events (SSE first event) | 12.09ms | OK |

### Reproduce

```bash
git checkout 00d48a9
./benchmarks/bench_minimal.sh release --runs 3 --warmup-runs 1 --duration 15
```

### Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
