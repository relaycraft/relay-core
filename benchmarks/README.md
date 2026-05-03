# Benchmarks Quick Start

本目录提供 RelayCore 当前可用的轻量性能基准入口（pre-CI baseline）。

## Scope

当前入口脚本：`bench_minimal.sh`

已覆盖：
- 冷启动时间（cold start）
- 空闲 RSS
- S1 吞吐量（req/s）与 P99 延迟
- Payload 趋势（S1/S2/S3 = 1KB/64KB/1024KB）
- HTTP API 路径延迟：
  - `GET /api/v1/flows`
  - `GET /api/v1/flows/{id}`
  - `GET /api/v1/events` 首事件到达时间
- 与历史 JSON 报告的回归对比（默认告警）

未覆盖（后续）：
- S4–S12 完整场景矩阵（TLS/规则/脚本/WebSocket/长稳）
- 峰值内存曲线、CPU 采样、P99.9
- mitmproxy 对照
- CI 强门禁（当前仅 smoke + 产物上传）

## Requirements

- Rust toolchain (`cargo`)
- `python3`
- 负载工具二选一：
  - `oha`（推荐）
  - `ab`（macOS 通常预装）

## Usage

```bash
# 1) 默认：single 模式（S1）
./benchmarks/bench_minimal.sh

# 2) 快速冒烟（10 秒）
./benchmarks/bench_minimal.sh quick

# 3) payload 趋势（S1/S2/S3）
./benchmarks/bench_minimal.sh matrix --duration 10

# 4) 与历史 JSON 比较（默认仅告警）
./benchmarks/bench_minimal.sh --baseline benchmarks/results/bench_xxx.json

# 5) 严格模式（DoD/回归告警视为失败）
./benchmarks/bench_minimal.sh --strict
```

## Environment Variables

可选覆盖端口与并发：

```bash
PROXY_PORT=18080 TARGET_PORT=19000 API_PORT=18082 CONNECTIONS=100 \
  ./benchmarks/bench_minimal.sh
```

## Output

每次运行输出到 `benchmarks/results/`：
- `bench_<timestamp>.md`
- `bench_<timestamp>.json`

JSON 可作为后续 CI 趋势分析与回归比较输入。

## CI Status

已接入最小 CI（告警型，不阻断）：
- Workflow: `.github/workflows/benchmark-smoke.yml`
- 触发：`pull_request(main)`、`workflow_dispatch`、每周定时
- 执行：
  - `./benchmarks/bench_minimal.sh quick`
  - `./benchmarks/bench_minimal.sh matrix --duration 5`
- 产出：
  - 上传 `benchmarks/results/` artifact
  - 在 Job Summary 展示关键指标（S1 + API paths）

说明：
- 当前 workflow 不启用 `--strict`，目的是先建立趋势与可见性，避免性能波动阻塞主线开发。

## Notes

- 当前定位是“快速体检 + 回归趋势”，不是最终容量极限评估。
- 若本机仅有 `curl`，脚本仍可运行，但吞吐结果会降级为近似或跳过。
- 建议固定机器负载后再比较两次结果，避免噪声导致误判。
