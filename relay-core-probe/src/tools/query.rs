use super::ToolError;
use super::{ToolOutcome, ToolSpec, ack_output_schema, ok_ack, ok_json, require_str, tool};
use crate::server::ProbeContext;
use relay_core_api::flow::Layer;
use relay_core_api::har::flow_to_har_entry;
use relay_core_api::modification::FlowQuery;
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn search_flows_schema() -> Tool {
    tool(
        ToolSpec::read_only(
        "search_flows",
        "Search captured HTTP/WebSocket flows with optional filters. \
         Returns flow summaries sorted by most recent first.",
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
        .with_output(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "description": "Number of summaries returned" },
                "flows": { "type": "array", "items": { "type": "object" }, "description": "Flow summaries, most recent first" }
            },
            "required": ["count", "flows"],
        })),
    )
}

pub fn get_flow_schema() -> Tool {
    tool(ToolSpec::read_only(
        "get_flow",
        "Get full details of a specific flow by ID, including headers, body, timing, and tags.",
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Flow UUID" }
            }
        }),
    ))
}

pub fn get_metrics_schema() -> Tool {
    tool(ToolSpec::read_only(
        "get_metrics",
        "Get proxy runtime metrics: total flows, memory usage, intercepts pending, rule errors.",
        json!({ "type": "object", "properties": {} }),
    ))
}

pub async fn search_flows(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let query = FlowQuery {
        host: args.get("host").and_then(Value::as_str).map(str::to_string),
        path_contains: args
            .get("path_contains")
            .and_then(Value::as_str)
            .map(str::to_string),
        method: args
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string),
        status_min: args
            .get("status_min")
            .and_then(Value::as_u64)
            .map(|v| v as u16),
        status_max: args
            .get("status_max")
            .and_then(Value::as_u64)
            .map(|v| v as u16),
        has_error: args.get("has_error").and_then(Value::as_bool),
        is_websocket: args.get("is_websocket").and_then(Value::as_bool),
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
        offset: args
            .get("offset")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
    };
    let summaries = ctx.flows.search_flows(query).await;

    // An empty list has two very different meanings, and returning it for both is how an agent
    // concludes "the app sent no requests" when in fact nothing was being captured at all. With no
    // proxy running and no history to show, say so instead.
    if summaries.is_empty() && !proxy_is_running(ctx) {
        return Err(ToolError::proxy_not_running(
            "No flows matched, and none have ever been captured.",
        ));
    }

    ok_json(&json!({ "count": summaries.len(), "flows": summaries }))
}

/// Whether the host this probe runs in reports an active proxy.
///
/// Absent proxy control (a host that embedded the probe without passing a controller) is not
/// treated as "not running": such a host may well be capturing traffic, and a false error would be
/// worse than an empty list.
fn proxy_is_running(ctx: &Arc<ProbeContext>) -> bool {
    match &ctx.proxy {
        Some(proxy) => proxy.proxy_lifecycle().is_active(),
        None => true,
    }
}

pub async fn get_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let id = require_str(&args, "id")?;
    match ctx.flows.get_flow(&id).await {
        Some(flow) => ok_json(&flow),
        None => Err(format!("Flow not found: {id}").into()),
    }
}

pub async fn get_metrics(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    let m = ctx.status.get_metrics().await;
    ok_json(&m)
}

pub fn replay_flow_schema() -> Tool {
    tool(
        ToolSpec::write(
        "replay_flow",
        "Re-send a captured HTTP request through the running proxy and return the new response. \
         The replay is captured as a new flow, and the current rules and scripts apply. \
         Fails when the proxy is not running. Only works for HTTP flows.",
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Flow UUID to replay" },
                "accept_invalid_certs": {
                    "type": "boolean",
                    "description": "Skip TLS certificate verification (insecure, dev only). Default: false."
                }
            }
        }),
        false,
        false,
    )
        .open_world(),
    )
}

pub async fn replay_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let id = require_str(&args, "id")?.to_string();
    let accept_invalid_certs = args
        .get("accept_invalid_certs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flow = ctx
        .flows
        .get_flow(&id)
        .await
        .ok_or(format!("Flow not found: {}", id))?;

    let (method, url, headers, body) = match &flow.layer {
        Layer::Http(http) => (
            http.request.method.clone(),
            http.request.url.to_string(),
            http.request.headers.clone(),
            http.request.body.clone(),
        ),
        _ => return Err("Replay only supports HTTP flows".to_string().into()),
    };

    let port = relay_core_http::replay::require_running_proxy(&ctx.status.status_snapshot())
        .map_err(|error| ToolError::unavailable("proxy_not_running", error))?;
    let ca_pem = ctx.status.ca_cert_pem();
    let response = relay_core_http::replay::send_captured_request(
        &method,
        &url,
        &headers,
        body.as_ref(),
        port,
        accept_invalid_certs,
        ca_pem.as_deref(),
    )
    .await?;

    ok_json(&json!({
        "status": response.status,
        "url": response.url,
        "headers": response.headers,
        "body": response.body,
    }))
}

pub fn export_har_schema() -> Tool {
    tool(ToolSpec::read_only(
        "export_har",
        "Export one or more flows as HAR (HTTP Archive) 1.2 format. \
         Specify an ID for a single flow, or use host/path_contains/limit for batch.",
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Single flow UUID to export" },
                "host": { "type": "string", "description": "Filter by hostname (batch mode)" },
                "path_contains": { "type": "string", "description": "Filter by URL path (batch mode)" },
                "limit": { "type": "integer", "description": "Max results (batch mode, default 50)" }
            }
        }),
    ))
}

pub async fn export_har(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let entries = if let Some(id) = args.get("id").and_then(Value::as_str) {
        let flow = ctx
            .flows
            .get_flow(id)
            .await
            .ok_or(format!("Flow not found: {}", id))?;
        vec![flow_to_har_entry(&flow)]
    } else {
        let query = FlowQuery {
            host: args.get("host").and_then(Value::as_str).map(str::to_string),
            path_contains: args
                .get("path_contains")
                .and_then(Value::as_str)
                .map(str::to_string),
            limit: args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .or(Some(50)),
            ..Default::default()
        };
        let summaries = ctx.flows.search_flows(query).await;
        let mut entries = Vec::new();
        for s in summaries {
            if let Some(flow) = ctx.flows.get_flow(&s.id).await {
                entries.push(flow_to_har_entry(&flow));
            }
        }
        entries
    };

    let har = json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "RelayCore", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries
        }
    });

    ok_json(&har)
}

pub fn clear_flows_schema() -> Tool {
    tool(
        ToolSpec::write(
            "clear_flows",
            "Delete captured flows and their summaries from memory and the database. \
             Rules, policy, and the audit log stay. Calling it again when history is already \
             empty is a no-op. Retention (default 5000 flows or 7 days) is what keeps a \
             long-running proxy bounded between clears.",
            json!({ "type": "object", "properties": {} }),
            true,
            true,
        )
        .with_output(ack_output_schema(json!({
            "flows": { "type": "integer", "description": "Flow rows deleted" },
            "flow_summaries": { "type": "integer", "description": "Summary rows deleted" }
        }))),
    )
}

pub async fn clear_flows(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    let (flows, summaries) = ctx.flows.clear_captured_flows().await?;
    ok_ack(
        format!("Cleared {flows} flows and {summaries} summaries."),
        json!({ "flows": flows, "flow_summaries": summaries }),
    )
}
