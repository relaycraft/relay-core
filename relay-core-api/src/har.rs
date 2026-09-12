use crate::flow::{Flow, Layer};
use serde_json::{Value, json};

/// Convert a Flow to a HAR 1.2 entry.
/// Used by both the HTTP API and MCP probe for consistent output.
pub fn flow_to_har_entry(flow: &Flow) -> Value {
    let (request, response) = match &flow.layer {
        Layer::Http(http) => (&http.request, http.response.as_ref()),
        Layer::WebSocket(ws) => (&ws.handshake_request, Some(&ws.handshake_response)),
        // Not an HTTP exchange: there is nothing to measure, so report no timings rather than a set
        // of zeroes that read as measurements.
        _ => {
            return json!({
                "startedDateTime": flow.start_time.to_rfc3339(),
                "time": -1,
                "request": {},
                "response": {},
                "timings": {},
                "cache": {},
            });
        }
    };

    let req_headers: Vec<Value> = request
        .headers
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();
    let req_query: Vec<Value> = request
        .query
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();
    let req_cookies: Vec<Value> = request
        .cookies
        .iter()
        .map(|c| json!({ "name": c.name, "value": c.value }))
        .collect();

    let req_content_type = request
        .headers
        .iter()
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
        && !body.content.is_empty()
    {
        req_json["postData"] = json!({
            "mimeType": req_content_type.unwrap_or_default(),
            "text": body.content,
        });
    }

    let resp_headers: Vec<Value> = response
        .map(|r| {
            r.headers
                .iter()
                .map(|(k, v)| json!({ "name": k, "value": v }))
                .collect()
        })
        .unwrap_or_default();
    let resp_cookies: Vec<Value> = response
        .map(|r| {
            r.cookies
                .iter()
                .map(|c| json!({ "name": c.name, "value": c.value }))
                .collect()
        })
        .unwrap_or_default();

    let mut resp_json = json!({});
    // HAR says `-1` means the phase was not measured and `0` means it was measured as instantaneous.
    // Every phase therefore starts as not-measured; only real observations replace that.
    let mut timings = json!({
        "send": -1, "wait": -1, "receive": -1,
        "connect": -1, "ssl": -1, "dns": -1, "blocked": -1,
    });

    if let Some(resp) = response {
        let resp_content_type = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let redirect_url = resp
            .headers
            .iter()
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

        if let Some(ttfb) = resp.timing.time_to_first_byte {
            timings["wait"] = json!(ttfb);
        }
        // `receive` is the remainder of the exchange, so it is only meaningful when both ends were
        // observed; otherwise it stays unknown rather than being derived from a fabricated zero.
        if let (Some(ttfb), Some(ttlb)) = (
            resp.timing.time_to_first_byte,
            resp.timing.time_to_last_byte,
        ) {
            timings["receive"] = json!(ttlb.saturating_sub(ttfb));
        }
        if let Some(c) = resp.timing.connect_time_ms {
            timings["connect"] = json!(c);
        }
        if let Some(s) = resp.timing.ssl_time_ms {
            timings["ssl"] = json!(s);
        }
    }

    // The total is the exchange's own duration when it was observed, and unknown otherwise.
    let total_time = response
        .and_then(|r| r.timing.time_to_last_byte)
        .map(|ms| ms as i64)
        .unwrap_or(-1);

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

fn har_headers_size(
    headers: &[(String, String)],
    method: &str,
    path: &str,
    query: Option<&str>,
    version: &str,
) -> u64 {
    let start_line = if method.is_empty() {
        version.len() + 1 + 3 + 1 + 3 + 2
    } else {
        let q = query.map(|q| q.len() + 1).unwrap_or(0);
        method.len() + 1 + path.len() + q + 1 + version.len() + 2
    };
    let headers_bytes: usize = headers.iter().map(|(k, v)| k.len() + 2 + v.len() + 2).sum();
    (start_line + headers_bytes + 2) as u64
}
