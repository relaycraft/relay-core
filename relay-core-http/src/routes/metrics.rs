use std::sync::Arc;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::get,
};
use relay_core_runtime::{
    CoreAuditQuery, CoreAuditSnapshot, CoreMetrics, CoreStatusSnapshot,
    audit::{AuditActor, AuditEventKind, AuditOutcome},
};
use crate::server::HttpApiContext;
use serde::Deserialize;

/// GET /api/v1/metrics
pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/metrics", get(get_metrics))
        .route("/api/v1/metrics/prometheus", get(get_metrics_prometheus))
        .route("/api/v1/audit", get(get_audit))
        .route("/api/v1/status", get(get_status))
        .with_state(ctx)
}

async fn get_metrics(State(ctx): State<Arc<HttpApiContext>>) -> Json<CoreMetrics> {
    Json(ctx.status.get_metrics().await)
}

async fn get_metrics_prometheus(State(ctx): State<Arc<HttpApiContext>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        ctx.status.get_metrics_prometheus_text().await,
    )
}

async fn get_status(State(ctx): State<Arc<HttpApiContext>>) -> Json<CoreStatusSnapshot> {
    Json(ctx.status.status_snapshot())
}

#[derive(Debug, Deserialize)]
struct AuditQueryParams {
    since_ms: Option<u64>,
    until_ms: Option<u64>,
    actor: Option<String>,
    kind: Option<String>,
    outcome: Option<String>,
    limit: Option<usize>,
}

async fn get_audit(
    State(ctx): State<Arc<HttpApiContext>>,
    Query(params): Query<AuditQueryParams>,
) -> Json<CoreAuditSnapshot> {
    let query = CoreAuditQuery {
        since_ms: params.since_ms,
        until_ms: params.until_ms,
        actor: params.actor.as_deref().and_then(|v| v.parse::<AuditActor>().ok()),
        kind: params
            .kind
            .as_deref()
            .and_then(|v| v.parse::<AuditEventKind>().ok()),
        outcome: params
            .outcome
            .as_deref()
            .and_then(|v| v.parse::<AuditOutcome>().ok()),
        limit: params.limit.unwrap_or(50),
    };
    Json(ctx.audit.query_audit_snapshot(query).await)
}
