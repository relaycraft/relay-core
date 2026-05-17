use std::sync::Arc;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use relay_core_api::flow::Layer;
use relay_core_api::modification::FlowQuery;
use relay_core_api::modification::FlowSummary;
use relay_core_api::har::flow_to_har_entry;
use crate::server::HttpApiContext;
use serde::{Deserialize, Serialize};

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

/// POST /api/v1/flows/{id}/replay
///
/// Re-sends the original HTTP request from a captured flow and returns the new response.
/// Only works for HTTP flows (not WebSocket, TCP, UDP).
async fn replay_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let flow = ctx.flows.get_flow(&id).await
        .ok_or((StatusCode::NOT_FOUND, format!("Flow {} not found", id)))?;

    let (method, url, headers, body) = match &flow.layer {
        Layer::Http(http) => {
            let method = http.request.method.clone();
            let url = http.request.url.to_string();
            let headers: Vec<(String, String)> = http.request.headers.iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("host") && !k.eq_ignore_ascii_case("connection"))
                .cloned()
                .collect();
            let body = http.request.body.clone();
            (method, url, headers, body)
        }
        _ => {
            return Err((StatusCode::BAD_REQUEST, "Replay only supports HTTP flows".to_string()));
        }
    };

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut req = client.request(
        method.parse::<reqwest::Method>().map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid method: {}", e)))?,
        &url,
    );

    for (k, v) in &headers {
        req = req.header(k, v);
    }

    if let Some(body_data) = &body {
        req = req.body(body_data.content.clone());
    }

    let resp = req.send().await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Replay request failed: {}", e)))?;

    let status = resp.status().as_u16();
    let resp_headers: Vec<(String, String)> = resp.headers().iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let resp_body = resp.text().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": status,
        "url": url,
        "headers": resp_headers,
        "body": resp_body,
    })))
}

/// GET /api/v1/flows/{id}/har — export a single flow as HAR entry
async fn get_flow_har(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let flow = ctx.flows.get_flow(&id).await
        .ok_or(StatusCode::NOT_FOUND)?;
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
