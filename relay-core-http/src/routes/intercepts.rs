use std::sync::Arc;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use relay_core_api::modification::FlowModification;
use relay_core_runtime::{CoreInterceptSnapshot, CoreState, audit::AuditActor};
use relay_core_runtime::rule::InterceptRuleConfig;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub fn router(state: Arc<CoreState>) -> Router {
    Router::new()
        .route("/api/v1/intercepts", post(set_intercept))
        .route("/api/v1/intercepts", get(pending_intercepts))
        .route("/api/v1/intercepts/{key}/resume", post(resume_flow))
        .with_state(state)
}

/// POST /api/v1/intercepts
#[derive(Debug, Deserialize)]
struct SetInterceptRequest {
    url_pattern: String,
    #[serde(default = "default_phase")]
    phase: String,
}

fn default_phase() -> String {
    "request".to_string()
}

async fn set_intercept(
    State(state): State<Arc<CoreState>>,
    Json(req): Json<SetInterceptRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let rule_id = Uuid::new_v4().to_string();
    state
        .create_intercept_rule_from(
            AuditActor::Http,
            rule_id.clone(),
            serde_json::json!({
                "route": "/api/v1/intercepts",
                "url_pattern": req.url_pattern,
                "phase": req.phase
            }),
            InterceptRuleConfig {
                rule_id: rule_id.clone(),
                active: true,
                url_pattern: req.url_pattern.clone(),
                method: None,
                phase: req.phase.clone(),
                name: format!("api-intercept:{}", req.url_pattern),
                priority: 100,
                termination: relay_core_api::rule::RuleTermination::Stop,
            },
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "rule_id": rule_id,
        "url_pattern": req.url_pattern,
        "phase": req.phase,
        "message": "Intercept breakpoint set. Waiting for matching request."
    })))
}

/// GET /api/v1/intercepts
async fn pending_intercepts(State(state): State<Arc<CoreState>>) -> Json<CoreInterceptSnapshot> {
    Json(state.intercept_snapshot().await)
}

/// POST /api/v1/intercepts/{key}/resume
#[derive(Debug, Deserialize)]
struct ResumeRequest {
    #[serde(default = "default_action")]
    action: String,
    #[serde(flatten)]
    modifications: FlowModification,
}

fn default_action() -> String {
    "continue".to_string()
}

async fn resume_flow(
    State(state): State<Arc<CoreState>>,
    Path(key): Path<String>,
    Json(req): Json<ResumeRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ResumeRequest { action, modifications } = req;
    let mods = modifications.into_option();
    state.resolve_intercept_with_modifications_from(AuditActor::Http, key.clone(), &action, mods)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    Ok(Json(serde_json::json!({
        "key": key,
        "action": action,
        "status": "resumed"
    })))
}
