use relay_core_api::flow::{BodyData, Flow, HttpResponse, Layer, ResponseTiming, WebSocketMessage};
use relay_core_api::modification::FlowModification;
use relay_core_lib::InterceptionResult;
use url::Url;

// Re-export API modification types at relay_core_runtime::modification::
pub use relay_core_api::modification::{FlowQuery, FlowSummary};

/// 将 FlowModification 应用到 Flow 的请求或响应上。
///
/// `phase` 约定：以 "request" 开头表示修改请求，以 "response" 开头表示修改响应，
/// 其他值返回 Continue。
///
/// 如果 Flow 的 Layer 不支持（Tcp/Unknown 等），同样返回 Continue。
pub fn apply_flow_modification(
    flow: &Flow,
    phase: &str,
    mods: FlowModification,
) -> InterceptionResult {
    if phase.starts_with("request") {
        let mut req = match &flow.layer {
            Layer::Http(h) => h.request.clone(),
            Layer::WebSocket(ws) => ws.handshake_request.clone(),
            _ => return InterceptionResult::Continue,
        };

        if let Some(m) = mods.method {
            req.method = m;
        }
        if let Some(u) = mods.url {
            // 无效 URL 静默忽略，保留原始值
            if let Ok(parsed) = Url::parse(&u) {
                req.url = parsed;
            }
        }
        if let Some(h) = mods.request_headers {
            req.headers = h.into_iter().collect();
        }
        if let Some(b) = mods.request_body {
            req.body = Some(BodyData {
                encoding: "utf-8".to_string(),
                size: b.len() as u64,
                content: b,
            });
        }

        InterceptionResult::ModifiedRequest(req)
    } else if phase.starts_with("response") {
        let mut res = match &flow.layer {
            Layer::Http(h) => h.response.clone().unwrap_or_else(|| HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            Layer::WebSocket(ws) => ws.handshake_response.clone(),
            _ => return InterceptionResult::Continue,
        };

        if let Some(s) = mods.status_code {
            res.status = s;
        }
        if let Some(h) = mods.response_headers {
            res.headers = h.into_iter().collect();
        }
        if let Some(b) = mods.response_body {
            res.body = Some(BodyData {
                encoding: "utf-8".to_string(),
                size: b.len() as u64,
                content: b,
            });
        }

        InterceptionResult::ModifiedResponse(res)
    } else {
        InterceptionResult::Continue
    }
}

/// 将 FlowModification 应用到 WebSocket 消息上。
///
/// 仅修改 message_content；其余字段（方向、opcode、时间戳等）保持不变。
/// 若 modification 不含 message_content，则原样返回消息。
pub fn apply_ws_modification(
    message: &WebSocketMessage,
    mods: FlowModification,
) -> InterceptionResult {
    let mut new_msg = message.clone();
    if let Some(content) = mods.message_content {
        new_msg.content.size = content.len() as u64;
        new_msg.content.content = content;
    }
    InterceptionResult::ModifiedMessage(new_msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use relay_core_api::flow::{
        BodyData, Direction, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
        ResponseTiming, TransportProtocol, WebSocketLayer, WebSocketMessage,
    };
    use relay_core_api::modification::FlowModification;
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn make_http_flow(url: &str) -> Flow {
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
                    url: Url::parse(url).unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    body: None,
                    cookies: vec![],
                    query: vec![],
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    fn make_http_flow_with_response(url: &str) -> Flow {
        let mut flow = make_http_flow(url);
        if let Layer::Http(ref mut h) = flow.layer {
            h.response = Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            });
        }
        flow
    }

    fn make_ws_flow(url: &str) -> Flow {
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
            layer: Layer::WebSocket(WebSocketLayer {
                handshake_request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse(url).unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![("Upgrade".to_string(), "websocket".to_string())],
                    body: None,
                    cookies: vec![],
                    query: vec![],
                },
                handshake_response: HttpResponse {
                    status: 101,
                    status_text: "Switching Protocols".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    body: None,
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                    cookies: vec![],
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    fn make_ws_message(content: &str) -> WebSocketMessage {
        WebSocketMessage {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            direction: Direction::ClientToServer,
            content: BodyData {
                encoding: "utf-8".to_string(),
                content: content.to_string(),
                size: content.len() as u64,
            },
            opcode: "Text".to_string(),
        }
    }

    // --- apply_flow_modification: request phase ---

    #[test]
    fn test_request_modification_applies_all_fields() {
        let flow = make_http_flow("http://example.com/api");
        let mods = FlowModification {
            method: Some("POST".to_string()),
            url: Some("http://example.com/v2/api".to_string()),
            request_headers: Some(HashMap::from([("X-Custom".to_string(), "123".to_string())])),
            request_body: Some("new-body".to_string()),
            ..Default::default()
        };

        let result = apply_flow_modification(&flow, "request", mods);

        if let InterceptionResult::ModifiedRequest(req) = result {
            assert_eq!(req.method, "POST");
            assert_eq!(req.url.as_str(), "http://example.com/v2/api");
            assert!(
                req.headers
                    .iter()
                    .any(|(k, v)| k == "X-Custom" && v == "123")
            );
            assert_eq!(req.body.unwrap().content, "new-body");
        } else {
            panic!("expected ModifiedRequest");
        }
    }

    #[test]
    fn test_request_modification_invalid_url_keeps_original() {
        let flow = make_http_flow("http://example.com/api");
        let original_url = match &flow.layer {
            Layer::Http(h) => h.request.url.clone(),
            _ => panic!("expected http layer"),
        };
        let mods = FlowModification {
            method: Some("PUT".to_string()),
            url: Some("://invalid-url".to_string()),
            ..Default::default()
        };

        let result = apply_flow_modification(&flow, "request", mods);

        match result {
            InterceptionResult::ModifiedRequest(req) => {
                assert_eq!(req.method, "PUT");
                assert_eq!(req.url, original_url, "invalid URL should keep original");
            }
            other => panic!("expected ModifiedRequest, got {:?}", other),
        }
    }

    #[test]
    fn test_request_headers_phase_prefix_routes_to_request() {
        let flow = make_http_flow("http://example.com/api");
        let mods = FlowModification {
            method: Some("PATCH".to_string()),
            ..Default::default()
        };
        let result = apply_flow_modification(&flow, "request_headers", mods);
        assert!(matches!(result, InterceptionResult::ModifiedRequest(_)));
    }

    #[test]
    fn test_request_body_phase_prefix_routes_to_request() {
        let flow = make_http_flow("http://example.com/api");
        let mods = FlowModification {
            request_body: Some("hello".to_string()),
            ..Default::default()
        };
        let result = apply_flow_modification(&flow, "request_body", mods);
        assert!(matches!(result, InterceptionResult::ModifiedRequest(_)));
    }

    // --- apply_flow_modification: response phase ---

    #[test]
    fn test_response_modification_applies_all_fields() {
        let flow = make_http_flow_with_response("http://example.com/api");
        let mods = FlowModification {
            status_code: Some(404),
            response_headers: Some(HashMap::from([(
                "Content-Type".to_string(),
                "application/json".to_string(),
            )])),
            response_body: Some("{\"error\": \"not found\"}".to_string()),
            ..Default::default()
        };

        let result = apply_flow_modification(&flow, "response", mods);

        if let InterceptionResult::ModifiedResponse(res) = result {
            assert_eq!(res.status, 404);
            assert!(
                res.headers
                    .iter()
                    .any(|(k, v)| k == "Content-Type" && v == "application/json")
            );
            assert_eq!(res.body.unwrap().content, "{\"error\": \"not found\"}");
        } else {
            panic!("expected ModifiedResponse");
        }
    }

    #[test]
    fn test_response_modification_no_existing_response_uses_default() {
        // Flow has no response yet
        let flow = make_http_flow("http://example.com/api");
        let mods = FlowModification {
            status_code: Some(503),
            ..Default::default()
        };

        let result = apply_flow_modification(&flow, "response_headers", mods);

        if let InterceptionResult::ModifiedResponse(res) = result {
            assert_eq!(res.status, 503);
        } else {
            panic!("expected ModifiedResponse");
        }
    }

    // --- apply_flow_modification: websocket handshake ---

    #[test]
    fn test_ws_handshake_request_modification() {
        let flow = make_ws_flow("ws://example.com/socket");
        let mods = FlowModification {
            url: Some("ws://example.com/socket-v2".to_string()),
            ..Default::default()
        };

        let result = apply_flow_modification(&flow, "request", mods);

        if let InterceptionResult::ModifiedRequest(req) = result {
            assert_eq!(req.url.as_str(), "ws://example.com/socket-v2");
        } else {
            panic!("expected ModifiedRequest for WebSocket handshake");
        }
    }

    // --- apply_flow_modification: unknown phase ---

    #[test]
    fn test_unknown_phase_returns_continue() {
        let flow = make_http_flow("http://example.com/api");
        let mods = FlowModification::default();
        let result = apply_flow_modification(&flow, "pre-request", mods);
        assert!(matches!(result, InterceptionResult::Continue));
    }

    // --- apply_ws_modification ---

    #[test]
    fn test_ws_modification_replaces_content() {
        let msg = make_ws_message("original");
        let mods = FlowModification {
            message_content: Some("modified".to_string()),
            ..Default::default()
        };

        let result = apply_ws_modification(&msg, mods);

        if let InterceptionResult::ModifiedMessage(new_msg) = result {
            assert_eq!(new_msg.content.content, "modified");
            assert_eq!(new_msg.content.size, 8);
            assert_eq!(new_msg.direction, Direction::ClientToServer);
            assert_eq!(new_msg.opcode, "Text");
        } else {
            panic!("expected ModifiedMessage");
        }
    }

    #[test]
    fn test_ws_modification_no_content_returns_original_message() {
        let msg = make_ws_message("origin");
        let mods = FlowModification::default();

        let result = apply_ws_modification(&msg, mods);

        if let InterceptionResult::ModifiedMessage(new_msg) = result {
            assert_eq!(new_msg.content.content, "origin");
            assert_eq!(new_msg.content.size, 6);
        } else {
            panic!("expected ModifiedMessage");
        }
    }
}
