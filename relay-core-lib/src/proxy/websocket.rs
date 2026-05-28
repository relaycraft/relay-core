use crate::capture::loop_detection::LoopDetector;
use crate::intercept::types::{
    BoxError, HttpBody, InterceptionResult, Interceptor, RequestAction, WebSocketMessageAction,
};
use crate::proxy::http_utils::{
    HttpsClient, create_error_response, create_initial_flow, mock_to_response, parse_request_meta,
};
use chrono::Utc;
use data_encoding::BASE64;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::upgrade::Upgraded;
use hyper::{Request, Response, StatusCode};
use relay_core_api::flow::{
    BodyData, Direction, Flow, FlowUpdate, HttpResponse, Layer, WebSocketMessage,
};
use relay_core_api::policy::ProxyPolicy;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use url::Url;
use uuid::Uuid;

use hyper_util::rt::TokioIo;
use relay_core_api::flow::ResponseTiming;
use tokio::sync::watch;

fn validate_ws_strict_handshake<B>(
    req: &Request<B>,
    policy: &ProxyPolicy,
) -> Result<(), Box<Response<HttpBody>>> {
    if !policy.strict_http_semantics {
        return Ok(());
    }

    if !req.headers().contains_key(hyper::header::SEC_WEBSOCKET_KEY) {
        return Err(Box::new(create_error_response(
            StatusCode::BAD_REQUEST,
            "Missing Sec-WebSocket-Key header in Strict Mode",
        )));
    }

    if let Some(v) = req.headers().get(hyper::header::SEC_WEBSOCKET_VERSION) {
        if v != "13" {
            return Err(Box::new(create_error_response(
                StatusCode::BAD_REQUEST,
                "Unsupported WebSocket Version in Strict Mode (Expected 13)",
            )));
        }
    } else {
        return Err(Box::new(create_error_response(
            StatusCode::BAD_REQUEST,
            "Missing Sec-WebSocket-Version header in Strict Mode",
        )));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_websocket_handshake<B>(
    req: Request<B>,
    client_addr: SocketAddr,
    on_flow: Sender<FlowUpdate>,
    client: Arc<HttpsClient>,
    interceptor: Arc<dyn Interceptor>,
    is_mitm: bool,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
) -> Result<Response<HttpBody>, Infallible>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // 2. Parse L7 (HTTP) - Re-extract for WS
    let meta = parse_request_meta(&req, is_mitm);
    let policy = policy_rx.borrow().clone();

    // STRICT MODE CHECKS
    if let Err(resp) = validate_ws_strict_handshake(&req, &policy) {
        return Ok(*resp);
    }

    // Create Initial Flow for WebSocket Handshake
    let mut flow = create_initial_flow(meta.clone(), None, client_addr, is_mitm, true);

    // INTERCEPT HEADERS (Handshake)
    match interceptor.on_request_headers(&mut flow).await {
        InterceptionResult::Drop => {
            return Ok(create_error_response(StatusCode::FORBIDDEN, ""));
        }
        InterceptionResult::MockResponse(mock) => {
            if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                crate::metrics::inc_flows_dropped();
            }
            return Ok(mock_to_response(mock));
        }
        InterceptionResult::ModifiedRequest(req) => {
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.handshake_request = req;
            }
        }
        InterceptionResult::ModifiedResponse(res) => {
            if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                crate::metrics::inc_flows_dropped();
            }
            return Ok(mock_to_response(res));
        }
        _ => {}
    }

    // INTERCEPT REQUEST (Handshake Full)
    // WS Handshake has empty body
    let body = http_body_util::Empty::new().map_err(|e| e.into()).boxed();

    match interceptor.on_request(&mut flow, body).await {
        Ok(RequestAction::Drop) => {
            return Ok(create_error_response(StatusCode::FORBIDDEN, ""));
        }
        Ok(RequestAction::MockResponse(res)) => {
            if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                crate::metrics::inc_flows_dropped();
            }
            let (parts, body) = res.into_parts();
            return Ok(Response::from_parts(parts, body));
        }
        Ok(RequestAction::Continue(_)) => {}
        Err(e) => {
            return Ok(create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Interceptor Error: {}", e),
            ));
        }
    }

    if on_flow
        .try_send(FlowUpdate::Full(Box::new(flow.clone())))
        .is_err()
    {
        crate::metrics::inc_flows_dropped();
    }

    // Prepare Upgrade
    let (parts, body) = req.into_parts();
    let req_for_upgrade = Request::from_parts(parts, body);

    // Determine Target URL
    let mut target_url_str = meta.url_str.clone();

    if policy.transparent_enabled
        && let Some(addr) = target_addr
    {
        flow.tags.push("transparent".to_string());

        // Update Flow Network Info
        flow.network.server_ip = addr.ip().to_string();
        flow.network.server_port = addr.port();

        // Loop Detection
        if loop_detector.would_loop(addr) {
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.handshake_response.status = 508;
                ws.closed = true;
            }
            if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                crate::metrics::inc_flows_dropped();
            }
            return Ok(create_error_response(
                StatusCode::LOOP_DETECTED,
                "Loop Detected",
            ));
        }

        // Rewrite URI
        let mut u = if let Layer::WebSocket(ws) = &flow.layer {
            ws.handshake_request.url.clone()
        } else {
            Url::parse(&meta.url_str).unwrap_or_else(|_| Url::parse("http://unknown/").unwrap())
        };

        if u.set_ip_host(addr.ip()).is_ok() {
            u.set_port(Some(addr.port())).ok();
            // Ensure scheme is correct (ws/wss)
            if is_mitm && (u.scheme() == "http" || u.scheme() == "ws") {
                u.set_scheme("wss").ok();
            } else if !is_mitm && (u.scheme() == "https" || u.scheme() == "wss") {
                u.set_scheme("ws").ok();
            }
            target_url_str = u.to_string();
        }
    }

    // Prepare Forward Request
    let current_req = if let Layer::WebSocket(ws) = &flow.layer {
        &ws.handshake_request
    } else {
        return Ok(create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid Flow Layer State",
        ));
    };

    let mut forward_req_builder = Request::builder()
        .method(current_req.method.as_str())
        .uri(target_url_str.as_str())
        .version(hyper::Version::HTTP_11);

    for (k, v) in current_req.headers.iter() {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            forward_req_builder = forward_req_builder.header(name, val);
        }
    }

    let forward_req =
        match forward_req_builder.body(Full::new(Bytes::new()).map_err(|e| e.into()).boxed()) {
            Ok(req) => req,
            Err(e) => {
                return Ok(create_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to build forward request: {}", e),
                ));
            }
        };

    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client.request(forward_req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                let (parts, body) = resp.into_parts();
                let resp_for_upgrade = Response::from_parts(parts.clone(), body);

                // Spawn upgrade task
                let on_flow_clone = on_flow.clone();
                let interceptor_clone = interceptor.clone();
                let flow_clone = flow.clone(); // Clone flow for the tunnel task

                tokio::task::spawn(async move {
                    // Add timeout for upgrades
                    let upgrade_timeout = std::time::Duration::from_secs(10);

                    let client_upgrade =
                        tokio::time::timeout(upgrade_timeout, hyper::upgrade::on(req_for_upgrade));
                    let server_upgrade =
                        tokio::time::timeout(upgrade_timeout, hyper::upgrade::on(resp_for_upgrade));

                    match tokio::try_join!(client_upgrade, server_upgrade) {
                        Ok((Ok(upgraded_client), Ok(upgraded_server))) => {
                            // Start WebSocket Tunnel
                            if let Err(e) = handle_websocket_tunnel(
                                upgraded_client,
                                upgraded_server,
                                flow_clone,
                                on_flow_clone,
                                interceptor_clone,
                            )
                            .await
                            {
                                tracing::error!("WebSocket Tunnel Error: {}", e);
                            }
                        }
                        Ok((Err(e), _)) => tracing::error!("Client WebSocket Upgrade Error: {}", e),
                        Ok((_, Err(e))) => {
                            tracing::error!("Upstream WebSocket Upgrade Error: {}", e)
                        }
                        Err(_) => tracing::error!("WebSocket Upgrade Timed Out"),
                    }
                });

                let mut client_resp_builder = Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .version(parts.version);

                for (k, v) in parts.headers.iter() {
                    client_resp_builder = client_resp_builder.header(k, v);
                }

                let client_resp = match client_resp_builder
                    .body(Full::new(Bytes::new()).map_err(|e| e.into()).boxed())
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!("Failed to build 101 Switching Protocols response: {}", e);
                        return Ok(create_error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Response build failed",
                        ));
                    }
                };

                // Update handshake response in flow (but we don't send update here as flow is moved/cloned)
                // Ideally we should send an update here for the handshake response
                // But `flow` variable is local.
                // We can construct a partial update? Or just ignore.
                // The tunnel will send messages.

                Ok(client_resp)
            } else {
                // Normal HTTP Response (Handshake failed)
                let (parts, body) = resp.into_parts();
                let body_bytes = match body.collect().await {
                    Ok(c) => c.to_bytes(),
                    Err(_) => Bytes::new(),
                };

                // Create HTTP Response for Flow
                let http_resp = HttpResponse {
                    status: parts.status.as_u16(),
                    status_text: parts
                        .status
                        .canonical_reason()
                        .unwrap_or("Unknown")
                        .to_string(),
                    version: format!("{:?}", parts.version),
                    headers: parts
                        .headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                String::from_utf8_lossy(v.as_bytes()).to_string(),
                            )
                        })
                        .collect(),
                    cookies: vec![], // Todo: parse cookies
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: String::from_utf8_lossy(&body_bytes).to_string(),
                        size: body_bytes.len() as u64,
                    }),
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                };

                if let Layer::WebSocket(ws) = &mut flow.layer {
                    ws.handshake_response = http_resp.clone();
                    ws.closed = true;
                }

                if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                    crate::metrics::inc_flows_dropped();
                }

                Ok(Response::from_parts(
                    parts,
                    Full::new(body_bytes).map_err(|e| e.into()).boxed(),
                ))
            }
        }
        Ok(Err(e)) => Ok(create_error_response(
            StatusCode::BAD_GATEWAY,
            format!("Upstream Handshake Failed: {}", e),
        )),
        Err(_) => Ok(create_error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "Upstream Handshake Timed Out",
        )),
    }
}

