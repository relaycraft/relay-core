# 0011 · soak 与 fuzz 的 CI 承诺

- **Status**: Accepted
- **Date**: 2026-10-12
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§15、§22、§24.10）、
  [`../engine-capability-status.md`](../engine-capability-status.md)（§2.4）

## 背景

路线图 §24.10 长期记着两件事：**soak 未接任何 workflow**、**无 fuzz**。
按 §22 的口径，这两种状态都不该无限期停留——它们要么变成真实门禁，要么被明确降级为「不承诺」，
否则读者无法区分「没做」与「不打算做」。

盘点发现 soak 在**未接线期间已经漂移到测错对象**：
`benchmarks/stability_test.sh` 的上游是 `echo_server.py`（HTTP/1.0，每响应即关连接，天花板约 2.7k req/s），
于是运行中的代理**从未进入持续负载**——而「持续负载下的内存行为」正是 soak 唯一能看见的东西。
（这与 §24.10「载荷生成器上限」是同一类错误：把上游的极限记在代理账上。）

## 决策

**1. soak 接入 CI，作为定时任务，不阻塞 PR。**

- 新增 `.github/workflows/soak.yml`：每周日 04:00 UTC + 可手动触发，跑 **20 分钟**。
- **判据就是脚本自己的 verdict**，不另发明阈值：panic → FAIL；RSS 增长 > 50% → FAIL；> 20% → WARN。
  一个只打印数字的 soak 正是本次要替换掉的装饰品。
- 时长取 20 分钟而非脚本默认的 2 小时：这是**能放进定时任务**的信号量，而阈值判的是**趋势**而不是
  「跑满某个小时数」。需要长跑时仍可手动 `--duration 12`。
- 脚本的上游改为 `benchmarks/rust_echo_server.rs`（与性能 harness 同一个上游，理由相同）；
  只有确实拿不到时回退 Python，**并在报告里写明这一轮是 upstream-bound、不是关于代理的结论**。

**1b. 「测不到」必须有别于「没问题」。** 接线过程中实测发现两处同族缺陷，都已修：

- CSV 里**表头行**被 `tail -1` 当成最后一条采样 ⇒ 一次**零采样**的运行从表头里算出 "growth 0%" 并报 **PASS**。
  现在统计的是**数据行**（`tail -n +2`），并且**少于两条采样直接 FAIL**：
  采样间隔是 60s，所以比这更短的运行本来就无法给出趋势——它得到的是 FAIL 加一句「本轮没有可用测量」，
  而不是一个关于代理的结论。
- RSS 读不到时 `|| echo 0` 会让 verdict 从 0 算出「零增长」；现在首次采样读不到即失败
  （与 `bench_minimal.sh` 的 `assert_rss_measurable` 同一处理）。

这两条与 §2.4 的结论同源：**「CI 绿」只有在测量真的发生时才有意义**。

**2. fuzz 明确不进 CI，且不承诺。**

理由：
- `cargo-fuzz` 需要 nightly 工具链、独立的 fuzz 目标与语料库维护；这三件事的成本落在「偶尔发现一个
  解析器边界」上，与当前投入产出的匹配度不如把同等的钱花在线路断言上。
- 更重要的：本仓库对**已知**的输入对抗面已经有**确定性**用例（`content_encoding.rs` 6 个、
  `rule/engine/loader.rs` 5 个），它们随每次 `cargo test` 运行、且是双向验证过的。
  **这就是当前的保证**：不是「随机喂输入」，而是「我们想得到的坏输入都锁住了」。
- 因此路线图里这一条从 ⬜（未做）改为「**不承诺**」，而不是继续挂着一个没有排期的待办。

## 影响

- **正面**：路线图上不再有一条「看起来迟早要做」的空账；soak 从「存在但没人跑、且跑起来也测错」变成
  每周产出一个带判据的报告；「CI 绿」这句话的覆盖面变大，且**边界写清了**（见下）。
- **代价与边界（必须一起读）**：
  - soak **不阻塞 PR**，所以 PR 上「CI 绿」不包含长跑结论；它只在周报里出现。
  - soak 只跑在 **Linux**（RSS 读 `/proc`），本机 macOS 跑不了——这是脚本的既有限制，不是本次新增的。
  - 20 分钟**不足以**发现「1 小时才出现的泄漏」；它抓的是**增长率异常**这一类。真正的长跑仍是人工决定。
  - 采样间隔 60s，因此 20 分钟约有 20 个采样点；**短于两分钟的运行会直接 FAIL**（无法给出趋势），
    这是有意的：宁可说「没测到」，不要给出「没问题」。
  - 不做 fuzz 意味着**解析器仍有未被想到的输入**没有被覆盖；这是被接受的残余风险，写在这里以免被当成
    「已 fuzz 过」。

## 相关证据

- 脚本与上游：`benchmarks/stability_test.sh`（`--minutes`、`UPSTREAM_BIN`、报告中的 `Upstream` 行）
- workflow：`.github/workflows/soak.yml`
- 确定性对抗用例：`relay-core-lib/src/proxy/content_encoding.rs`、`relay-core-lib/src/rule/engine/loader.rs`
