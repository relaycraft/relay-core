# RelayCore v0.8.1 — Release Performance Report

- **Date**: 2026-06-07T18:24:42Z
- **Commit**: `79e3346`
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
| Cold start | 370.6ms | ±541.92 | 125.0ms | 1340.0ms | <200ms | FAIL |
| Idle RSS | 70.8MB | ±0.84 | 70.0MB | 72.0MB | <50MB | FAIL |
| Throughput | 35212.2 req/s | ±21940.92 | 9175.0 | 63947.0 | >10000 req/s | PASS |
| P99 Latency | 28.27ms | ±32.75 | 2.91ms | 83.32ms | <5ms | FAIL |

## Scenario Results

| Scenario | Payload | Throughput (req/s) | P99 (ms) | QPS | Lat |
|----------|---------|--------------------|----------|-----|-----|
| S1 | 1KB | 35212.2 ±21940.92 | 28.27 ±32.75 | PASS | FAIL |

## API Path Latency

| Path | Result | Status |
|------|--------|--------|
| GET /api/v1/flows | 0.71ms | OK |
| GET /api/v1/flows/{id} | 0.47ms | OK |
| GET /api/v1/events (SSE first event) | 16.01ms | OK |

## Reproduce

```bash
git checkout 79e3346
./benchmarks/bench_minimal.sh release --runs 5 --warmup-runs 3 --duration 30
```

> **Note**: Results depend on hardware and system load. For comparable results, use a
> quiet machine (no other heavy processes), plug in power (laptop), and match the
> environment specs above as closely as possible.
