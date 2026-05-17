use crate::flow::{Flow, Layer};
use serde_json::{json, Value};

/// Convert a Flow to a HAR 1.2 entry.
/// Used by both the HTTP API and MCP probe for consistent output.
pub fn flow_to_har_entry(flow: &Flow) -> Value {
    let (request, response) = match &flow.layer {
        Layer::Http(http) => (&http.request, http.response.as_ref()),
        Layer::WebSocket(ws) => (&ws.handshake_request, Some(&ws.handshake_response)),
        _ => return json!({ "request": {}, "response": {}, "timings": {} }),
    };

    let req_headers: Vec<Value> = request.headers.iter()
        .map(|(k, v)| json!({ "name": k, "value": v })).collect();
    let req_query: Vec<Value> = request.query.iter()
        .map(|(k, v)| json!({ "name": k, "value": v })).collect();
    let req_cookies: Vec<Value> = request.cookies.iter()
        .map(|c| json!({ "name": c.name, "value": c.value })).collect();

    let req_content_type = request.headers.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone());

    let mut req_json = json!({
        "method": request.method,
        "url": request.url.to_string(),
        "httpVersion": request.version,
        "headers": req_headers,
        "queryString": req_query,
        "cookies": req_cookies,
        "headersSize": har_headers_size(&request.headers, &request.method, request.url.path(), request.url.query(), &request.version),
        "bodySize": request.body.as_ref().map(|b| b.size).unwrap_or(0),
    });
    if let Some(body) = &request.body
        && !body.content.is_empty() {
            req_json["postData"] = json!({
                "mimeType": req_content_type.unwrap_or_default(),
                "text": body.content,
            });
    }

    let resp_headers: Vec<Value> = response.map(|r| r.headers.iter()
        .map(|(k, v)| json!({ "name": k, "value": v })).collect()).unwrap_or_default();
    let resp_cookies: Vec<Value> = response.map(|r| r.cookies.iter()
        .map(|c| json!({ "name": c.name, "value": c.value })).collect()).unwrap_or_default();

    let mut resp_json = json!({});
    let mut timings = json!({ "send": 0, "wait": 0, "receive": 0, "connect": -1, "ssl": -1, "dns": -1, "blocked": -1 });

    if let Some(resp) = response {
        let resp_content_type = resp.headers.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let redirect_url = resp.headers.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        resp_json = json!({
            "status": resp.status,
            "statusText": resp.status_text,
            "httpVersion": resp.version,
            "headers": resp_headers,
            "cookies": resp_cookies,
            "content": {
                "size": resp.body.as_ref().map(|b| b.size).unwrap_or(0),
                "mimeType": resp_content_type,
                "text": resp.body.as_ref().map(|b| b.content.as_str()).unwrap_or(""),
            },
            "redirectURL": redirect_url,
            "headersSize": har_headers_size(&resp.headers, "", "", None, &resp.version),
            "bodySize": resp.body.as_ref().map(|b| b.size).unwrap_or(0),
        });

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
        "request": req_json,
        "response": resp_json,
        "timings": timings,
        "cache": {},
        "_relaycore": {
            "flow_id": flow.id.to_string(),
            "client_ip": flow.network.client_ip,
            "server_ip": flow.network.server_ip,
            "tags": flow.tags,
        }
    })
}

fn har_headers_size(headers: &[(String, String)], method: &str, path: &str, query: Option<&str>, version: &str) -> u64 {
    let start_line = if method.is_empty() {
        version.len() + 1 + 3 + 1 + 3 + 2
    } else {
        let q = query.map(|q| q.len() + 1).unwrap_or(0);
        method.len() + 1 + path.len() + q + 1 + version.len() + 2
    };
    let headers_bytes: usize = headers.iter().map(|(k, v)| k.len() + 2 + v.len() + 2).sum();
    (start_line + headers_bytes + 2) as u64
}
