use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, patch},
};
use relay_core_api::policy::{ProxyPolicyPatch, UpstreamProxyConfig};
use relay_core_runtime::audit::AuditActor;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/policy", get(get_policy))
        .route("/api/v1/policy", patch(patch_policy))
        .with_state(ctx)
}

/// GET /api/v1/policy
async fn get_policy(State(ctx): State<Arc<HttpApiContext>>) -> Json<Value> {
    let policy = ctx.policy.policy_snapshot();
    let mut val = serde_json::to_value(&policy).unwrap_or_default();
    // Mask upstream auth password in response
    if let Some(obj) = val.as_object_mut()
        && let Some(upstream) = obj.get_mut("upstream")
        && let Some(auth) = upstream.get_mut("auth")
    {
        auth["password"] = serde_json::Value::String("***".to_string());
    }
    Json(val)
}

/// PATCH /api/v1/policy  — body: ProxyPolicyPatch JSON
async fn patch_policy(
    State(ctx): State<Arc<HttpApiContext>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    #[derive(Deserialize)]
    struct PatchBody {
        #[serde(default)]
        redaction: Option<relay_core_api::policy::RedactionPolicyPatch>,
        #[serde(default)]
        upstream: Option<UpstreamProxyConfig>,
    }

    let patch_body: PatchBody = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid patch JSON: {}", e),
        )
    })?;

    let patch = ProxyPolicyPatch {
        redaction: patch_body.redaction,
        upstream: patch_body.upstream.clone(),
    };

    // R-N1: upstream connector is built once at startup; changing upstream
    // at runtime has no effect on active connections. Return 409 to avoid
    // the silent-failure UX.
    if patch_body.upstream.is_some() {
        let current = ctx.policy.policy_snapshot();
        if current.upstream != patch_body.upstream {
            return Err((
                StatusCode::CONFLICT,
                "upstream proxy config change requires proxy restart".to_string(),
            ));
        }
    }

    ctx.policy
        .patch_policy_from(AuditActor::Http, "/api/v1/policy PATCH".to_string(), patch);

    Ok(Json(serde_json::json!({ "status": "ok" })))
}
