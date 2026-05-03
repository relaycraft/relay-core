use std::sync::Arc;
use relay_core_runtime::CoreState;
use relay_core_api::modification::FlowQuery;
use rmcp::model::{Content, Tool};
use serde_json::{json, Value};
use super::{make_tool, require_str, ok_json};

pub fn search_flows_schema() -> Tool {
    make_tool(
        "search_flows",
        "Search captured HTTP/WebSocket flows with optional filters. \
         Returns a list of flow summaries sorted by most recent first.",
        json!({
            "type": "object",
            "properties": {
                "host":          { "type": "string", "description": "Filter by hostname (substring match)" },
                "path_contains": { "type": "string", "description": "Filter by URL path (substring match)" },
                "method":        { "type": "string", "description": "HTTP method filter (e.g. GET, POST)" },
                "status_min":    { "type": "integer", "description": "Minimum HTTP status code (inclusive)" },
                "status_max":    { "type": "integer", "description": "Maximum HTTP status code (inclusive)" },
                "has_error":     { "type": "boolean", "description": "If true, only return flows with 5xx or error tags" },
                "is_websocket":  { "type": "boolean", "description": "If true, only return WebSocket flows" },
                "limit":         { "type": "integer", "description": "Max results to return (default 50, max 200)" },
                "offset":        { "type": "integer", "description": "Result offset for pagination (default 0)" }
            }
        }),
    )
}

pub fn get_flow_schema() -> Tool {
    make_tool(
        "get_flow",
        "Get full details of a specific flow by ID, including headers, body, timing, and tags.",
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Flow UUID" }
            }
        }),
    )
}

pub fn get_metrics_schema() -> Tool {
    make_tool(
        "get_metrics",
        "Get proxy runtime metrics: total flows, memory usage, intercepts pending, rule errors.",
        json!({ "type": "object", "properties": {} }),
    )
}

pub async fn search_flows(state: &Arc<CoreState>, args: Value) -> Result<Vec<Content>, String> {
    let query = FlowQuery {
        host:          args.get("host").and_then(Value::as_str).map(str::to_string),
        path_contains: args.get("path_contains").and_then(Value::as_str).map(str::to_string),
        method:        args.get("method").and_then(Value::as_str).map(str::to_string),
        status_min:    args.get("status_min").and_then(Value::as_u64).map(|v| v as u16),
        status_max:    args.get("status_max").and_then(Value::as_u64).map(|v| v as u16),
        has_error:     args.get("has_error").and_then(Value::as_bool),
        is_websocket:  args.get("is_websocket").and_then(Value::as_bool),
        limit:         args.get("limit").and_then(Value::as_u64).map(|v| v as usize),
        offset:        args.get("offset").and_then(Value::as_u64).map(|v| v as usize),
    };
    let summaries = state.search_flows(query).await;
    ok_json(&summaries)
}

pub async fn get_flow(state: &Arc<CoreState>, args: Value) -> Result<Vec<Content>, String> {
    let id = require_str(&args, "id")?;
    match state.get_flow(id.clone()).await {
        Some(flow) => ok_json(&flow),
        None => Err(format!("Flow not found: {}", id)),
    }
}

pub async fn get_metrics(state: &Arc<CoreState>) -> Result<Vec<Content>, String> {
    let m = state.get_metrics().await;
    ok_json(&m)
}
