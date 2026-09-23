use crate::capture::loop_detection::LoopDetector;
use crate::interceptor::HttpBody;
use crate::proxy::body_codec::process_body;
use chrono::Utc;
use cookie::Cookie as CookieCrate;
use data_encoding::BASE64;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use relay_core_api::flow::{
    BodyData, Cookie, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
    TransportProtocol, WebSocketLayer,
};
use relay_core_api::policy::ProxyPolicy;
use std::net::SocketAddr;
use url::Url;
use uuid::Uuid;

pub type HttpsClient = Client<HttpsConnector<HttpConnector>, HttpBody>;

#[derive(Clone, Debug)]
pub struct RequestMeta {
    pub method: String,
    pub url_str: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub cookies: Vec<Cookie>,
}

pub fn parse_request_meta<B>(req: &Request<B>, is_mitm: bool) -> RequestMeta {
    let method = req.method().to_string();
    let mut url_str = req.uri().to_string();

    // Attempt to construct absolute URL if relative
    if Url::parse(&url_str).is_err()
        && let Some(host) = req.headers().get("Host").and_then(|v| v.to_str().ok())
    {
        let scheme = if is_mitm { "https" } else { "http" };
        let new_url = format!("{}://{}{}", scheme, host, url_str);
        if Url::parse(&new_url).is_ok() {
            url_str = new_url;
        }
    }

    let version = format!("{:?}", req.version());

    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).to_string(),
            )
        })
        .collect();

    let query: Vec<(String, String)> = if let Ok(parsed_url) = Url::parse(&url_str) {
        parsed_url.query_pairs().into_owned().collect()
    } else {
        vec![]
    };

    let mut cookies = Vec::new();
    for cookie_header in req.headers().get_all(hyper::header::COOKIE) {
        let Ok(cookie_str) = cookie_header.to_str() else {
            continue;
        };
        for parsed in CookieCrate::split_parse(cookie_str).flatten() {
            cookies.push(Cookie {
                name: parsed.name().to_string(),
                value: parsed.value().to_string(),
                path: None,
                domain: None,
                expires: None,
                http_only: None,
                secure: None,
            });
        }
    }

    RequestMeta {
        method,
        url_str,
        version,
        headers,
        query,
        cookies,
    }
}

pub fn is_hop_by_hop(name: &str) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-authenticate")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("te")
        || name.eq_ignore_ascii_case("trailers")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("upgrade")
}

/// Join every `Cookie` field into one header, in the order they arrived.
///
/// HTTP/2 is allowed to send each cookie as its own field. Passing those through as separate
/// `Cookie` lines makes an HTTP/1 upstream that reads only the first line drop the rest, which
/// logs SSO sessions out. `Set-Cookie` is a different header and is left untouched.
pub fn coalesce_cookie_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(headers.len());
    let mut cookies: Vec<&str> = Vec::new();
    let mut slot = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("cookie") {
            if slot.is_none() {
                slot = Some(out.len());
                out.push(("Cookie".to_string(), String::new()));
            }
            if !value.is_empty() {
                cookies.push(value.as_str());
            }
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
    if let Some(index) = slot {
        if cookies.is_empty() {
            out.remove(index);
        } else {
            out[index].1 = cookies.join("; ");
        }
    }
    out
}

