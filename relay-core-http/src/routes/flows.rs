use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use relay_core_api::flow::Layer;
use relay_core_api::har::flow_to_har_entry;
use relay_core_api::modification::FlowQuery;
use relay_core_api::modification::FlowSummary;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/flows/export/har", get(export_har))
        .route("/api/v1/flows", get(search_flows))
        .route("/api/v1/flows/{id}/har", get(get_flow_har))
        .route("/api/v1/flows/{id}/trace", get(get_flow_trace))
        .route("/api/v1/flows/{id}", get(get_flow))
        .route("/api/v1/flows/{id}/replay", post(replay_flow))
        .with_state(ctx)
}

/// Query parameters for GET /api/v1/flows
#[derive(Debug, Deserialize)]
pub struct FlowQueryParams {
    pub host: Option<String>,
    pub path: Option<String>,
    pub path_contains: Option<String>,
    pub method: Option<String>,
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
    pub has_error: Option<bool>,
    pub is_websocket: Option<bool>,
    /// Include the CONNECT instruction flows that opened tunnels. Off unless asked for.
    pub include_connect: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct FlowSearchResponse {
    items: Vec<FlowSummary>,
    returned: usize,
    limit: usize,
    offset: usize,
}

async fn search_flows(
    State(ctx): State<Arc<HttpApiContext>>,
    Query(params): Query<FlowQueryParams>,
) -> Json<FlowSearchResponse> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let offset = params.offset.unwrap_or(0);
    let query = FlowQuery {
        host: params.host,
        path_contains: params.path_contains.or(params.path),
        method: params.method,
        status_min: params.status_min,
        status_max: params.status_max,
        has_error: params.has_error,
        is_websocket: params.is_websocket,
        include_connect: params.include_connect.unwrap_or(false),
        limit: Some(limit),
        offset: Some(offset),
    };
    let items = ctx.flows.search_flows(query).await;
    Json(FlowSearchResponse {
        returned: items.len(),
        items,
        limit,
        offset,
    })
}

/// GET /api/v1/flows/{id}/trace — rule evaluations for one captured flow.
///
/// The trace is diagnostic state for the running process, not part of the stored flow: it answers
/// "why did this rule not fire" for a rule that was evaluated and declined. A missing flow is 404,
/// not an empty trace, so "the flow is gone" and "no rules were evaluated" stay distinguishable.
async fn get_flow_trace(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if ctx.flows.get_flow(&id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let snapshot = ctx.flows.rule_trace(&id);
    Ok(Json(serde_json::json!({
        "flow_id": id,
        "events": snapshot.events,
        "truncated": snapshot.truncated,
    })))
}

async fn get_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match ctx.flows.get_flow_detail(&id).await {
        Some(flow) => Ok(Json(serde_json::to_value(&flow).unwrap_or_default())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Debug, Deserialize)]
pub struct ReplayParams {
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

/// POST /api/v1/flows/{id}/replay
///
/// Re-sends the captured HTTP request through the running proxy and returns the new response.
/// The replay is captured as a new flow. Only HTTP flows are accepted.
///
/// Query parameters:
/// - `accept_invalid_certs` (bool, default false): skip TLS verification on the replay client
async fn replay_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
    Query(params): Query<ReplayParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let flow = ctx
        .flows
        .get_flow(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, format!("Flow {} not found", id)))?;

    let (method, url, headers, body) = match &flow.layer {
        Layer::Http(http) => (
            http.request.method.clone(),
            http.request.url.to_string(),
            http.request.headers.clone(),
            http.request.body.clone(),
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Replay only supports HTTP flows".to_string(),
            ));
        }
    };

    let port = crate::replay::require_running_proxy(&ctx.status.status_snapshot())
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?;
    let ca_pem = ctx.status.ca_cert_pem();
    let response = crate::replay::send_captured_request(
        &method,
        &url,
        &headers,
        body.as_ref(),
        port,
        params.accept_invalid_certs,
        ca_pem.as_deref(),
    )
    .await
    .map_err(|error| {
        let status = if error.starts_with("Invalid method") || error.contains("not valid base64") {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        (status, error)
    })?;

    Ok(Json(serde_json::json!({
        "status": response.status,
        "url": response.url,
        "headers": response.headers,
        "body": response.body,
    })))
}

/// GET /api/v1/flows/{id}/har — export a single flow as HAR entry
async fn get_flow_har(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let flow = ctx.flows.get_flow(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    let entry = flow_to_har_entry(&flow);
    let har = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "RelayCore", "version": env!("CARGO_PKG_VERSION") },
            "entries": [entry]
        }
    });
    Ok(Json(har))
}

/// GET /api/v1/flows/export/har — batch export flows as HAR log
async fn export_har(
    State(ctx): State<Arc<HttpApiContext>>,
    Query(params): Query<FlowQueryParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let query = FlowQuery {
        host: params.host,
        path_contains: params.path_contains.or(params.path),
        method: params.method,
        status_min: params.status_min,
        status_max: params.status_max,
        has_error: params.has_error,
        is_websocket: params.is_websocket,
        include_connect: params.include_connect.unwrap_or(false),
        limit: Some(limit),
        offset: params.offset,
    };
    let summaries = ctx.flows.search_flows(query).await;
    let mut entries = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        if let Some(flow) = ctx.flows.get_flow(&summary.id).await {
            entries.push(flow_to_har_entry(&flow));
        }
    }

    Json(serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "RelayCore", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::router;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode},
    };
    use relay_core_api::event::FlowEvent;
    use relay_core_api::flow::{Direction, Flow};
    use relay_core_api::rule::{RuleOutcome, RuleStage};
    use relay_core_runtime::CoreState;
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn flow(id: &str) -> Flow {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "start_time": "2026-05-20T10:00:00Z",
            "end_time": null,
            "network": {
                "client_ip": "127.0.0.1", "client_port": 1234,
                "server_ip": "0.0.0.0", "server_port": 0,
                "protocol": "TCP", "tls": false, "tls_version": null, "sni": null
            },
            "layer": { "type": "Http", "data": {
                "request": {
                    "method": "GET", "url": "https://example.com/",
                    "version": "HTTP/1.1", "headers": [], "cookies": [], "query": [], "body": null
                },
                "response": null, "error": null
            }},
            "tags": [], "meta": {}
        }))
        .expect("sample flow")
    }

    #[tokio::test]
    async fn a_flow_trace_reports_evaluations_and_404s_for_a_missing_flow() {
        let state = Arc::new(CoreState::new(None).await);
        let id = uuid::Uuid::new_v4().to_string();
        state.upsert_flow(Box::new(flow(&id)));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        state.publish_flow_event(FlowEvent::RuleEvaluated {
            flow_id: id.parse().expect("uuid"),
            direction: Direction::ClientToServer,
            rule_id: "post-only".to_string(),
            stage: RuleStage::RequestHeaders,
            matched: false,
            duration_us: 4,
            outcome: RuleOutcome::NotMatched,
        });

        let ctx = Arc::new(crate::server::HttpApiContext::new(state));
        let app = router(ctx);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{id}/trace"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["flow_id"], id);
        assert_eq!(json["events"][0]["rule_id"], "post-only");
        assert_eq!(json["events"][0]["matched"], false);
        assert_eq!(json["truncated"], false);

        let missing = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{}/trace", uuid::Uuid::new_v4()))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        assert_eq!(
            missing.status(),
            StatusCode::NOT_FOUND,
            "a missing flow must not look like an empty trace"
        );
    }
}
