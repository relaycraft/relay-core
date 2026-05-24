use chrono::Utc;
use relay_core_api::flow::{
    BodyData, Direction, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
    ResponseTiming, TransportProtocol, WebSocketLayer, WebSocketMessage,
};
use relay_core_tauri::commands::Modification;
use relay_core_tauri::transport::{FlowDetail, FlowIndex};
use std::collections::HashMap;
use url::Url;
use uuid::Uuid;

#[test]
fn test_flow_to_flow_index_conversion() {
    // 1. Create a sample Flow (HTTP)
    let flow_id = Uuid::new_v4();
    let now = Utc::now();

    let flow = Flow {
        id: flow_id,
        start_time: now,
        end_time: Some(now + chrono::Duration::milliseconds(100)),
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
                url: Url::parse("http://example.com/api/test").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![
                    ("Host".to_string(), "example.com".to_string()),
                    ("User-Agent".to_string(), "RelayCraft-Test".to_string()),
                ],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("Content-Length".to_string(), "15".to_string()),
                ],
                body: Some(BodyData {
                    encoding: "utf-8".to_string(),
                    content: "{\"status\":\"ok\"}".to_string(),
                    size: 15,
                }),
                timing: ResponseTiming {
                    time_to_first_byte: Some(50),
                    time_to_last_byte: Some(100),
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            error: None,
        }),
        tags: vec!["proxy".to_string()],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    // 2. Convert to FlowIndex
    let index: FlowIndex = FlowIndex::from(flow.clone());

    // 3. Verify Fields
    assert_eq!(index.id, flow_id.to_string());
    assert_eq!(index.method, "GET");
    assert_eq!(index.url, "http://example.com/api/test");
    assert_eq!(index.host, "example.com");
    assert_eq!(index.path, "/api/test");
    assert_eq!(index.status, 200);
    assert_eq!(index.content_type, "application/json");
    assert_eq!(index.time, 100);
    assert_eq!(index.size, 15);
    assert_eq!(index.has_request_body, false);
    assert_eq!(index.has_response_body, true);
    assert_eq!(index.is_websocket, false);

    // 4. Verify JSON Serialization (CamelCase check)
    let json = serde_json::to_string(&index).unwrap();
    println!("FlowIndex JSON: {}", json);

    assert!(json.contains("\"msgTs\""));
    assert!(json.contains("\"httpVersion\""));
    assert!(json.contains("\"contentType\""));
    assert!(json.contains("\"startedDateTime\""));
    assert!(json.contains("\"clientIp\""));
    assert!(json.contains("\"hasError\""));
}

#[test]
fn test_modification_deserialization() {
    // 1. Simulate Frontend JSON Payload for Modification
    // Frontend uses camelCase, Rust struct expects camelCase due to serde rename_all
    let json_payload = r#"{
        "method": "POST",
        "url": "http://example.com/modified",
        "requestHeaders": {
            "X-Custom-Header": "ModifiedValue"
        },
        "requestBody": "new-body-content",
        "statusCode": 201,
        "responseHeaders": {
            "Content-Type": "text/plain"
        },
        "responseBody": "response-modified"
    }"#;

    // 2. Deserialize
    let modification: Modification =
        serde_json::from_str(json_payload).expect("Failed to deserialize Modification");

    // 3. Verify Fields
    assert_eq!(modification.method.as_deref(), Some("POST"));
    assert_eq!(
        modification.url.as_deref(),
        Some("http://example.com/modified")
    );

    let req_headers = modification.request_headers.as_ref().unwrap();
    assert_eq!(
        req_headers.get("X-Custom-Header").map(|s| s.as_str()),
        Some("ModifiedValue")
    );

    assert_eq!(
        modification.request_body.as_deref(),
        Some("new-body-content")
    );
    assert_eq!(modification.status_code, Some(201));

    let resp_headers = modification.response_headers.as_ref().unwrap();
    assert_eq!(
        resp_headers.get("Content-Type").map(|s| s.as_str()),
        Some("text/plain")
    );

    assert_eq!(
        modification.response_body.as_deref(),
        Some("response-modified")
    );
}