pub fn create_initial_flow(
    meta: RequestMeta,
    req_body: Option<BodyData>,
    client_addr: SocketAddr,
    is_mitm: bool,
    is_websocket: bool,
) -> Flow {
    let flow_id = Uuid::new_v4();
    let start_time = Utc::now();

    // Parsed once, because both the network info and the request need it.
    let url = Url::parse(&meta.url_str).unwrap_or_else(|_| Url::parse("http://unknown").unwrap());

    // What the request was aiming at. This is not the same thing as the peer address: a forward-proxy
    // request hands the URL to the HTTP client, so this engine never learns the resolved address, and
    // `server_ip`/`server_port` stay at their placeholder. Reporting the placeholder as a destination
    // is worse than reporting nothing — it looks like a real address and gets read as one.
    let server_host = url
        .host_str()
        .map(|host| match url.port_or_known_default() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        });

    let network_info = NetworkInfo {
        client_ip: client_addr.ip().to_string(),
        client_port: client_addr.port(),
        server_ip: "0.0.0.0".to_string(), // unknown: see `server_host`
        server_port: 0,                   // unknown: see `server_host`
        server_host,
        protocol: TransportProtocol::TCP,
        tls: is_mitm,
        tls_version: None,
        sni: None,
    };

    let http_request = HttpRequest {
        method: meta.method,
        url,
        version: meta.version,
        headers: meta.headers,
        cookies: meta.cookies,
        query: meta.query,
        body: req_body,
    };

    let mut flow = if is_websocket {
        Flow {
            id: flow_id,
            start_time,
            end_time: None,
            close_reason: None,
            network: network_info,
            layer: Layer::WebSocket(WebSocketLayer {
                handshake_request: http_request,
                handshake_response: HttpResponse {
                    status: 0,
                    status_text: "".to_string(),
                    version: "".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    trailers: vec![],
                    timing: relay_core_api::flow::ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec!["websocket".to_string()],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    } else {
        Flow {
            id: flow_id,
            start_time,
            end_time: None,
            close_reason: None,
            network: network_info,
            layer: Layer::Http(HttpLayer {
                request: http_request,
                response: None,
                error: None,
            }),
            tags: vec!["proxy".to_string()],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    };

    if is_mitm {
        flow.tags.push("mitm".to_string());
    }

    flow
}

pub fn create_error_response(status: StatusCode, message: impl Into<Bytes>) -> Response<HttpBody> {
    Response::builder()
        .status(status)
        .body(Full::new(message.into()).map_err(|e| e.into()).boxed())
        .unwrap_or_else(|_| {
            Response::new(
                Full::new(Bytes::from("Internal Error"))
                    .map_err(|e| e.into())
                    .boxed(),
            )
        })
}

/// Rebuild the client-facing response head from the Flow.
///
/// This is the response-direction counterpart of [`build_forward_request`]: it makes the Flow the
/// single source of truth for status and headers. Before this existed, the client received the
/// `res_parts` captured straight from the upstream, so any response-header or status mutation
/// (rules, scripts, manual intercept) changed the Flow and the UI but never the wire.
///
/// Framing caveat: `content-length` / `transfer-encoding` are carried over from `upstream_parts`
/// because the body returned here is still the upstream body. When a caller replaces the body it
/// must supply corrected framing itself (see [`crate::proxy::http_utils::build_response_from_flow_response`]).
pub fn build_client_response_head(
    flow: &Flow,
    upstream_parts: &hyper::http::response::Parts,
) -> hyper::http::response::Parts {
    // Start from the upstream parts so extensions and framings the body still depends on
    // (content-length, transfer-encoding, and any extension state) survive untouched.
    let mut parts = upstream_parts.clone();

    // The response must speak the client's version, not the upstream's: an HTTP/1.0 client answered
    // with an HTTP/1.1 response gets different framing and keep-alive expectations, and an H2 client
    // answered with HTTP/1.1 metadata misdescribes the exchange. `Flow` records what the client sent.
    if let Layer::Http(http) = &flow.layer {
        parts.version = parse_http_version(&http.request.version);
    }

    if let Some(response) = flow_response(flow) {
        parts.status = StatusCode::from_u16(response.status).unwrap_or(upstream_parts.status);

        // Drop the upstream's non-framing headers, then lay down the Flow's set. `insert`
        // semantics mean a mutation replaces rather than duplicates.
        let mut headers = hyper::HeaderMap::new();
        for (name, value) in upstream_parts.headers.iter() {
            if name == hyper::header::CONTENT_LENGTH || name == hyper::header::TRANSFER_ENCODING {
                headers.insert(name, value.clone());
            }
        }
        for (k, v) in &response.headers {
            // Framing belongs to the body, which is still the upstream stream in this path.
            if k.eq_ignore_ascii_case("content-length")
                || k.eq_ignore_ascii_case("transfer-encoding")
            {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        }
        parts.headers = headers;
    }

    parts
}

fn flow_response(flow: &Flow) -> Option<&HttpResponse> {
    match &flow.layer {
        Layer::Http(http) => http.response.as_ref(),
        _ => None,
    }
}

/// Materialize an interceptor-replaced request body as a wire body with correct framing.
///
/// The request direction has the same divergence the response direction had: `Action::SetRequestBody`
/// (and its transform variants) writes `flow.layer.http.request.body`, while the forwarded request
/// carried the original `current_body` stream — so the upstream saw the unmodified body.
///
/// Taking the body from the Flow makes the Flow authoritative for request bodies too. The cost is
/// losing the streaming property for requests whose body actually changed, so callers must only use
/// this when the Flow holds a replaced body; a pass-through body must keep streaming.
///
/// Framing is rebuilt rather than inherited from the client: `content-length` is recomputed from
/// the actual bytes, `transfer-encoding` is dropped because the replacement is a known-length
/// buffer, and `content-encoding` is dropped because the replacement is plain bytes (keeping the
/// client's `Content-Encoding` while sending an uncompressed body would be a lie).
pub fn build_request_body_from_flow(body_data: &BodyData) -> HttpBody {
    let bytes = body_data_to_bytes(Some(body_data));
    Full::new(bytes).map_err(|e| e.into()).boxed()
}

/// Rewrite request headers for a request whose body was replaced by the Flow.
pub fn reframe_request_headers_for_replaced_body(
    headers: &[(String, String)],
    body_len: usize,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| {
            !k.eq_ignore_ascii_case("content-length")
                && !k.eq_ignore_ascii_case("transfer-encoding")
                && !k.eq_ignore_ascii_case("content-encoding")
        })
        .cloned()
        .collect();

    out.push(("Content-Length".to_string(), body_len.to_string()));
    out
}

/// Byte length of the request body the Flow currently holds, if any.
///
/// Used to decide whether replacing the streaming body is warranted: an unchanged body must keep
/// streaming (roadmap §22 "无修改场景不无谓解压/缓冲").
pub fn request_body_from_flow_len(flow: &Flow) -> Option<usize> {
    match &flow.layer {
        Layer::Http(http) => http
            .request
            .body
            .as_ref()
            .map(|b| body_data_to_bytes(Some(b)).len()),
        _ => None,
    }
}

/// Materialize an interceptor-replaced **response** body as a wire body.
///
/// Symmetric to [`build_request_body_from_flow`]: `Action::SetResponseBody` writes
/// `flow.layer.http.response.body`, so the Flow is authoritative and the upstream stream must be
/// discarded rather than sent alongside it.
pub fn build_response_body_from_flow(body_data: &BodyData) -> HttpBody {
    let bytes = body_data_to_bytes(Some(body_data));
    Full::new(bytes).map_err(|e| e.into()).boxed()
}

/// Rewrite response headers for a response whose body was replaced by the Flow.
///
/// Recomputes `content-length` from the new bytes and drops `transfer-encoding` (the replacement is
/// a known-length buffer) and `content-encoding` (the replacement is plain bytes, so keeping the
/// upstream's encoding would misdescribe it).
pub fn reframe_response_headers_for_replaced_body(
    headers: &[(String, String)],
    body_len: usize,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| {
            !k.eq_ignore_ascii_case("content-length")
                && !k.eq_ignore_ascii_case("transfer-encoding")
                && !k.eq_ignore_ascii_case("content-encoding")
        })
        .cloned()
        .collect();

    out.push(("Content-Length".to_string(), body_len.to_string()));
    out
}

/// Byte length of the response body the Flow currently holds, if any.
pub fn response_body_from_flow_len(flow: &Flow) -> Option<usize> {
    match &flow.layer {
        Layer::Http(http) => http
            .response
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .map(|b| body_data_to_bytes(Some(b)).len()),
        _ => None,
    }
}

/// Build the client-facing response directly from a `relay-core-api` `HttpResponse`.
///
/// Used by the proxy wiring for terminal/mock/modified results, so it must produce the SAME
/// framing as [`build_client_response_from_flow`]: transport-level headers from a stale upstream
/// response must never leak onto a locally-constructed body, and `BodyData.encoding` must be
/// honoured.
pub fn build_response_from_flow_response(
    response: &HttpResponse,
) -> Result<Response<HttpBody>, String> {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK);
    let mut builder = Response::builder().status(status);

    let body_bytes = body_data_to_bytes(response.body.as_ref());

    // `content-encoding` is derived from (not declared by) the body: honour it only while the
    // bytes are still encoded, and drop it once we send a decoded payload.
    let keep_content_encoding = response
        .body
        .as_ref()
        .is_some_and(|b| b.encoding != "base64");

    for (k, v) in &response.headers {
        // Framing headers describe the ORIGINAL body being replaced; letting them through
        // produces a mis-framed response (hyper writes a caller-supplied Content-Length verbatim).
        if k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("transfer-encoding")
            || k.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        if k.eq_ignore_ascii_case("content-encoding") && !keep_content_encoding {
            continue;
        }

        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }

    builder
        .body(Full::new(body_bytes).map_err(|e| e.into()).boxed())
        .map_err(|e| format!("Failed to build response: {}", e))
}

pub(crate) fn body_data_to_bytes(body: Option<&BodyData>) -> Bytes {
    match body {
        Some(b) if b.encoding == "base64" => match BASE64.decode(b.content.as_bytes()) {
            Ok(bytes) => Bytes::from(bytes),
            // Not valid base64: send the raw content rather than silently dropping the body.
            Err(_) => Bytes::from(b.content.clone()),
        },
        Some(b) => Bytes::from(b.content.clone()),
        None => Bytes::new(),
    }
}

pub fn mock_to_response(mock: HttpResponse) -> Response<HttpBody> {
    match build_response_from_flow_response(&mock) {
        Ok(response) => response,
        Err(e) => {
            tracing::error!("Failed to build mock response: {}", e);
            create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build mock response",
            )
        }
    }
}

#[allow(clippy::result_large_err)]
pub fn build_forward_request(
    flow: &mut Flow,
    body: HttpBody,
    target_addr: Option<SocketAddr>,
    policy: &ProxyPolicy,
    loop_detector: &LoopDetector,
    body_replaced: bool,
) -> Result<Request<HttpBody>, Response<HttpBody>> {
    let current_req = if let Layer::Http(http) = &flow.layer {
        &http.request
    } else {
        return Err(create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid Flow Layer State",
        ));
    };

    // Carry the version the client actually spoke, so the forwarded request describes the real
    // ingress protocol rather than the builder's HTTP/1.1 default — an HTTP/1.0 client must not have
    // its request line rewritten to HTTP/1.1 upstream.
    //
    // Only for HTTP/1.x, though. An HTTP/2 ingress (h2c on the plaintext listener, or H2 behind a
    // TLS tunnel) says nothing about what the *upstream* connection will speak: that is decided by
    // the URL and ALPN, not by the client. Setting HTTP/2 unconditionally here made hyper refuse the
    // send outright — "Connection is HTTP/1, but request requires HTTP/2" → 502 — so every h2c
    // request failed to forward. The connection is the only thing that knows its own protocol, so
    // for HTTP/2 ingress the version is left to it.
    let ingress_version = parse_http_version(&current_req.version);
    let mut forward_req_builder = Request::builder().method(current_req.method.as_str());
    if ingress_version == hyper::Version::HTTP_10 || ingress_version == hyper::Version::HTTP_11 {
        forward_req_builder = forward_req_builder.version(ingress_version);
    }

    // Determine upstream URI
    let mut target_url = current_req.url.clone();

    // Transparent Proxy Routing Logic
    if policy.transparent_enabled
        && let Some(addr) = target_addr
    {
        flow.tags.push("transparent".to_string());

        // Update Flow Network Info
        flow.network.server_ip = addr.ip().to_string();
        flow.network.server_port = addr.port();

        // Loop Detection
        if loop_detector.would_loop(addr) {
            if let Layer::Http(http) = &mut flow.layer {
                http.error = Some("Loop Detected".to_string());
            }
            return Err(create_error_response(
                StatusCode::LOOP_DETECTED,
                "Loop Detected",
            ));
        }

        // Rewrite URI to use target IP
        if target_url.set_ip_host(addr.ip()).is_ok() {
            target_url.set_port(Some(addr.port())).ok();
        }

        // Update scheme if MITM
        if flow.network.tls && target_url.scheme() == "http" {
            target_url.set_scheme("https").ok();
        }
    }

    forward_req_builder = forward_req_builder.uri(target_url.as_str());

    // When the body was replaced, framing must describe the NEW bytes: the client's
    // content-length/transfer-encoding/content-encoding no longer apply.
    let headers: Vec<(String, String)> = if body_replaced {
        reframe_request_headers_for_replaced_body(
            &current_req.headers,
            request_body_from_flow_len(flow).unwrap_or(0),
        )
    } else {
        current_req.headers.clone()
    };
    // HTTP/2 may split one Cookie into several fields. An upstream that reads only the
    // first (common in SSO) then drops the session. RFC 6265 allows a single Cookie header.
    let headers = coalesce_cookie_headers(&headers);

    for (k, v) in &headers {
        // Filter out hop-by-hop headers to allow connection pooling
        if is_hop_by_hop(k) {
            continue;
        }

        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            forward_req_builder = forward_req_builder.header(name, val);
        }
    }

    match forward_req_builder.body(body) {
        Ok(req) => Ok(req),
        Err(e) => Err(create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to build forward request: {}", e),
        )),
    }
}

