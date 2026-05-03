use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

/// GET /api/v1/version
pub fn router() -> Router {
    Router::new().route("/api/v1/version", get(handler))
}

async fn handler() -> Json<Value> {
    Json(json!({
        "engine_version": env!("CARGO_PKG_VERSION"),
        "api_version": "1",
        "capabilities": ["flows", "rules", "intercepts", "mock", "metrics", "status", "events"]
    }))
}
