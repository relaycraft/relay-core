use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
use relay_core_api::rule::Rule;
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::rule::MockResponseRuleConfig;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/rules", get(list_rules))
        .route("/api/v1/rules", put(set_rule))
        .route("/api/v1/rules/{id}", delete(delete_rule))
        .route("/api/v1/mock", post(mock_url))
        .with_state(ctx)
}

/// GET /api/v1/rules
async fn list_rules(State(ctx): State<Arc<HttpApiContext>>) -> Json<Value> {
    let rules = ctx.rules.get_rules().await;
    Json(serde_json::to_value(&rules).unwrap_or_default())
}

/// PUT /api/v1/rules  — body: full Rule JSON
async fn set_rule(
    State(ctx): State<Arc<HttpApiContext>>,
    Json(rule_val): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let rule: Rule = serde_json::from_value(rule_val)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid rule JSON: {}", e)))?;
    let rule_id = rule.id.clone();
    ctx.rules
        .upsert_rule_from(
            AuditActor::Http,
            "rule.upsert",
            rule_id.clone(),
            serde_json::json!({ "route": "/api/v1/rules" }),
            rule,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({ "id": rule_id, "status": "ok" })))
}

/// DELETE /api/v1/rules/{id}
async fn delete_rule(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let deleted = ctx
        .rules
        .delete_rule_from(
            AuditActor::Http,
            "rule.delete",
            id.clone(),
            serde_json::json!({ "route": "/api/v1/rules/{id}" }),
            &id,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(serde_json::json!({ "id": id, "status": "deleted" })))
}

/// POST /api/v1/mock  — quickly create a MockResponse rule
#[derive(Debug, Deserialize)]
struct MockRequest {
    url_pattern: String,
    status: u16,
    #[serde(default)]
    body: String,
    #[serde(default = "default_content_type")]
    content_type: String,
}

fn default_content_type() -> String {
    "application/json".to_string()
}

async fn mock_url(
    State(ctx): State<Arc<HttpApiContext>>,
    Json(req): Json<MockRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let rule_id = format!("api-mock-{}", Uuid::new_v4());
    ctx.rules
        .create_mock_response_rule_from(
            AuditActor::Http,
            rule_id.clone(),
            serde_json::json!({
                "route": "/api/v1/mock",
                "url_pattern": req.url_pattern,
                "status": req.status
            }),
            MockResponseRuleConfig {
                rule_id: rule_id.clone(),
                url_pattern: req.url_pattern.clone(),
                name: format!("api-mock:{}", req.url_pattern),
                status: req.status,
                content_type: req.content_type,
                body: req.body,
            },
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "rule_id": rule_id,
        "url_pattern": req.url_pattern,
        "status": req.status
    })))
}
