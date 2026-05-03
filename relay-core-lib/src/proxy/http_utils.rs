use std::net::SocketAddr;
use hyper::{Response, Request, StatusCode};
use hyper::body::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use http_body_util::{Full, BodyExt};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_rustls::HttpsConnector;
use relay_core_api::flow::{
    BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, TransportProtocol,
    WebSocketLayer, Cookie,
};
use relay_core_api::policy::ProxyPolicy;
use crate::capture::loop_detection::LoopDetector;
use uuid::Uuid;
use chrono::Utc;
use url::Url;
use cookie::Cookie as CookieCrate;
use data_encoding::BASE64;
use crate::proxy::body_codec::process_body;
use crate::interceptor::HttpBody;

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
    if Url::parse(&url_str).is_err() {
        if let Some(host) = req.headers().get("Host").and_then(|v| v.to_str().ok()) {
             let scheme = if is_mitm { "https" } else { "http" };
             let new_url = format!("{}://{}{}", scheme, host, url_str);
             if Url::parse(&new_url).is_ok() {
                 url_str = new_url;
             }
        }
    }

    let version = format!("{:?}", req.version());
    
    let headers: Vec<(String, String)> = req.headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let query: Vec<(String, String)> = if let Ok(parsed_url) = Url::parse(&url_str) {
        parsed_url.query_pairs().into_owned().collect()
    } else {
        vec![]
    };

    let mut cookies = Vec::new();
    if let Some(cookie_header) = req.headers().get(hyper::header::COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
             for c in CookieCrate::split_parse(cookie_str).flatten() {
                 cookies.push(Cookie {
                     name: c.name().to_string(),
                     value: c.value().to_string(),
                     path: None,
                     domain: None,
                     expires: None,
                     http_only: None,
                     secure: None,
                 });
             }
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
        || name.eq_ignore_ascii_case("content-length")
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
    
    let network_info = NetworkInfo {
        client_ip: client_addr.ip().to_string(),
        client_port: client_addr.port(),
        server_ip: "0.0.0.0".to_string(), // Placeholder
        server_port: 0,                   // Placeholder
        protocol: TransportProtocol::TCP,
        tls: is_mitm,
        tls_version: None,
        sni: None,
    };

    let http_request = HttpRequest {
        method: meta.method,
        url: Url::parse(&meta.url_str).unwrap_or_else(|_| Url::parse("http://unknown").unwrap()),
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
                    timing: relay_core_api::flow::ResponseTiming { time_to_first_byte: None, time_to_last_byte: None },
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec!["websocket".to_string()],
            meta: std::collections::HashMap::new(),
        }
    } else {
        Flow {
            id: flow_id,
            start_time,
            end_time: None,
            network: network_info,
            layer: Layer::Http(HttpLayer {
                request: http_request,
                response: None,
                error: None,
            }),
            tags: vec!["proxy".to_string()],
            meta: std::collections::HashMap::new(),
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
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from("Internal Error")).map_err(|e| e.into()).boxed()))
}

pub fn mock_to_response(mock: HttpResponse) -> Response<HttpBody> {
     let mut builder = Response::builder()
        .status(StatusCode::from_u16(mock.status).unwrap_or(StatusCode::OK));
        
    for (k, v) in mock.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            builder = builder.header(name, val);
        }
    }
    
    let body = if let Some(b) = mock.body {
        Bytes::from(b.content)
    } else {
        Bytes::new()
    };
    
    builder.body(Full::new(body).map_err(|e| e.into()).boxed()).unwrap_or_else(|_| create_error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to build mock response"))
}

