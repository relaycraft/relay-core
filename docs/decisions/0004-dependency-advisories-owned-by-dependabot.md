# 0004 · 依赖漏洞由 Dependabot 接管

- **Status**: Accepted
- **Date**: 2026-09-13
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§16、§24.10）、[`../engine-capability-status.md`](../engine-capability-status.md)（§2.5）

## 背景

本项目此前**没有任何依赖审计**：无 `deny.toml`、无 `cargo-deny`/`cargo-audit` 步骤、无 dependabot
配置。2026-09-13 首次接入 `cargo-deny` 后，第一次运行即查出 7 项存量问题，其中两项落在本引擎的
威胁模型内：

- `rustls-webpki 0.101.7`：3 条**证书校验**漏洞（URI name constraints 被错误接受、
  wildcard name constraints 被接受、CRL 解析可 panic）——本引擎的职责就是校验证书；
- `h2 0.4.15`：空 DATA 帧无界（DoS）——MITM 之后的 H2 路径正在使用。

其余为间接依赖或维护性问题（`quick-xml`、`rustls-pemfile` 停维护、`lru` unsound、
`atomic`、`spin` 被 yank）。

当时留下的问题是：这批存量由谁负责、以什么方式收敛。

## 决策

1. **依赖版本与漏洞收敛由 Dependabot 自动化负责**，不由人工逐条分类（不写 `ignore` 白名单）。
2. 在此之前，CI 的 `audit` job 中：
   - **licences 与 bans 为阻塞门禁**（当前已满足，回归即失败）；
   - **advisories 为追踪项**（`continue-on-error: true`），每次 CI 打印完整清单，但**不阻塞**。
3. `deny.toml` 的 `ignore` 列表**保持为空**。写入任何一条都等于做了一次安全决策，
   而本决策恰恰是不在人工层面做这个决定。
4. 接入 Dependabot 时再评估是否把 advisories 升级为阻塞门禁；**本决策不预设届时一定阻塞**，
   因为自动化升级会带来自己的失败模式（批量 PR、间接依赖不可升级等）。

## 理由

- **人工逐条分类的成本与收益不成比例**：7 条里多数是间接依赖，人工能做的事（升级、确认可达性）
  与 Dependabot 的重叠度很高，而人工分类会很快过期。
- **不写 `ignore` 是关键**：`ignore` 一旦写入就会长期存在并被当作「已接受」，
  而本项目的现状是**没有人评估过这些漏洞是否可达**——所以正确的表达是「已知、未评估、由自动化跟进」，
  而不是「已接受」。
- **保留追踪项而非删除该步骤**：advisories 步骤在每次 CI 上打印清单，
  因此在 Dependabot 接入前，风险是**可见**的；若直接删掉该步骤，就回到「没有任何人看过」的状态。
- **licences/bans 立刻阻塞是安全的**：它们当前通过，且不涉及未决的安全判断。

## 影响

- **正面**：依赖风险在 CI 上持续可见；licence/依赖策略的回归立刻失败；不产生需要维护的 `ignore` 债务。
- **代价（需知晓，且是有意接受）**：
  - 在 Dependabot 接入前，**已知漏洞不会阻塞发布**。其中最需要留意的是
    `rustls-webpki 0.101.7` 的证书校验问题：它在**上游证书校验**路径上，
    若用户依赖本引擎拦截恶意或不一致的证书链，这些问题是实际存在的。
  - 该决策**不声称这批漏洞无害**，只声称**处置方式交给自动化**。
  - `cargo-deny` 的 advisories 步骤是追踪项，因此**CI 全绿不代表没有已知漏洞**。
    这一点必须与 `docs/engine-capability-status.md` §2.5 一起阅读，那一节是唯一的权威清单。
- **接入 Dependabot 时的最小动作**（供届时参考，不在本决策范围内实施）：需要覆盖 **cargo** 与
  **npm** 两个生态（`npm/cli`、`npm/mcp`、`npm/packages/*`），并决定是否启用自动合并——
  建议先只开 PR，人工确认后再自动化。

## 相关证据

- 首次审计结果与完整清单：`docs/engine-capability-status.md` §2.5
- 策略配置：`deny.toml`（`ignore = []` 及其注释说明本决策）
- CI：`.github/workflows/ci.yml` 的 `audit` job（licences/bans 阻塞，advisories 追踪）
- 缺失项：仓库内无 `.github/dependabot.yml`（即本决策第 1 条待落地的那一步）