pub fn update_flow_with_response_headers(
    flow: &mut Flow,
    status: StatusCode,
    version: hyper::Version,
    headers: &hyper::HeaderMap,
) {
    let mut response_cookies = Vec::new();
    for (k, v) in headers.iter() {
        if k == hyper::header::SET_COOKIE
            && let Ok(v_str) = v.to_str()
            && let Ok(c) = CookieCrate::parse(v_str)
        {
            response_cookies.push(Cookie {
                name: c.name().to_string(),
                value: c.value().to_string(),
                path: c.path().map(|s| s.to_string()),
                domain: c.domain().map(|s| s.to_string()),
                expires: c.expires().map(|e| format!("{:?}", e)),
                http_only: c.http_only(),
                secure: c.secure(),
            });
        }
    }

    let resp_headers_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).to_string(),
            )
        })
        .collect();

    let http_response = HttpResponse {
        status: status.as_u16(),
        status_text: status.to_string(),
        version: format!("{:?}", version),
        headers: resp_headers_vec,
        cookies: response_cookies,
        body: None,
        trailers: vec![],
        timing: relay_core_api::flow::ResponseTiming {
            time_to_first_byte: None,
            time_to_last_byte: None,
            connect_time_ms: None,
            ssl_time_ms: None,
        },
    };

    match &mut flow.layer {
        Layer::Http(http) => {
            http.response = Some(http_response);
        }
        Layer::WebSocket(ws) => {
            ws.handshake_response = http_response;
        }
        _ => {}
    }
}

