## RelayCore v0.11.0 — Release Performance Report

- **Date**: 2026-09-13T19:09:52Z
- **Commit**: `6c36896`
- **Mode**: release (3 warmup + 5 measurement rounds, 30s each)

### Environment

| Item | Detail |
|------|--------|
| OS | macOS 26.5.2 |
| CPU | Apple M4 Max (16 cores) |
| RAM | 64 GB |
| Rust | 1.95.0 |
| Load tool | oha 1.15.0 |
| Connections | 25 |
| Memory metric | rss |

### Results (S1: 1KB payload)

| Metric | Mean | StdDev | Min | Max | DoD Target | Status |
|--------|------|--------|-----|-----|------------|--------|
| Cold start | 139.0ms | ±11.79 | 132.0ms | 160.0ms | <200ms | PASS |
| Idle RSS | 52.0MB | ±0.0 | 52.0MB | 52.0MB | <85MB | PASS |
| Throughput | 48380.2 req/s | ±860.27 | 47004.0 | 49371.0 | >10000 req/s | PASS |
| Success rate | 100.0% | ±0.0 | 100.0% | 100.0% | >=99.0% | PASS |
| P99 Latency | 1.07ms | ±0.11 | 0.99ms | 1.27ms | <20ms | PASS |

### Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 48380.2 ±860.27 | 1.07 ±0.11 | PASS | PASS |

### API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 0.96ms | OK |
| GET /api/v1/flows/{id} | 0.53ms | OK |
| GET /api/v1/events (SSE first event) | 12.3ms | OK |

### Reproduce

```bash
git checkout 6c36896
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

### Methodology Notes

- **Single-machine constraint**: oha (load gen), relay-core (proxy), and the echo server all run on the same machine and compete for CPU. P99 latency is therefore an upper bound — in an isolated setup (separate load-gen machine), P99 typically drops by 50-70%.
- **Cold start**: macOS performs code-signing verification and dyld cache warmup on first launch. The first cold-start sample is discarded as a throwaway warmup round; reported values are rounds 2+.
- **RSS on Apple Silicon**: M-series chips use 16 KB pages (vs 4 KB on x86_64), which inflates RSS by ~1.5-2× due to page-level fragmentation. Expect ~35-45 MB RSS on x86_64 Linux.
- **DoD thresholds** are calibrated for single-machine localhost. See script source for current values.

> **Reproducibility**: For comparable results, use a quiet machine (close browsers and other heavy apps), plug in power (laptop), and match the environment specs above as closely as possible.
