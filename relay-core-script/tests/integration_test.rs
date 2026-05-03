use relay_core_script::ScriptInterceptor;
use relay_core_lib::interceptor::{Interceptor, RequestAction, ResponseAction, WebSocketMessageAction, BoxError};
use relay_core_api::flow::{Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, TransportProtocol, WebSocketMessage, BodyData, Direction, ResponseTiming};
use url::Url;
use uuid::Uuid;
use chrono::Utc;
use std::collections::HashMap;
use http_body_util::{Full, BodyExt};
use bytes::Bytes;

#[tokio::test]
async fn test_deno_script_on_request() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    
    // JS script that modifies a header - Deno style
    let script = r#"
        globalThis.onRequest = function(context, flow) {
            // flow.layer is { type: "Http", data: { request: ... } }
            if (flow.layer.type === "Http") {
                flow.layer.data.request.headers.push(["X-Deno-Scripted", "true"]);
            }
            
            return flow;
        }
    "#;
    
    interceptor.load_script(script).await.unwrap();
    
    let mut flow = create_dummy_flow();
    let body = Full::new(Bytes::new()).map_err(|e| Box::new(e) as BoxError).boxed();
    
    match interceptor.on_request(&mut flow, body).await.unwrap() {
        RequestAction::Continue(_) => {
            if let Layer::Http(http) = &flow.layer {
                let headers = &http.request.headers;
                assert!(headers.iter().any(|(k, v)| k == "X-Deno-Scripted" && v == "true"));
            } else {
                panic!("Flow layer is not Http");
            }
        },
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_deno_script_on_websocket_message() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    
    let script = r#"
        globalThis.onWebSocketMessage = function(context, flow, message) {
            if (message.content.encoding === "utf-8") {
                message.content.content += " [Modified]";
            }
            return message;
        }
    "#;
    
    interceptor.load_script(script).await.unwrap();
    
    let mut flow = create_dummy_flow();
    
    let message = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "utf-8".to_string(),
            content: "Hello".to_string(),
            size: 5,
        },
        opcode: "Text".to_string(),
    };
    
    match interceptor.on_websocket_message(&mut flow, message.clone()).await.unwrap() {
        WebSocketMessageAction::Continue(mod_msg) => {
            assert_eq!(mod_msg.content.content, "Hello [Modified]");
        },
        res => panic!("Expected Continue, got {:?}", res),
    }
}

fn create_dummy_flow() -> Flow {
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
                url: Url::parse("http://example.com").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("User-Agent".to_string(), "Test".to_string())],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            response: None,
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
    }
}

#[tokio::test]
async fn test_deno_script_on_websocket_binary_message() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    
    let script = r#"
        globalThis.onWebSocketMessage = function(context, flow, message) {
            if (message.content.encoding === "base64") {
                // Replace "Hello" (SGVsbG8=) with "Hello [BinMod]" (SGVsbG8gW0Jpbk1vZF0=)
                if (message.content.content === "SGVsbG8=") {
                    message.content.content = "SGVsbG8gW0Jpbk1vZF0=";
                }
            }
            return message;
        }
    "#;
    
    interceptor.load_script(script).await.unwrap();
    
    let mut flow = create_dummy_flow();
    
    let message = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "base64".to_string(),
            content: "SGVsbG8=".to_string(),
            size: 5,
        },
        opcode: "Binary".to_string(),
    };
    
    match interceptor.on_websocket_message(&mut flow, message.clone()).await.unwrap() {
        WebSocketMessageAction::Continue(mod_msg) => {
            assert_eq!(mod_msg.content.content, "SGVsbG8gW0Jpbk1vZF0=");
            assert_eq!(mod_msg.content.encoding, "base64");
        },
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_deno_script_on_response() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onResponse = function(body, flow) {
            if (flow.layer.type === "Http" && flow.layer.data.response) {
                flow.layer.data.response.headers.push(["X-Resp-Scripted", "true"]);
            }
            return flow;
        }
    "#;
    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    if let Layer::Http(http) = &mut flow.layer {
        http.response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
            body: None,
            timing: ResponseTiming { time_to_first_byte: None, time_to_last_byte: None },
            cookies: vec![],
        });
    }

    let body = Full::new(Bytes::from("payload"))
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_response(&mut flow, body).await.unwrap() {
        ResponseAction::Continue(new_body) => {
            let bytes = new_body.collect().await.unwrap().to_bytes();
            assert_eq!(bytes.as_ref(), b"payload");
            if let Layer::Http(http) = &flow.layer {
                let response = http.response.as_ref().expect("response should exist");
                assert!(
                    response
                        .headers
                        .iter()
                        .any(|(k, v)| k == "X-Resp-Scripted" && v == "true")
                );
            } else {
                panic!("Flow layer is not Http");
            }
        }
        other => panic!("Expected Continue, got {:?}", other),
    }
}

#[tokio::test]
async fn test_deno_script_invalid_source_rejected() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let bad_script = "globalThis.onRequest = () => { invalid javascript !!!";
    let result = interceptor.load_script(bad_script).await;
    assert!(result.is_err(), "invalid script source should fail to load");
}
