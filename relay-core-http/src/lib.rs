//! relay-core-http — REST + SSE HTTP API adapter
//!
//! Exposes relay-core capabilities as a versioned REST API so any language
//! or tool can integrate without a Rust dependency.
//!
//! # API surface (v1)
//!
//! | Method | Path                              | Description                        |
//! |--------|-----------------------------------|------------------------------------|
//! | GET    | /api/v1/version                   | Server & API version               |
//! | GET    | /api/v1/metrics                   | Runtime metrics                    |
//! | GET    | /api/v1/metrics/prometheus        | Prometheus text metrics            |
//! | GET    | /api/v1/status                    | Runtime lifecycle snapshot         |
//! | GET    | /api/v1/flows                     | Search flows (query params)         |
//! | GET    | /api/v1/flows/{id}                | Get single flow                    |
//! | GET    | /api/v1/rules                     | List active rules                  |
//! | PUT    | /api/v1/rules                     | Add/replace a rule (full Rule JSON) |
//! | DELETE | /api/v1/rules/{id}                | Delete a rule                      |
//! | POST   | /api/v1/mock                      | Quickly mock a URL pattern         |
//! | POST   | /api/v1/intercepts                | Set a one-shot intercept breakpoint |
//! | GET    | /api/v1/intercepts                | List pending intercepts            |
//! | POST   | /api/v1/intercepts/{key}/resume   | Resume an intercepted flow         |
//! | GET    | /api/v1/events                    | SSE stream of live flow events     |

mod routes;
pub mod server;
#[cfg(feature = "webui")]
pub mod webui;

pub use server::{HttpApiConfig, HttpApiServer};
