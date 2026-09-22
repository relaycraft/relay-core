//! Internal support crate for [relay-core](https://crates.io/crates/relay-core).
//! Shared data contracts (Flow, Rule, Policy, etc.) used across all relay-core crates.
//!
//! **Users should depend on `relay-core` instead** — its public API re-exports
//! the types defined here under `relay_core_runtime::flow`, `relay_core_runtime::policy`, etc.

pub mod body_plan;
pub mod event;
pub mod flow;
pub mod flow_query;
pub mod grpc;
pub mod har;
pub mod modification;
pub mod policy;
pub mod rule;
pub mod sse;

/// Command users type.
///
/// `@relay-core/cli` installs this name. The native file inside the platform
/// package is `relay-core-cli`; help text, errors and audit labels must use
/// this command, not the file name and not `relay`.
pub const CLI_COMMAND: &str = "relay-core";

// Placeholder
pub fn version() -> &'static str {
    "0.1.0"
}
