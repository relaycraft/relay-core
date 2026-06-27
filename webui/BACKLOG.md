# RelayCore WebUI — Backlog（应做未做）

> **用途**：v0.9.0 Alpha 可交付；本文件记录相对合理产品预期、但**尚未完成**的项。  
> **更新**：2026-06-28 · 基线 **v0.9.0**

状态：`✅` 已完成 · `⚠️` 部分 · `❌` 未做 · `⏸` 移出范围

---

## 发版范围（v0.9.0 可宣称）

`relay run --web` → 浏览器 Dashboard：Flows / Rules / Workshop / Scripts / Settings、快捷键、SSE。

**不宣称**：mitmweb 级 polish、UI i18n、内嵌 AI、Audit、性能基线。

---

## A. 前端

| ID | 项 | 优先级 | 状态 |
|----|-----|--------|------|
| A1 | CodeMirror 6 懒加载 | P1 | ❌ |
| A2 | Shiki 只读高亮 | P2 | ❌ |
| A3 | Workshop Split View | P1 | ❌ |
| A4 | Scripts 控制台接 SSE | P1 | ❌ |
| A5 | 面板拖拽 + localStorage | P3 | ❌ |
| A6 | 命令面板 fuzzy 跳 flow | P3 | ❌ |
| A7 | Settings upstream + 409 | P2 | ⚠️ |
| A8 | Audit 子页 | P3 | ❌ |
| A9 | Rule Trace 完整数据 | P2 | ⚠️ |
| A10 | Body 按 Tab 懒加载 | P2 | ⚠️ |
| A11 | 重启检测 + 未保存确认 | P2 | ❌ |
| A12–A14 | vitest SSE/MSW、Playwright | P2–P3 | ❌ |
| A15 | UI i18n | — | ⏸ |
| A16 | 完整 ARIA | P3 | ❌ |
| A17 | Bearer + SSE | P2 | ❌ |

## B. 后端

| ID | 项 | 优先级 | 状态 |
|----|-----|--------|------|
| B1 | `GET /rules/schema` | P1 | ❌ |
| B2 | Intercept SSE | P1 | ❌ |
| B3/B4 | script-log SSE + GET /script | P1 | ❌ |
| B5 | 非 loopback 无 token 拒绝启动 | P2 | ❌ |
| B6–B9 | body/trace/cURL/owner | P2–P3 | ❌ |

**v0.9 已做**：`GET /intercepts` → `items[]`；`RuleInterceptor` Inspect 挂起。

## C. 测试

| ID | 项 | 状态 |
|----|-----|------|
| C1 | `webui_contract_tests.rs` | ❌ |
| C2 | `benchmarks/webui-perf/` | ❌ |
| C3 | `ci-check.sh` 全绿 | ✅ |

## D. 移出范围

WebUI 内 AI → MCP；UI 中英切换；Monaco / mitmweb content view。

## E. 发版后迭代顺序

C1 → B1+A1 → B2+A3 → B3/B4+A4 → B5+A17 → A7/B7/A9 → C2/A14