pub fn update_flow_with_response_body(flow: &mut Flow, body_bytes: Bytes) {
    let headers = match &flow.layer {
        Layer::Http(http) => http
            .response
            .as_ref()
            .map(|r| r.headers.clone())
            .unwrap_or_default(),
        Layer::WebSocket(ws) => ws.handshake_response.headers.clone(),
        _ => Vec::new(),
    };

    let (resp_encoding, resp_content) = process_body(&body_bytes, &headers);

    let body_data = BodyData {
        encoding: resp_encoding,
        content: resp_content,
        size: body_bytes.len() as u64,
        grpc: None,
    };

    match &mut flow.layer {
        Layer::Http(http) => {
            if let Some(resp) = &mut http.response {
                resp.body = Some(body_data);
            }
        }
        Layer::WebSocket(ws) => {
            ws.handshake_response.body = Some(body_data);
        }
        _ => {}
    }
}

pub fn update_flow_with_response(
    flow: &mut Flow,
    status: StatusCode,
    version: hyper::Version,
    headers: &hyper::HeaderMap,
    body_bytes: Bytes,
) {
    update_flow_with_response_headers(flow, status, version, headers);
    update_flow_with_response_body(flow, body_bytes);
}

pub fn build_client_response_from_flow(
    flow: &Flow,
    default_version: hyper::Version,
    strict_mode: bool,
) -> Result<Response<Full<Bytes>>, String> {
    if let Layer::Http(http) = &flow.layer {
        if let Some(response) = &http.response {
            let status = match StatusCode::from_u16(response.status) {
                Ok(s) => s,
                Err(_) => {
                    if strict_mode {
                        crate::metrics::inc_proxy_invalid_status();
                        return Err(format!("Invalid status code: {}", response.status));
                    }
                    StatusCode::OK
                }
            };

            let mut builder = Response::builder().status(status).version(default_version); // TODO: Parse version from flow string if needed

            for (k, v) in &response.headers {
                // Filter out transport-level headers that might conflict with the new body
                if k.eq_ignore_ascii_case("content-length")
                    || k.eq_ignore_ascii_case("transfer-encoding")
                    || k.eq_ignore_ascii_case("connection")
                {
                    continue;
                }

                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    builder = builder.header(name, val);
                } else if strict_mode {
                    return Err(format!("Invalid header: {}: {}", k, v));
                }
            }

            let body_bytes = if let Some(b) = &response.body {
                if b.encoding == "base64" {
                    match BASE64.decode(b.content.as_bytes()) {
                        Ok(bytes) => Bytes::from(bytes),
                        Err(_e) => {
                            // Fallback
                            Bytes::from(b.content.clone())
                        }
                    }
                } else {
                    Bytes::from(b.content.clone())
                }
            } else {
                Bytes::new()
            };

            builder
                .body(Full::new(body_bytes))
                .map_err(|e| format!("Failed to build response: {}", e))
        } else {
            Err("No response in flow".to_string())
        }
    } else {
        Err("Not HTTP layer".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_client_response_from_flow, build_forward_request, mock_to_response,
        parse_request_meta,
    };
    use crate::capture::loop_detection::LoopDetector;
    use chrono::Utc;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper::{Request, StatusCode, Version};
    use relay_core_api::flow::{
        BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol,
    };
    use relay_core_api::policy::ProxyPolicy;
    use std::collections::{BTreeSet, HashMap};
    use url::Url;
    use uuid::Uuid;

    /// HTTP/2 delivers one logical Cookie header as several fields. Parsing must see every
    /// field, and the upstream request must carry them as a single Cookie header (RFC 6265).
    #[test]
    fn every_cookie_field_is_parsed_and_forwarded_as_one_header() {
        let req = Request::builder()
            .uri("http://sso.example/login")
            .header("cookie", "sid=aaa")
            .header("cookie", "portal=bbb")
            .header("host", "sso.example")
            .body(())
            .expect("request");
        let meta = parse_request_meta(&req, false);
        let names: Vec<_> = meta
            .cookies
            .iter()
            .map(|cookie| cookie.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["sid", "portal"],
            "both cookie fields are parsed"
        );

        let mut flow = sample_flow_with_response(200);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.headers = meta.headers;
        }
        let body = Full::new(Bytes::new())
            .map_err(|error| error.into())
            .boxed();
        let forward = build_forward_request(
            &mut flow,
            body,
            None,
            &ProxyPolicy::default(),
            &LoopDetector::new(BTreeSet::new()),
            false,
        )
        .expect("forward");
        let cookies: Vec<_> = forward
            .headers()
            .get_all("cookie")
            .iter()
            .map(|value| value.to_str().expect("cookie text"))
            .collect();
        assert_eq!(
            cookies,
            vec!["sid=aaa; portal=bbb"],
            "the upstream sees one Cookie header"
        );
    }

    fn sample_response_with(
        status: u16,
        headers: Vec<(String, String)>,
        body: Option<BodyData>,
    ) -> HttpResponse {
        HttpResponse {
            status,
            status_text: "X".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            cookies: vec![],
            body,
            trailers: vec![],
            timing: ResponseTiming {
                time_to_first_byte: None,
                time_to_last_byte: None,
                connect_time_ms: None,
                ssl_time_ms: None,
            },
        }
    }

    /// A `BodySource::Base64` mock must put the DECODED bytes on the wire, and the framing
    /// headers must describe those bytes — not the upstream response they were lifted from.
    #[tokio::test]
    async fn test_mock_to_response_decodes_base64_and_reframes() {
        let decoded = b"hello".to_vec();
        let mock = sample_response_with(
            200,
            vec![
                (
                    "content-type".to_string(),
                    "application/octet-stream".to_string(),
                ),
                ("content-encoding".to_string(), "gzip".to_string()),
                ("transfer-encoding".to_string(), "chunked".to_string()),
                // Deliberately wrong: describes the upstream body, not the mock body.
                ("content-length".to_string(), "999".to_string()),
            ],
            Some(BodyData {
                encoding: "base64".to_string(),
                content: data_encoding::BASE64.encode(&decoded),
                size: decoded.len() as u64,
                grpc: None,
            }),
        );

        let resp = mock_to_response(mock);
        let (parts, body) = resp.into_parts();
        let bytes = body.collect().await.expect("collect body").to_bytes();

        assert_eq!(
            bytes.as_ref(),
            decoded.as_slice(),
            "base64 mock body must be decoded, not sent as base64 text"
        );
        assert_eq!(
            parts
                .headers
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            None,
            "stale upstream content-length must not be copied onto the mock response"
        );
        assert!(
            !parts.headers.contains_key("transfer-encoding"),
            "transfer-encoding must be stripped: the mock body is a known-length buffer"
        );
        assert!(
            !parts.headers.contains_key("content-encoding"),
            "content-encoding must be dropped when it no longer describes the body"
        );
        assert_eq!(
            parts
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream"),
            "unrelated headers must survive"
        );
    }

    /// A body-less mock must not advertise a compressed body it does not have.
    #[tokio::test]
    async fn test_mock_to_response_without_body_drops_content_encoding() {
        let mock = sample_response_with(
            204,
            vec![("content-encoding".to_string(), "br".to_string())],
            None,
        );

        let resp = mock_to_response(mock);
        let (parts, body) = resp.into_parts();
        let bytes = body.collect().await.expect("collect body").to_bytes();

        assert!(bytes.is_empty());
        assert!(!parts.headers.contains_key("content-encoding"));
    }

    fn sample_flow_with_response(status: u16) -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12345,
                server_ip: "1.1.1.1".to_string(),
                server_port: 80,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("http://example.com/a").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: Some(HttpResponse {
                    status,
                    status_text: "X".to_string(),
                    version: "HTTP/2.0".to_string(),
                    headers: vec![
                        ("X-Test".to_string(), "1".to_string()),
                        ("content-length".to_string(), "999".to_string()),
                        ("connection".to_string(), "keep-alive".to_string()),
                    ],
                    cookies: vec![],
                    body: None,
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: HashMap::new(),
            matched_rules: vec![],
        }
    }

    /// A forward-proxy flow must record what it was aiming at.
    ///
    /// The peer address is not knowable here — the URL is handed to the HTTP client — so the target is
    /// the only honest answer, and leaving `server_ip` at `0.0.0.0` with no target recorded is what
    /// made the UI show a placeholder as if it were an address.
    #[test]
    fn a_proxied_request_records_its_target() {
        let request = Request::builder()
            .method("GET")
            .uri("http://example.com/a/b")
            .body(())
            .expect("request");

        let meta = parse_request_meta(&request, false);
        let flow = super::create_initial_flow(
            meta,
            None,
            "127.0.0.1:12345".parse().expect("addr"),
            false,
            false,
        );

        assert_eq!(
            flow.network.server_host.as_deref(),
            Some("example.com:80"),
            "the target must be recorded, with the scheme's default port filled in"
        );
        assert_eq!(
            flow.network.server_ip, "0.0.0.0",
            "the peer address stays unknown rather than being invented"
        );

        // An explicit port is kept as written, and https has its own default.
        let request = Request::builder()
            .method("GET")
            .uri("https://example.com:8443/x")
            .body(())
            .expect("request");
        let meta = parse_request_meta(&request, true);
        let flow = super::create_initial_flow(
            meta,
            None,
            "127.0.0.1:12345".parse().expect("addr"),
            true,
            false,
        );
        assert_eq!(
            flow.network.server_host.as_deref(),
            Some("example.com:8443")
        );
    }

    #[test]
    fn test_parse_request_meta_relative_uri_uses_host_http() {
        let req = Request::builder()
            .uri("/api/v1?q=1")
            .header("Host", "example.com:8080")
            .body(())
            .expect("request");
        let meta = parse_request_meta(&req, false);
        assert_eq!(meta.url_str, "http://example.com:8080/api/v1?q=1");
        assert_eq!(meta.query, vec![("q".to_string(), "1".to_string())]);
    }

    #[test]
    fn test_parse_request_meta_relative_uri_uses_host_https_in_mitm() {
        let req = Request::builder()
            .uri("/secure")
            .header("Host", "secure.example.com")
            .body(())
            .expect("request");
        let meta = parse_request_meta(&req, true);
        assert_eq!(meta.url_str, "https://secure.example.com/secure");
    }

    #[test]
    fn test_build_client_response_from_flow_uses_default_version_currently() {
        let flow = sample_flow_with_response(201);
        let resp = build_client_response_from_flow(&flow, Version::HTTP_11, true)
            .expect("response should build");
        assert_eq!(resp.version(), Version::HTTP_11);
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers().get("x-test").and_then(|v| v.to_str().ok()),
            Some("1")
        );
        assert!(
            resp.headers().get("content-length").is_none(),
            "content-length should be stripped from forwarded mock response"
        );
        assert!(resp.headers().get("connection").is_none());
    }

    #[test]
    fn test_build_client_response_from_flow_invalid_status_strict_fails() {
        let flow = sample_flow_with_response(1000);
        let err = build_client_response_from_flow(&flow, Version::HTTP_11, true)
            .expect_err("strict mode should reject invalid status");
        assert!(err.contains("Invalid status code"));
    }

    #[tokio::test]
    async fn test_build_client_response_from_flow_invalid_status_non_strict_fallback_ok() {
        let mut flow = sample_flow_with_response(1000);
        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: "hello".to_string(),
                size: 5,
                grpc: None,
            });
        }

        let resp = build_client_response_from_flow(&flow, Version::HTTP_11, false)
            .expect("non-strict should fallback");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        assert_eq!(body.as_ref(), b"hello");
    }
}

