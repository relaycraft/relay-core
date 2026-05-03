//! Internal transport/engine crate for [relay-core](https://crates.io/crates/relay-core).
//! Contains capture sources, MITM/TLS, HTTP/WebSocket proxy, and rule engine.
//!
//! **Users should depend on `relay-core` instead.** Direct use of `relay-core-lib`
//! is intended only for advanced integrators building custom orchestration layers.

pub mod capture;
pub mod intercept;
pub mod tls;
pub mod proxy;
pub mod rule;
pub mod utils;
pub mod error;
pub mod metrics;

// Re-export rule module as rule_engine for backward compatibility
pub use rule as rule_engine;


// Re-exports to maintain backward compatibility
pub use capture::source as engine;
pub use intercept::types as interceptor;

// Re-export start_proxy for convenience
pub use proxy::start_proxy;
pub use intercept::types::{Interceptor, InterceptionResult, NoOpInterceptor};

pub fn init() {
    // tracing::info!("Relay Core Lib Initialized");
}