#[allow(clippy::result_large_err)]
pub fn build_forward_request(
    flow: &mut Flow,
    body: HttpBody,
    req_parts: &http::request::Parts,
    target_addr: Option<SocketAddr>,
    policy: &ProxyPolicy,
    loop_detector: &LoopDetector,
) -> Result<Request<HttpBody>, Response<HttpBody>> {
    let current_req = if let Layer::Http(http) = &flow.layer {
        &http.request
    } else {
        return Err(create_error_response(StatusCode::INTERNAL_SERVER_ERROR, "Invalid Flow Layer State"));
    };

    let mut forward_req_builder = Request::builder()
        .method(current_req.method.as_str())
        .version(req_parts.version);

    // Determine upstream URI
    let mut target_url = current_req.url.clone();
    
    // Transparent Proxy Routing Logic
    if policy.transparent_enabled {
        if let Some(addr) = target_addr {
            flow.tags.push("transparent".to_string());
            
            // Update Flow Network Info
            flow.network.server_ip = addr.ip().to_string();
            flow.network.server_port = addr.port();

            // Loop Detection
            if loop_detector.would_loop(addr) {
                if let Layer::Http(http) = &mut flow.layer {
                    http.error = Some("Loop Detected".to_string());
                }
                return Err(create_error_response(StatusCode::LOOP_DETECTED, "Loop Detected"));
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
    }

    forward_req_builder = forward_req_builder.uri(target_url.as_str());

    for (k, v) in &current_req.headers {
        // Filter out hop-by-hop headers to allow connection pooling
        if is_hop_by_hop(k) {
            continue;
        }

        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            forward_req_builder = forward_req_builder.header(name, val);
        }
    }

    match forward_req_builder.body(body) {
        Ok(req) => Ok(req),
        Err(e) => Err(create_error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to build forward request: {}", e))),
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
        if k == hyper::header::SET_COOKIE {
            if let Ok(v_str) = v.to_str() {
                if let Ok(c) = CookieCrate::parse(v_str) {
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
        }
    }

    let resp_headers_vec: Vec<(String, String)> = headers.iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let http_response = HttpResponse {
        status: status.as_u16(),
        status_text: status.to_string(),
        version: format!("{:?}", version),
        headers: resp_headers_vec,
        cookies: response_cookies,
        body: None,
        timing: relay_core_api::flow::ResponseTiming {
            time_to_first_byte: None,
            time_to_last_byte: None,
        },
    };

    match &mut flow.layer {
        Layer::Http(http) => {
            http.response = Some(http_response);
        },
        Layer::WebSocket(ws) => {
            ws.handshake_response = http_response;
        },
        _ => {}
    }
}

pub fn update_flow_with_response_body(
    flow: &mut Flow,
    body_bytes: Bytes,
) {
    let headers = match &flow.layer {
        Layer::Http(http) => http.response.as_ref().map(|r| r.headers.clone()).unwrap_or_default(),
        Layer::WebSocket(ws) => ws.handshake_response.headers.clone(),
        _ => Vec::new(),
    };

    let (resp_encoding, resp_content) = process_body(&body_bytes, &headers);
    
    let body_data = BodyData {
        encoding: resp_encoding,
        content: resp_content,
        size: body_bytes.len() as u64,
    };

    match &mut flow.layer {
        Layer::Http(http) => {
            if let Some(resp) = &mut http.response {
                resp.body = Some(body_data);
            }
        },
        Layer::WebSocket(ws) => {
            ws.handshake_response.body = Some(body_data);
        },
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

pub fn build_client_response_from_flow(flow: &Flow, default_version: hyper::Version, strict_mode: bool) -> Result<Response<Full<Bytes>>, String> {
    if let Layer::Http(http) = &flow.layer {
        if let Some(response) = &http.response {
            let status = match StatusCode::from_u16(response.status) {
                Ok(s) => s,
                Err(_) => {
                    if strict_mode {
                        return Err(format!("Invalid status code: {}", response.status));
                    }
                    StatusCode::OK
                }
            };

            let mut builder = Response::builder()
                .status(status)
                .version(default_version); // TODO: Parse version from flow string if needed

            for (k, v) in &response.headers {
                // Filter out transport-level headers that might conflict with the new body
                if k.eq_ignore_ascii_case("content-length") 
                    || k.eq_ignore_ascii_case("transfer-encoding") 
                    || k.eq_ignore_ascii_case("connection") {
                    continue;
                }

                if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
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

            builder.body(Full::new(body_bytes))
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
    use super::{build_client_response_from_flow, parse_request_meta};
    use chrono::Utc;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode, Version};
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol,
    };
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn sample_flow_with_response(status: u16) -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12345,
                server_ip: "1.1.1.1".to_string(),
                server_port: 80,
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
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
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
        assert_eq!(resp.headers().get("x-test").and_then(|v| v.to_str().ok()), Some("1"));
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
        if let Layer::Http(http) = &mut flow.layer {
            if let Some(res) = &mut http.response {
                res.body = Some(relay_core_api::flow::BodyData {
                    encoding: "utf-8".to_string(),
                    content: "hello".to_string(),
                    size: 5,
                });
            }
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
