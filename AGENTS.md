# RelayCore AI Agent Guide (AGENTS.md)

Welcome, AI Agent or Developer. This document outlines the essential information, workflows, and standards for developing the **RelayCore** engine.

## 1. Project Context
RelayCore is a high-performance, Rust-based traffic interception engine designed to replace the legacy Python backend in RelayCraft.
It is both the future RelayCraft proxy engine and a standalone Rust proxy platform with multiple adapters (`relay-core-cli`, `relay-core-http`, `relay-core-probe`, `relay-core-tauri`) built on top of a shared runtime.

## 2. Development Philosophy
We strictly follow **Test-Driven Development (TDD)** and **Offline-First** principles.

### 2.1 TDD Workflow
1.  **Write a Failing Test**: Create a test case in `relay-core-tauri/tests/offline_dev_tests.rs` (or similar adapter/runtime test) that defines the expected behavior (contract).
2.  **Implement Logic**: Write the minimal code in `relay-core-lib`, `relay-core-runtime`, or the relevant adapter crate to make the test pass.
3.  **Refactor**: Clean up the code while ensuring tests remain green.
4.  **Verify**: Run `cargo test` to confirm.

### 2.2 Offline-First Development
*   **Do not rely on the full Tauri app** for core logic development.
*   Use `relay-core-tauri/tests/offline_dev_tests.rs` as the default contract suite to simulate frontend-backend interactions.
*   Prefer validating shared behavior at the `relay-core-runtime` / adapter boundary so the same core can serve CLI, TUI, HTTP, MCP, and Tauri hosts.
*   This ensures the core logic is decoupled from the UI shell and can be tested in isolation (CI/CD friendly).

## 3. Documentation Standards
*   **Architecture**: See `.ai/ARCHITECTURE.md`.
*   **Specifications**: Before implementing any major module, creating a detailed design document in `.ai/` is **MANDATORY**.
    *   Format: `.ai/NNN-module-name.md` (e.g., `001-traffic-modification.md`).
    *   Content: Goals, Data Structures, API Design, Edge Cases.
*   **Bilingual**: All major documentation should be in English and Chinese (where possible/practical).

## 4. Architecture Overview
*   **`relay-core-lib`**: The packet/connection engine (capture, proxy, MITM, protocol parsing, interception hooks).
*   **`relay-core-runtime`**: The host-agnostic orchestration layer (`CoreState`, actor-style state coordination, flow broadcasting, rule/intercept management).
*   **`relay-core-api`**: Shared types and contracts (Flow, Rule, Event, Policy, Modification).
*   **`relay-core-script`**: Deno/V8-based script interceptor for dynamic traffic modification.
*   **`relay-core-cli`**: Standalone CLI and TUI host for local proxy operation.
*   **`relay-core-http`**: REST + SSE adapter for Web UI, editor extensions, and language-agnostic integrations.
*   **`relay-core-probe`**: MCP adapter that exposes RelayCore capabilities to AI agents and automation.
*   **`relay-core-tauri`**: Tauri adapter used by the RelayCraft desktop application.

## 5. Key Commands

### Quality gate (matches CI `quality` job — run before push or release)

```bash
./scripts/ci-check.sh          # fmt --check → clippy → test --workspace
./scripts/install-git-hooks.sh # installs pre-push hook that runs ci-check.sh
```

GitHub Actions runs the same script in `.github/workflows/ci.yml`. Tag workflows (`release`, `publish-npm`, `publish`) also run `quality` before building.

*   **Run Offline Tests**:
    ```bash
    cargo test --package relay-core-tauri --test offline_dev_tests
    ```
*   **Run Script Engine Tests**:
    ```bash
    cargo test --package relay-core-script
    ```
*   **Run All Tests**:
    ```bash
    cargo test --workspace
    ```
*   **Check Lints**:
    ```bash
    cargo clippy --workspace -- -D warnings
    ```

## 6. Commit Message Convention

### Type whitelist

- `feat` — new user-facing capability
- `fix` — bug fix
- `perf` — performance improvement
- `refactor` — internal restructuring, no behavior change
- `test` — adding or updating tests
- `docs` — documentation only
- `chore` — tooling, dependencies, maintenance

### Format

```
<type(scope): subject>
```

- Subject: single intent, ≤72 chars, present-tense
- Scope: crate or module (e.g. `runtime`, `lib`, `http`, `cli`)
- For multi-file changes (≥5 files), add a bullet body: first line explains why, second explains impact
- No release notes or changelog blocks needed

### Examples

```
feat(runtime): add SSE stream for real-time flow events
fix(lib): correct HTTP header size calculation for chunked encoding
refactor(runtime): introduce narrow capability-domain traits for CoreState
test(tauri): add HAR compatibility regression for redirectURL
chore: switch license to MIT
```

### Pre-commit checklist

- [ ] `./scripts/ci-check.sh` passes (or pre-push hook installed)
- [ ] Subject is single intent, ≤72 chars
- [ ] Type is in the whitelist
- [ ] No secrets, credentials, or generated keys committed

---

**Remember**: Your primary goal is to build a robust, testable core engine. The frontend integration is a secondary step that happens only after the core logic is proven via offline tests.