#[test]
fn test_partial_modification_deserialization() {
    // Test that partial updates work (Option fields)
    let json_payload = r#"{
        "statusCode": 404
    }"#;

    let modification: Modification =
        serde_json::from_str(json_payload).expect("Failed to deserialize Partial Modification");

    assert_eq!(modification.status_code, Some(404));
    assert!(modification.method.is_none());
    assert!(modification.request_headers.is_none());
}

#[test]
fn test_websocket_flow_to_detail_conversion() {
    let flow_id = Uuid::new_v4();
    let now = Utc::now();

    let flow = Flow {
        id: flow_id,
        start_time: now,
        end_time: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 54321,
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
                url: Url::parse("ws://example.com/socket").unwrap(),
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
            messages: vec![
                WebSocketMessage {
                    id: Uuid::new_v4(),
                    timestamp: now,
                    direction: Direction::ClientToServer,
                    content: BodyData {
                        encoding: "text".to_string(),
                        content: "Hello Server".to_string(),
                        size: 12,
                    },
                    opcode: "Text".to_string(),
                },
                WebSocketMessage {
                    id: Uuid::new_v4(),
                    timestamp: now,
                    direction: Direction::ServerToClient,
                    content: BodyData {
                        encoding: "text".to_string(),
                        content: "Hello Client".to_string(),
                        size: 12,
                    },
                    opcode: "Text".to_string(),
                },
            ],
            closed: false,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let detail = FlowDetail::from(flow);

    assert_eq!(detail.id, flow_id.to_string());
    assert_eq!(detail._rc.is_websocket, true);
    assert_eq!(detail._rc.websocket_frame_count, 2);
    assert_eq!(detail._rc.websocket_messages.len(), 2);

    let msg1 = &detail._rc.websocket_messages[0];
    assert_eq!(msg1.from_client, true);
    assert_eq!(msg1.content, "Hello Server");
    assert_eq!(msg1.type_field, "text");

    let msg2 = &detail._rc.websocket_messages[1];
    assert_eq!(msg2.from_client, false);
    assert_eq!(msg2.content, "Hello Client");

    // Check JSON serialization for camelCase and special renames
    let json = serde_json::to_string(&detail).unwrap();
    println!("FlowDetail JSON: {}", json);
    assert!(json.contains("\"websocketFrames\"")); // Special rename
    assert!(json.contains("\"type\"")); // Special rename for type_field
    assert!(json.contains("\"fromClient\""));
}

#[test]
fn test_websocket_modification_deserialization() {
    let json_payload = r#"{
        "messageContent": "new-ws-message"
    }"#;

    let modification: Modification =
        serde_json::from_str(json_payload).expect("Failed to deserialize WS Modification");

    assert_eq!(
        modification.message_content.as_deref(),
        Some("new-ws-message")
    );
    assert!(modification.method.is_none());
}

#[test]
fn test_flow_to_flow_index_without_response_defaults() {
    let flow = Flow {
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
                url: Url::parse("http://example.com/no-response").unwrap(),
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
    };

    let index: FlowIndex = FlowIndex::from(flow);
    assert_eq!(index.status, 0, "missing response should map to status=0");
    assert_eq!(
        index.content_type, "",
        "missing response should have empty content type"
    );
    assert_eq!(index.size, 0, "missing response body size should be zero");
    assert!(!index.has_response_body);
}

#[test]
fn test_modification_deserialization_ignores_unknown_fields() {
    let json_payload = r#"{
        "method": "PATCH",
        "messageContent": "ws-body",
        "unexpectedField": "ignore-me"
    }"#;

    let modification: Modification =
        serde_json::from_str(json_payload).expect("unknown field should be ignored");
    assert_eq!(modification.method.as_deref(), Some("PATCH"));
    assert_eq!(modification.message_content.as_deref(), Some("ws-body"));
}

