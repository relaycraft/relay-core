//! relay-core-probe — MCP server adapter
//!
//! Exposes relay-core traffic management capabilities to AI agents via the
//! Model Context Protocol (MCP). Sits alongside relay-core-tauri as a peer
//! adapter layer over `CoreState`.
//!
//! # Tool contract version
//!
//! [`TOOL_CONTRACT_VERSION`] identifies the stability of the MCP tool
//! interface. Consumers (agents, orchestrators) should check this on connect
//! and handle unknown tool names gracefully for forward-compatibility.
//!
//! | Version | Status      | Notes                                                        |
//! |---------|-------------|--------------------------------------------------------------|
//! | 1       | superseded  | 15 tools, JSON returned as text only                         |
//! | 2       | current     | 18 tools at introduction; later tools do not bump this       |
//!
//! ## What changed in v2 (breaking)
//!
//! - Every tool returns `structuredContent` **and** the serialized result as text, so a client reads
//!   typed fields instead of parsing a pretty-printed blob. The two halves are the same shape.
//! - `search_flows` returns `{ count, flows }` instead of a bare array.
//! - Write tools return `{ ok, message, … }` instead of a sentence.
//! - Every tool carries `readOnlyHint` / `destructiveHint` / `idempotentHint` annotations, and the
//!   lifecycle tools are listed first.
//! - `proxy_status` / `proxy_start` / `proxy_stop` join the surface; traffic tools answer
//!   `proxy_not_running` rather than an empty list when nothing is being captured.
//!
//! ## Stable tools (v2)
//! - `proxy_status` / `proxy_start` / `proxy_stop` — proxy lifecycle of the owning daemon
//! - `search_flows` / `get_flow` / `get_metrics` — read-only, no side effects
//! - `replay_flow` / `export_har` — flow replay and HAR export
//! - `set_rule` / `list_rules` / `delete_rule` / `mock_url` — rule engine management
//! - `set_intercept` / `get_pending_intercepts` / `resume_flow` — breakpoint flow
//! - `get_policy` / `update_policy` / `patch_policy` — policy management
//! - `clear_flows` — drop captured history without touching rules or audit
//! - `get_script` / `set_script` — read and load Deno scripts for dynamic modification
//!
//! # Quick start
//! ```no_run
//! use relay_core_probe::{ProbeServer, ProbeConfig, ProbeTransport};
//! use relay_core_runtime::CoreState;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let state = Arc::new(CoreState::new(None).await);
//!     let config = ProbeConfig { transport: ProbeTransport::Stdio };
//!     ProbeServer::new(config, state).run().await.unwrap();
//! }
//! ```

/// Current MCP tool contract version.
///
/// Increment when any tool name, parameter, or semantic meaning changes in a
/// breaking way. Minor additions (new optional params, new tools) do NOT
/// require a version bump — consumers must ignore unknown tools/params.
pub const TOOL_CONTRACT_VERSION: u32 = 2;

pub mod bridge;
pub mod resources;
pub mod server;
pub mod tools;

pub use bridge::{BridgeError, BridgeOptions, run_stdio_bridge};
pub use server::{ProbeConfig, ProbeServer, ProbeTransport};
