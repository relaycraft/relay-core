//! Proxy module
//!
//! This module handles the core proxy server logic.
//!
//! Responsibilities:
//! - `server`: Entry point (`start_proxy`) and main connection loop (accepts connections, spawns handlers).
//! - `http`: HTTP-specific handling (request parsing, MITM, request/response modification).
//! - `tunnel`: HTTP CONNECT tunnel handling (blind TCP forwarding or MITM upgrade).
//! - `websocket`: WebSocket framing, message interception, and forwarding.
//! - `body_codec`: Chooses the `BodyData` representation for a body (utf-8 vs base64). It does
//!   **not** decompress: no gzip/deflate/brotli/zstd handling exists anywhere in the engine yet,
//!   so bodies are passed through in whatever encoding they arrived in (roadmap §24.9).
//! - `body_plan`: Mechanics for carrying out a `BodyPlan` decision — bounded prefix retention for
//!   observation, and bounded materialization when something must rewrite the body.
//! - `content_encoding`: Decodes and re-encodes `Content-Encoding` around inspection, so a rule
//!   sees plaintext and a rewrite is sent with a header that describes what was actually sent.
//! - `tap`: Tapping body streams for UI updates.
//! - `outbound`: Outbound connector abstraction (direct / upstream proxy).

pub mod body_codec;
pub mod body_plan;
pub mod budget;
pub mod circuit_breaker;
pub mod content_encoding;
pub mod http;
pub mod http_utils;
pub mod outbound;
pub mod server;
pub mod tap;
pub mod throttle;
pub mod tunnel;
pub mod websocket;

pub use server::start_proxy;
