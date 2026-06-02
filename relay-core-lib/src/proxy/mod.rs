//! Proxy module
//!
//! This module handles the core proxy server logic.
//!
//! Responsibilities:
//! - `server`: Entry point (`start_proxy`) and main connection loop (accepts connections, spawns handlers).
//! - `http`: HTTP-specific handling (request parsing, MITM, request/response modification).
//! - `tunnel`: HTTP CONNECT tunnel handling (blind TCP forwarding or MITM upgrade).
//! - `websocket`: WebSocket framing, message interception, and forwarding.
//! - `body_codec`: Handling of HTTP body streams (decompression, buffering for inspection).
//! - `tap`: Tapping body streams for UI updates.
//! - `outbound`: Outbound connector abstraction (direct / upstream proxy).

pub mod body_codec;
pub mod budget;
pub mod circuit_breaker;
pub mod http;
pub mod http_utils;
pub mod outbound;
pub mod server;
pub mod tap;
pub mod throttle;
pub mod tunnel;
pub mod websocket;

pub use server::start_proxy;
