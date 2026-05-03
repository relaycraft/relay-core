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
//! | Version | Status  | Notes                                  |
//! |---------|---------|----------------------------------------|
//! | 1       | current | All tools listed in [`tools`] are stable |
//!
//! ## Stable tools (v1)
//! - `search_flows` / `get_flow` / `get_metrics` — read-only, no side effects
//! - `set_rule` / `delete_rule` / `mock_url` — rule engine management
//! - `set_intercept` / `get_pending_intercepts` / `resume_flow` — breakpoint flow
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
pub const TOOL_CONTRACT_VERSION: u32 = 1;

pub mod server;
pub mod tools;
pub mod resources;

pub use server::{ProbeServer, ProbeConfig, ProbeTransport};
