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

async fn get_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match ctx.flows.get_flow(&id).await {
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
