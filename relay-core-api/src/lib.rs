//! Internal support crate for [relay-core](https://crates.io/crates/relay-core).
//! Shared data contracts (Flow, Rule, Policy, etc.) used across all relay-core crates.
//!
//! **Users should depend on `relay-core` instead** — its public API re-exports
//! the types defined here under `relay_core_runtime::flow`, `relay_core_runtime::policy`, etc.

pub mod flow;
pub mod rule;
pub mod event;
pub mod policy;
pub mod modification;
pub mod har;

// Placeholder
pub fn version() -> &'static str {
    "0.1.0"
}
