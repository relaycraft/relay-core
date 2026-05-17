use std::sync::Arc;
use crate::server::ProbeContext;
use relay_core_api::flow::Layer;
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

pub async fn search_flows(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
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
    let summaries = ctx.flows.search_flows(query).await;
    ok_json(&summaries)
}

pub async fn get_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let id = require_str(&args, "id")?;
    match ctx.flows.get_flow(&id).await {
        Some(flow) => ok_json(&flow),
        None => Err(format!("Flow not found: {}", id)),
    }
}

pub async fn get_metrics(ctx: &Arc<ProbeContext>) -> Result<Vec<Content>, String> {
    let m = ctx.status.get_metrics().await;
    ok_json(&m)
}

pub fn replay_flow_schema() -> Tool {
    make_tool(
        "replay_flow",
        "Re-send a captured HTTP request and return the new response. \
         Only works for HTTP flows.",
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Flow UUID to replay" }
            }
        }),
    )
}

pub async fn replay_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let id = require_str(&args, "id")?.to_string();
    let flow = ctx.flows.get_flow(&id).await
        .ok_or(format!("Flow not found: {}", id))?;

    let (method, url, headers, body) = match &flow.layer {
        Layer::Http(http) => {
            let headers: Vec<(String, String)> = http.request.headers.iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("host") && !k.eq_ignore_ascii_case("connection"))
                .cloned()
                .collect();
            (http.request.method.clone(), http.request.url.to_string(), headers, http.request.body.clone())
        }
        _ => return Err("Replay only supports HTTP flows".to_string()),
    };

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client.request(
        method.parse::<reqwest::Method>().map_err(|e| format!("Invalid method: {}", e))?,
        &url,
    );
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = &body {
        req = req.body(b.content.clone());
    }

    let resp = req.send().await.map_err(|e| format!("Replay failed: {}", e))?;
    let status = resp.status().as_u16();
    let resp_headers: Vec<(String, String)> = resp.headers().iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let resp_body = resp.text().await.map_err(|e| e.to_string())?;

    Ok(vec![Content::text(serde_json::to_string_pretty(&json!({
        "status": status,
        "url": url,
        "headers": resp_headers,
        "body": resp_body,
    })).map_err(|e| e.to_string())?)])
}

pub fn export_har_schema() -> Tool {
    make_tool(
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
    )
}

pub async fn export_har(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let entries = if let Some(id) = args.get("id").and_then(Value::as_str) {
        let flow = ctx.flows.get_flow(id).await
            .ok_or(format!("Flow not found: {}", id))?;
        vec![flow_to_har_entry(&flow)]
    } else {
        let query = FlowQuery {
            host: args.get("host").and_then(Value::as_str).map(str::to_string),
            path_contains: args.get("path_contains").and_then(Value::as_str).map(str::to_string),
            limit: args.get("limit").and_then(Value::as_u64).map(|v| v as usize).or(Some(50)),
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

fn flow_to_har_entry(flow: &relay_core_api::flow::Flow) -> Value {
    let (request, response) = match &flow.layer {
        Layer::Http(http) => (&http.request, http.response.as_ref()),
        Layer::WebSocket(ws) => (&ws.handshake_request, Some(&ws.handshake_response)),
        _ => return json!({ "request": {}, "response": {}, "timings": {} }),
    };

    let req_headers: Vec<Value> = request.headers.iter()
        .map(|(k, v)| json!({ "name": k, "value": v })).collect();
    let resp_headers: Vec<Value> = response.map(|r| r.headers.iter()
        .map(|(k, v)| json!({ "name": k, "value": v })).collect()).unwrap_or_default();

    let mut timings = json!({ "send": 0, "wait": 0, "receive": 0, "connect": -1, "ssl": -1 });
    if let Some(resp) = response {
        timings["wait"] = json!(resp.timing.time_to_first_byte.unwrap_or(0));
        let ttlbs = resp.timing.time_to_last_byte.unwrap_or(0);
        let wait = resp.timing.time_to_first_byte.unwrap_or(0);
        timings["receive"] = json!(ttlbs.saturating_sub(wait));
        if let Some(c) = resp.timing.connect_time_ms { timings["connect"] = json!(c); }
        if let Some(s) = resp.timing.ssl_time_ms { timings["ssl"] = json!(s); }
    }

    let total_time = response.map(|r| r.timing.time_to_last_byte.unwrap_or(0))
        .unwrap_or(0);

    json!({
        "startedDateTime": flow.start_time.to_rfc3339(),
        "time": total_time,
        "request": {
            "method": request.method,
            "url": request.url.to_string(),
            "httpVersion": request.version,
            "headers": req_headers,
            "bodySize": request.body.as_ref().map(|b| b.size).unwrap_or(0),
            "headersSize": 0,
        },
        "response": {
            "status": response.map(|r| r.status).unwrap_or(0),
            "statusText": response.map(|r| r.status_text.as_str()).unwrap_or(""),
            "headers": resp_headers,
            "content": {
                "size": response.and_then(|r| r.body.as_ref().map(|b| b.size)).unwrap_or(0),
                "mimeType": "",
            },
            "redirectURL": response.map(|r| r.headers.iter()
                .find(|(k,_)| k.eq_ignore_ascii_case("location"))
                .map(|(_,v)| v.clone()).unwrap_or_default()).unwrap_or_default(),
            "bodySize": response.and_then(|r| r.body.as_ref().map(|b| b.size)).unwrap_or(0),
            "headersSize": 0,
        },
        "timings": timings,
        "cache": {},
    })
}
