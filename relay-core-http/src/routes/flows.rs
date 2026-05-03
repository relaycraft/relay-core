use std::sync::Arc;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use relay_core_api::modification::FlowQuery;
use relay_core_api::modification::FlowSummary;
use relay_core_runtime::CoreState;
use serde::{Deserialize, Serialize};

pub fn router(state: Arc<CoreState>) -> Router {
    Router::new()
        .route("/api/v1/flows", get(search_flows))
        .route("/api/v1/flows/{id}", get(get_flow))
        .with_state(state)
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
    State(state): State<Arc<CoreState>>,
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
    let items = state.search_flows(query).await;
    Json(FlowSearchResponse {
        returned: items.len(),
        items,
        limit,
        offset,
    })
}

async fn get_flow(
    State(state): State<Arc<CoreState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.get_flow(id).await {
        Some(flow) => Ok(Json(serde_json::to_value(&flow).unwrap_or_default())),
        None => Err(StatusCode::NOT_FOUND),
    }
}