/// Parse the `Flow`'s stored HTTP version string back into a `hyper::Version`.
///
/// `Flow.version` is populated from `format!("{:?}", req.version())`, so it reads `"HTTP/1.1"`,
/// `"HTTP/2.0"` and so on. Recovering the typed value matters because the outbound request used to be
/// built with the builder's default (HTTP/1.1) regardless of what the client actually spoke, which
/// hid the ingress version from the upstream and from any diagnostics reading the forwarded request.
pub fn parse_http_version(version: &str) -> hyper::Version {
    match version.trim().to_ascii_uppercase().as_str() {
        "HTTP/0.9" => hyper::Version::HTTP_09,
        "HTTP/1.0" => hyper::Version::HTTP_10,
        "HTTP/1.1" => hyper::Version::HTTP_11,
        "HTTP/2" | "HTTP/2.0" => hyper::Version::HTTP_2,
        "HTTP/3" | "HTTP/3.0" => hyper::Version::HTTP_3,
        // Unknown or empty: fall back to HTTP/1.1, which is what the builder would have used anyway.
        _ => hyper::Version::HTTP_11,
    }
}

#[cfg(test)]
mod version_parsing_tests {
    use super::parse_http_version;
    use hyper::Version;

    #[test]
    fn known_versions_round_trip_from_the_debug_format() {
        // These strings are what `format!("{:?}", Version::…)` produces, which is how Flow stores it.
        for (text, expected) in [
            ("HTTP/1.1", Version::HTTP_11),
            ("HTTP/1.0", Version::HTTP_10),
            ("HTTP/2.0", Version::HTTP_2),
            ("HTTP/3.0", Version::HTTP_3),
            ("HTTP/0.9", Version::HTTP_09),
        ] {
            assert_eq!(parse_http_version(text), expected, "parsing {text}");
        }
    }

    #[test]
    fn variant_spellings_are_accepted() {
        for text in ["http/2", "HTTP/2", "Http/2.0", " http/2.0 "] {
            assert_eq!(
                parse_http_version(text),
                Version::HTTP_2,
                "parsing {text:?}"
            );
        }
    }

    /// An unknown or empty version must not panic or invent a protocol.
    #[test]
    fn unknown_versions_fall_back_to_http_1_1() {
        for text in ["", " ", "gopher/1", "HTTP/9.9"] {
            assert_eq!(
                parse_http_version(text),
                Version::HTTP_11,
                "parsing {text:?} should fall back rather than guess"
            );
        }
    }
}
