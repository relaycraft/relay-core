# 0009 · Daemon 保留 body 前缀

- **Status**: Accepted
- **Date**: 2026-09-22
- **Supersedes**: [`0002`](./0002-body-observation-policy.md) 中「CLI / MCP 不受观察策略影响」这一句
- **Related**: [`0002`](./0002-body-observation-policy.md)

## 背景

[`0002`](./0002-body-observation-policy.md) 把引擎默认保持在 `Off`，桌面宿主单独声明 `Full`。当时无界面运行不需要展示 body。现在 CLI 和 MCP 共用的 daemon 就是 Agent 读流量的地方，`get_flow` 要能看到正文。

## 决策

1. 引擎默认仍是 `Off`。桌面宿主仍在启动时声明 `Full`。
2. Daemon 启动时声明 `BodyObservation::Prefixed`。已有的 tap 仍在流式转发时把观察到的正文写入 flow，上限是 `max_body_size`。这次不把 daemon 改成 `Full`，避免每条响应都先缓冲。
3. MCP 增加只读工具 `list_rules` 与 `get_script`。规则和当前脚本源可以读回来，不必先覆盖再猜。

## 理由

- 全天开着的代理不该为了观察把每条响应都改成 `Full` 缓冲。`Prefixed` 是这条宿主要的观察量。
- 引擎默认不动，测试和还没声明观察的嵌入方行为不变。
- 规则列表和脚本源本来只存在于资源或内存里。只暴露工具的客户端看不见它们。

## 影响

- 用这份代码启动的 daemon，`get_policy` 里的 `body_observation` 是 `prefixed`。
- 已在跑的旧进程要重启后才带上这个声明，以及 `list_rules` / `get_script`。