#[test]
fn test_flow_detail_http_har_compat_fields() {
    let now = Utc::now();
    let flow = Flow {
        id: Uuid::new_v4(),
        start_time: now,
        end_time: Some(now + chrono::Duration::milliseconds(23)),
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
                method: "POST".to_string(),
                url: Url::parse("http://example.com/h").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: Some(BodyData {
                    encoding: "utf-8".to_string(),
                    content: "{\"k\":1}".to_string(),
                    size: 7,
                }),
                cookies: vec![],
                query: vec![],
            },
            response: Some(HttpResponse {
                status: 302,
                status_text: "Found".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Location".to_string(), "http://example.com/r".to_string())],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: Some(5),
                    time_to_last_byte: Some(10),
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let detail = FlowDetail::from(flow);
    assert_eq!(detail.request.headers_size, 52);
    assert_eq!(detail.response.headers_size, 54);
    assert_eq!(detail.response.redirect_url, "http://example.com/r");
    assert_eq!(detail.timings.send, 0);
    assert_eq!(detail.timings.wait, 0);
    assert_eq!(detail.timings.receive, 0);
}

#[test]
fn test_flow_detail_post_data_and_response_content_preserve_base64_encoding() {
    let flow = Flow {
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
                method: "POST".to_string(),
                url: Url::parse("http://example.com/upload").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![(
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                )],
                body: Some(BodyData {
                    encoding: "base64".to_string(),
                    content: "AQID".to_string(),
                    size: 3,
                }),
                cookies: vec![],
                query: vec![],
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![(
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                )],
                body: Some(BodyData {
                    encoding: "base64".to_string(),
                    content: "BAUG".to_string(),
                    size: 3,
                }),
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let detail = FlowDetail::from(flow);
    let post = detail.request.post_data.expect("postData should exist");
    assert_eq!(post.encoding.as_deref(), Some("base64"));
    assert_eq!(post.text.as_deref(), Some("AQID"));
    assert_eq!(detail.response.content.encoding.as_deref(), Some("base64"));
    assert_eq!(detail.response.content.text.as_deref(), Some("BAUG"));
}

#[test]
fn test_flow_detail_request_query_string_uses_struct_field() {
    let flow = Flow {
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
                url: Url::parse("http://example.com/search?q=ignored").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                cookies: vec![],
                query: vec![
                    ("q".to_string(), "hello".to_string()),
                    ("lang".to_string(), "zh".to_string()),
                ],
            },
            response: None,
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let detail = FlowDetail::from(flow);
    assert_eq!(detail.request.query_string.len(), 2);
    assert_eq!(detail.request.query_string[0].name, "q");
    assert_eq!(detail.request.query_string[0].value, "hello");
    assert_eq!(detail.request.query_string[1].name, "lang");
    assert_eq!(detail.request.query_string[1].value, "zh");
}

#[test]
fn test_flow_detail_redirect_url_empty_without_location_header() {
    let flow = Flow {
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
                url: Url::parse("http://example.com/no-redirect").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let detail = FlowDetail::from(flow);
    assert_eq!(detail.response.redirect_url, "");
}

#[test]
fn test_flow_index_websocket_fields_reflect_handshake_and_message_count() {
    let flow = Flow {
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
                url: Url::parse("ws://example.com/chat").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            handshake_response: HttpResponse {
                status: 101,
                status_text: "Switching Protocols".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            },
            messages: vec![
                WebSocketMessage {
                    id: Uuid::new_v4(),
                    timestamp: Utc::now(),
                    direction: Direction::ClientToServer,
                    content: BodyData {
                        encoding: "utf-8".to_string(),
                        content: "a".to_string(),
                        size: 1,
                    },
                    opcode: "Text".to_string(),
                },
                WebSocketMessage {
                    id: Uuid::new_v4(),
                    timestamp: Utc::now(),
                    direction: Direction::ServerToClient,
                    content: BodyData {
                        encoding: "utf-8".to_string(),
                        content: "b".to_string(),
                        size: 1,
                    },
                    opcode: "Text".to_string(),
                },
                WebSocketMessage {
                    id: Uuid::new_v4(),
                    timestamp: Utc::now(),
                    direction: Direction::ClientToServer,
                    content: BodyData {
                        encoding: "utf-8".to_string(),
                        content: "c".to_string(),
                        size: 1,
                    },
                    opcode: "Text".to_string(),
                },
            ],
            closed: false,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    };

    let index = FlowIndex::from(flow);
    assert!(index.is_websocket);
    assert_eq!(index.websocket_frame_count, 3);
    assert_eq!(index.status, 101);
    assert_eq!(index.path, "/chat");
}
