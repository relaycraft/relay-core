use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

/// GET /api/v1/version
pub fn router() -> Router {
    Router::new().route("/api/v1/version", get(handler))
}

async fn handler() -> Json<Value> {
    #[cfg(feature = "script")]
    let caps = json!([
        "flows",
        "rules",
        "intercepts",
        "mock",
        "metrics",
        "status",
        "events",
        "scripts"
    ]);
    #[cfg(not(feature = "script"))]
    let caps = json!([
        "flows",
        "rules",
        "intercepts",
        "mock",
        "metrics",
        "status",
        "events"
    ]);

    Json(json!({
        "engine_version": env!("CARGO_PKG_VERSION"),
        "api_version": "1",
        "capabilities": caps
    }))
}
