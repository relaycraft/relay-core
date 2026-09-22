use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, patch},
};
use relay_core_api::policy::ProxyPolicyPatch;
use relay_core_runtime::audit::AuditActor;
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
    Json(policy_json(&ctx))
}

/// The policy the control API hands to clients. Upstream passwords stay masked.
fn policy_json(ctx: &HttpApiContext) -> Value {
    let policy = ctx.policy.policy_snapshot();
    let mut val = serde_json::to_value(&policy).unwrap_or_default();
    if let Some(obj) = val.as_object_mut()
        && let Some(upstream) = obj.get_mut("upstream")
        && let Some(auth) = upstream.get_mut("auth")
    {
        auth["password"] = serde_json::Value::String("***".to_string());
    }
    val
}

/// PATCH /api/v1/policy — body: ProxyPolicyPatch JSON. Response is the updated policy.
async fn patch_policy(
    State(ctx): State<Arc<HttpApiContext>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let patch: ProxyPolicyPatch = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid patch JSON: {}", e),
        )
    })?;

    // R-N1: upstream connector is built once at startup; changing upstream
    // at runtime has no effect on active connections. Return 409 to avoid
    // the silent-failure UX.
    if patch.upstream.is_some() {
        let current = ctx.policy.policy_snapshot();
        if current.upstream != patch.upstream {
            return Err((
                StatusCode::CONFLICT,
                "upstream proxy config change requires proxy restart".to_string(),
            ));
        }
    }

    ctx.policy
        .patch_policy_from(AuditActor::Http, "/api/v1/policy PATCH".to_string(), patch);

    // Callers, including the Web UI, replace their local policy with this body.
    // A status acknowledgement has no `redaction` or runtime fields, so the next
    // render reads `undefined.enabled` and the runtime panel goes blank.
    Ok(Json(policy_json(&ctx)))
}

#[cfg(test)]
mod tests {
    use super::router;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode},
    };
    use relay_core_runtime::CoreState;
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn patch_returns_the_updated_policy() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(crate::server::HttpApiContext::new(state));
        let app = router(ctx);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"redaction":{"enabled":false}}"#))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let json: Value = serde_json::from_slice(&body).expect("body should be valid json");
        assert_eq!(json["redaction"]["enabled"], false);
        assert!(json["max_body_size"].is_number());
        assert!(json["rule_body_inspect_budget"].is_number());
        assert!(json["request_timeout_ms"].is_number());
        assert!(json["transparent_enabled"].is_boolean());
        assert!(json.get("status").is_none());
    }
}