async fn handle_websocket_tunnel(
    client_io: Upgraded,
    server_io: Upgraded,
    mut flow: Flow,
    on_flow: Sender<FlowUpdate>,
    interceptor: Arc<dyn Interceptor>,
) -> Result<(), BoxError> {
    let client_ws = WebSocketStream::from_raw_socket(
        TokioIo::new(client_io),
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let server_ws = WebSocketStream::from_raw_socket(
        TokioIo::new(server_io),
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut server_tx, mut server_rx) = server_ws.split();

    // Idle timeout for WebSocket
    let idle_timeout_duration = std::time::Duration::from_secs(300); // 5 minutes

    interceptor.on_websocket_start(&mut flow).await;

    loop {
        let event = tokio::time::timeout(idle_timeout_duration, async {
            tokio::select! {
                msg = client_rx.next() => (Direction::ClientToServer, msg),
                msg = server_rx.next() => (Direction::ServerToClient, msg),
            }
        })
        .await;

        match event {
            Ok((dir, msg_opt)) => {
                match msg_opt {
                    Some(Ok(msg)) => {
                        // Handle Message
                        let (sender, _receiver, intercept_dir) = if dir == Direction::ClientToServer
                        {
                            (&mut server_tx, &mut client_tx, Direction::ClientToServer)
                        } else {
                            (&mut client_tx, &mut server_tx, Direction::ServerToClient)
                        };

                        if let Some(ws_msg) = tungstenite_to_flow_msg(msg.clone(), intercept_dir) {
                            match interceptor
                                .on_websocket_message(&mut flow, ws_msg.clone())
                                .await
                            {
                                Ok(WebSocketMessageAction::Drop) => continue,
                                Ok(WebSocketMessageAction::Continue(mod_msg)) => {
                                    let t_msg = flow_msg_to_tungstenite(&mod_msg);
                                    if let Err(e) = sender.send(t_msg).await {
                                        interceptor
                                            .on_websocket_error(&mut flow, &e.to_string())
                                            .await;
                                        return Err(e.into());
                                    }

                                    if on_flow
                                        .try_send(FlowUpdate::WebSocketMessage {
                                            flow_id: flow.id.to_string(),
                                            message: mod_msg,
                                        })
                                        .is_err()
                                    {
                                        crate::metrics::inc_flows_dropped();
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("WebSocket Interception Error: {}", e);
                                    sender.send(msg).await?;

                                    if on_flow
                                        .try_send(FlowUpdate::WebSocketMessage {
                                            flow_id: flow.id.to_string(),
                                            message: ws_msg,
                                        })
                                        .is_err()
                                    {
                                        crate::metrics::inc_flows_dropped();
                                    }
                                }
                            }
                        } else {
                            // Non-data message
                            if let Err(e) = sender.send(msg).await {
                                interceptor
                                    .on_websocket_error(&mut flow, &e.to_string())
                                    .await;
                                return Err(e.into());
                            }
                        }
                    }
                    Some(Err(e)) => {
                        interceptor
                            .on_websocket_error(&mut flow, &e.to_string())
                            .await;
                        return Err(e.into());
                    }
                    None => {
                        interceptor
                            .on_websocket_end(&mut flow, 1000, "normal")
                            .await;
                        break;
                    }
                }
            }
            Err(_) => {
                tracing::warn!("WebSocket Tunnel Idle Timeout");
                interceptor
                    .on_websocket_error(&mut flow, "WebSocket Idle Timeout")
                    .await;
                return Err("WebSocket Idle Timeout".into());
            }
        }
    }

    Ok(())
}

fn tungstenite_to_flow_msg(msg: Message, dir: Direction) -> Option<WebSocketMessage> {
    let (opcode, content, encoding, size) = match msg {
        Message::Text(t) => {
            let len = t.len();
            ("Text", t.to_string(), "utf-8", len)
        }
        Message::Binary(b) => {
            let len = b.len();
            ("Binary", BASE64.encode(&b), "base64", len)
        }
        Message::Ping(b) => {
            let len = b.len();
            ("Ping", BASE64.encode(&b), "base64", len)
        }
        Message::Pong(b) => {
            let len = b.len();
            ("Pong", BASE64.encode(&b), "base64", len)
        }
        Message::Close(_) => ("Close", String::new(), "none", 0),
        Message::Frame(_) => return None,
    };

    Some(WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: dir,
        content: BodyData {
            encoding: encoding.to_string(),
            content,
            size: size as u64,
        },
        opcode: opcode.to_string(),
    })
}

fn flow_msg_to_tungstenite(msg: &WebSocketMessage) -> Message {
    match msg.opcode.as_str() {
        "Text" => Message::Text(msg.content.content.clone().into()),
        "Binary" => {
            if let Ok(b) = BASE64.decode(msg.content.content.as_bytes()) {
                Message::Binary(Bytes::from(b))
            } else {
                Message::Binary(Bytes::new())
            }
        }
        "Ping" => {
            if let Ok(b) = BASE64.decode(msg.content.content.as_bytes()) {
                Message::Ping(Bytes::from(b))
            } else {
                Message::Ping(Bytes::new())
            }
        }
        "Pong" => {
            if let Ok(b) = BASE64.decode(msg.content.content.as_bytes()) {
                Message::Pong(Bytes::from(b))
            } else {
                Message::Pong(Bytes::new())
            }
        }
        "Close" => Message::Close(None),
        _ => Message::Text(msg.content.content.clone().into()),
    }
}

#[cfg(test)]
mod websocket_tests {
    use super::*;
    use http_body_util::Empty;

    #[test]
    fn test_validate_ws_strict_handshake_rejects_missing_key() {
        let policy = ProxyPolicy {
            strict_http_semantics: true,
            ..Default::default()
        };
        let req = Request::builder()
            .method("GET")
            .uri("ws://example.com/socket")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .body(Empty::<Bytes>::new())
            .expect("request build");
        let result = validate_ws_strict_handshake(&req, &policy);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_ws_strict_handshake_rejects_invalid_version() {
        let policy = ProxyPolicy {
            strict_http_semantics: true,
            ..Default::default()
        };
        let req = Request::builder()
            .method("GET")
            .uri("ws://example.com/socket")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "test-key")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "12")
            .body(Empty::<Bytes>::new())
            .expect("request build");
        let result = validate_ws_strict_handshake(&req, &policy);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_ws_strict_handshake_accepts_valid_request() {
        let policy = ProxyPolicy {
            strict_http_semantics: true,
            ..Default::default()
        };
        let req = Request::builder()
            .method("GET")
            .uri("ws://example.com/socket")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "test-key")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .body(Empty::<Bytes>::new())
            .expect("request build");
        let result = validate_ws_strict_handshake(&req, &policy);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_ws_strict_handshake_skips_when_disabled() {
        let policy = ProxyPolicy {
            strict_http_semantics: false,
            ..Default::default()
        };
        let req = Request::builder()
            .method("GET")
            .uri("ws://example.com/socket")
            .body(Empty::<Bytes>::new())
            .expect("request build");
        let result = validate_ws_strict_handshake(&req, &policy);
        assert!(result.is_ok());
    }
}
