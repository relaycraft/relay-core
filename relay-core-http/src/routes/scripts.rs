use crate::server::HttpApiContext;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use relay_core_runtime::audit::AuditActor;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/script", post(load_script))
        .with_state(ctx)
}

#[derive(Debug, Deserialize)]
struct LoadScriptRequest {
    script: String,
}

async fn load_script(
    State(ctx): State<Arc<HttpApiContext>>,
    Json(req): Json<LoadScriptRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if req.script.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "script body must not be empty".to_string(),
        ));
    }

    ctx.scripts
        .load_script_from(
            AuditActor::Http,
            "api.script.upload".to_string(),
            &req.script,
        )
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    Ok(Json(serde_json::json!({
        "status": "loaded",
        "script_bytes": req.script.len()
    })))
}
