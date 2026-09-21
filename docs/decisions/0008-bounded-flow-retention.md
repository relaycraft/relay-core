# 0008 · 流量历史默认有界

- **Status**: Accepted
- **Date**: 2026-09-22
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§24.8）、[`../engine-capability-status.md`](../engine-capability-status.md)

## 背景

热数据本来就是 LRU 约 200 条，落盘用 SQLite，停掉 MCP/CLI 不丢历史。缺的是出站：`RetentionPolicy` 的默认是每个字段都空，也就是永不裁剪，而且没有「清掉已捕获流量」的入口。家用或全天挂着代理时，库和 WAL 只进不出。

CLI 和 MCP 共用一个 daemon、一份 `~/.relay-core` 是对的：浏览器过代理，Agent 才能看见同一份流量。为了限容去拆第二个数据目录，只会变成「CLI 有流量、MCP 没有」。

## 决策

1. 产品默认改为 **最多 5000 条，且超过 7 天删除**（`max_flows = 5000`，`max_age_secs = 604800`）。审计事件默认仍不设界。
2. 存储层的 `unbounded()` 保留，给明确要留全量历史的宿主。它不再是 `ProxyPolicy` 的默认值。
3. `patch_policy` 接受 `retention`。没写到的界保持原样，`null` 去掉该界。其余策略字段仍拒绝，超时等继续走 `update_policy`。
4. 增加 MCP `clear_flows`（runtime `clear_captured_flows`）：立刻删除流量和摘要，并 `wal_checkpoint(TRUNCATE)`。规则、策略、审计不动。
5. 不拆默认数据目录。要隔离实验时再开第二个 `RELAY_DATA_DIR`，或用 `relay start --in-memory`。

## 理由

- 内存上限和落盘本身不用改。要收的是默认「只进不出」。
- 条数和天数一起生效：忙的一下午会先撞上 5000，安静地挂一周也会丢掉过期记录。
- 整表替换式的 retention 补丁分不清「没写」和「改成无界」。按字段三态更新，才能只改一边。
- 清空必须走 flow actor，否则会和正在落盘的写入抢同一张表，内存里的热数据也会残留。
- 共享 daemon 不变。限容和清空已经能让一份库长期开着。

## 影响

- **下次启动就会裁**。已有的 `~/.relay-core` 在默认策略下，约 30 秒后开始删掉超出 5000 条或老于 7 天的流量。这是有意的行为变更。
- 审计日志不受这次默认值影响，也不被 `clear_flows` 删除。
- 删行之后 WAL 会 checkpoint 截断；主库文件里的空页留给后续写入复用，不会为每次裁剪做 `VACUUM`。
- 显式 `retention` 全空（或 `RetentionPolicy::unbounded()`）仍表示永不裁剪。
