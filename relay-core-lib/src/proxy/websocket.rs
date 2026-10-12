use crate::capture::loop_detection::LoopDetector;
use crate::intercept::types::{
    BoxError, HttpBody, InterceptionResult, Interceptor, RequestAction, WebSocketMessageAction,
};
use crate::proxy::http_utils::{
    ExchangeOrigin, coalesce_cookie_headers, create_error_response, create_initial_flow,
    mock_to_response, parse_request_meta, update_flow_with_response_headers,
};
use crate::proxy::outbound::OutboundConnector;
use chrono::Utc;
use data_encoding::BASE64;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::upgrade::Upgraded;
use hyper::{Request, Response, StatusCode};
use relay_core_api::event::CloseReason;
use relay_core_api::flow::{
    BodyData, Direction, ExchangeStage, Flow, FlowUpdate, HttpResponse, Layer, WebSocketMessage,
};
use relay_core_api::policy::ProxyPolicy;

/// Declare one stage of the handshake's life, from the place that reached it (roadmap §4-4).
async fn declare_stage(
    on_flow: &tokio::sync::mpsc::Sender<FlowUpdate>,
    flow: &Flow,
    stage: ExchangeStage,
) {
    if let Err(e) = on_flow
        .send(FlowUpdate::Stage {
            flow_id: flow.id.to_string(),
            stage,
            at: Utc::now(),
        })
        .await
    {
        tracing::debug!("could not declare stage {stage:?} for {}: {e}", flow.id);
    }
}

/// Tag placed on a WebSocket flow whose extension offer was removed.
pub const WS_EXTENSIONS_STRIPPED_TAG: &str = "ws-extensions-stripped";

/// Handshake headers this engine removes because it cannot honour what they negotiate.
///
/// Deliberately a list of one: the policy is "strip what changes framing", and today that is exactly
/// the extension header. Keeping it a predicate puts the rule in one place, so an extension this
/// engine later implements is supported by removing an entry rather than by hunting down string
/// comparisons in two loops.
fn is_downgraded_handshake_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("sec-websocket-extensions")
}
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
    origin: ExchangeOrigin,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
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
    let mut flow = create_initial_flow(meta.clone(), None, client_addr, is_mitm, true, &origin);

    // The handshake's request head is known here, and the 101 below is this engine's answer, so both
    // stages are declared from this path rather than inferred from the flow's snapshots.
    declare_stage(&on_flow, &flow, ExchangeStage::Started).await;
    declare_stage(&on_flow, &flow, ExchangeStage::RequestHeaders).await;

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

    // Determine Target URL from the Flow, which is authoritative: an interceptor may have rewritten
    // the handshake URL (`Action::MapRemote`), and reading the original request metadata ignored
    // that rewrite.
    let mut target_url_str = match &flow.layer {
        Layer::WebSocket(ws) => ws.handshake_request.url.to_string(),
        _ => meta.url_str.clone(),
    };

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

    // WebSocket extensions change how every frame after the handshake is framed, and this engine
    // does not implement any of them. Forwarding the client's offer would make the *upstream* accept
    // `permessage-deflate`, and the client would then see it in our 101 — so both peers would believe
    // the extension was negotiated end to end while the tunnel simply copies bytes. That is the worst
    // of the three outcomes roadmap §7.1 allows (support / pass through / explicit downgrade): the
    // client's messages would arrive compressed with the RSV1 bit set, unreadable to the frame
    // inspection that is the point of this proxy, and, because `permessage-deflate` keeps an LZ77
    // window across messages by default, two independently negotiated contexts relayed verbatim can
    // produce frames neither side can decompress.
    //
    // So the extension is stripped in both directions — from the request we forward, and from the 101
    // we return — which is the downgrade: neither peer is told the extension is available, so both
    // speak uncompressed. The removal is not silent; it is tagged on the flow and counted, for the
    // same reason `body-encoding-dropped` is (§24.9): a client that quietly stopped compressing
    // should be discoverable from the capture rather than only from a packet trace.
    //
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

    let mut stripped_extensions = false;
    for (k, v) in coalesce_cookie_headers(&current_req.headers) {
        if is_downgraded_handshake_header(&k) {
            // Only `Sec-WebSocket-Extensions` is stripped. `Sec-WebSocket-Protocol` is *not*: a
            // subprotocol does not change framing, so passing it through is what keeps an app's own
            // protocol working — stripping it would break every client that asked for one.
            stripped_extensions = true;
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            forward_req_builder = forward_req_builder.header(name, val);
        }
    }
    if stripped_extensions {
        // Tag rather than a log line: a tag travels with the flow into every adapter and the store,
        // while a log line is only visible to whoever happened to be reading the log.
        flow.tags.push(WS_EXTENSIONS_STRIPPED_TAG.to_string());
        crate::metrics::inc_proxy_ws_extensions_stripped();
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

    let target_host = forward_req.uri().host().unwrap_or("unknown").to_string();
    let port = forward_req.uri().port_u16().unwrap_or(
        if forward_req.uri().scheme_str() == Some("https") {
            443
        } else {
            80
        },
    );

    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        connector.send_request(forward_req, &target_host, port, &mut flow),
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

                // Record the upstream handshake response on the live Flow so it is observable,
                // then let interceptors act on it. This is the only point where the WS path can
                // run the response-header stage; it previously skipped it entirely and built the
                // client's 101 straight from the upstream Parts, so WS handshake response-header
                // mutations could never reach the client.
                update_flow_with_response_headers(
                    &mut flow,
                    parts.status,
                    parts.version,
                    &parts.headers,
                );
                declare_stage(&on_flow, &flow, ExchangeStage::ResponseHeaders).await;

                match interceptor.on_response_headers(&mut flow).await {
                    InterceptionResult::Drop => {
                        if on_flow
                            .try_send(FlowUpdate::Full(Box::new(flow.clone())))
                            .is_err()
                        {
                            crate::metrics::inc_flows_dropped();
                        }
                        return Ok(create_error_response(
                            StatusCode::FORBIDDEN,
                            "WebSocket handshake response dropped by policy",
                        ));
                    }
                    InterceptionResult::MockResponse(mock) => {
                        if on_flow
                            .try_send(FlowUpdate::Full(Box::new(flow.clone())))
                            .is_err()
                        {
                            crate::metrics::inc_flows_dropped();
                        }
                        return Ok(mock_to_response(mock));
                    }
                    InterceptionResult::ModifiedResponse(resp) => {
                        if on_flow
                            .try_send(FlowUpdate::Full(Box::new(flow.clone())))
                            .is_err()
                        {
                            crate::metrics::inc_flows_dropped();
                        }
                        return Ok(mock_to_response(resp));
                    }
                    _ => {}
                }

                // Build the client's 101 from the Flow so interceptor changes are what the client
                // sees. Framing headers are omitted: this response carries no body, and the upgrade
                // continues on the raw socket.
                let handshake = match &flow.layer {
                    Layer::WebSocket(ws) => Some(&ws.handshake_response),
                    _ => None,
                };
                let status = handshake
                    .and_then(|h| StatusCode::from_u16(h.status).ok())
                    .unwrap_or(StatusCode::SWITCHING_PROTOCOLS);

                let mut client_resp_builder =
                    Response::builder().status(status).version(parts.version);

                if let Some(handshake) = handshake {
                    for (k, v) in &handshake.headers {
                        if k.eq_ignore_ascii_case("content-length")
                            || k.eq_ignore_ascii_case("transfer-encoding")
                        {
                            continue;
                        }
                        // Never tell the client an extension is on when this engine will relay frames
                        // verbatim: an upstream that offers it unprompted would otherwise leave the
                        // client compressing into a tunnel that cannot read it.
                        if is_downgraded_handshake_header(k) {
                            continue;
                        }
                        if let (Ok(name), Ok(val)) = (
                            HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(v),
                        ) {
                            client_resp_builder = client_resp_builder.header(name, val);
                        }
                    }
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

                if on_flow
                    .try_send(FlowUpdate::Full(Box::new(flow.clone())))
                    .is_err()
                {
                    crate::metrics::inc_flows_dropped();
                }

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
                        grpc: None,
                    }),
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                    sse_events: vec![],
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
        // The handshake failed, so the session is over before it began — but the Flow was
        // dropped without ever being emitted, leaving the exchange invisible in the UI.
        Ok(Err(e)) => {
            let detail = format!("Upstream Handshake Failed: {}", e);
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.handshake_response.status = StatusCode::BAD_GATEWAY.as_u16();
                ws.handshake_response.status_text = "Bad Gateway".to_string();
                ws.closed = true;
            }
            report_websocket_end(&mut flow, &on_flow, CloseReason::UpstreamClosed);
            Ok(create_error_response(StatusCode::BAD_GATEWAY, detail))
        }
        Err(_) => {
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.handshake_response.status = StatusCode::GATEWAY_TIMEOUT.as_u16();
                ws.handshake_response.status_text = "Gateway Timeout".to_string();
                ws.closed = true;
            }
            report_websocket_end(
                &mut flow,
                &on_flow,
                CloseReason::Timeout {
                    kind: "handshake".to_string(),
                },
            );
            Ok(create_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "Upstream Handshake Timed Out",
            ))
        }
    }
}

/// Report that a WebSocket session is over.
///
/// The tunnel used to end silently: no closing Flow was ever emitted, so `WebSocketLayer.closed`
/// stayed `false` and `end_time` stayed `None`. A consumer could not tell a live session from a
/// finished one, and a finished session's duration was unknowable — while the HTTP path records
/// both at every one of its terminal sites.
fn report_websocket_end(flow: &mut Flow, on_flow: &Sender<FlowUpdate>, reason: CloseReason) {
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.closed = true;
    }
    flow.end_time = Some(chrono::Utc::now());
    flow.close_reason = Some(reason);
    if on_flow
        .try_send(FlowUpdate::Full(Box::new(flow.clone())))
        .is_err()
    {
        crate::metrics::inc_flows_dropped();
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

    // Which side ended the stream, when it ended cleanly. Every other exit (an error or the idle
    // timeout) reports its own reason before returning, so this is only assigned on the clean path.
    #[allow(unused_assignments)]
    let mut ended_by: Option<CloseReason> = None;

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
                                        report_websocket_end(
                                            &mut flow,
                                            &on_flow,
                                            CloseReason::Reset,
                                        );
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
                                report_websocket_end(&mut flow, &on_flow, CloseReason::Reset);
                                return Err(e.into());
                            }
                        }
                    }
                    Some(Err(e)) => {
                        interceptor
                            .on_websocket_error(&mut flow, &e.to_string())
                            .await;
                        report_websocket_end(
                            &mut flow,
                            &on_flow,
                            CloseReason::ParserError {
                                detail: e.to_string(),
                            },
                        );
                        return Err(e.into());
                    }
                    None => {
                        interceptor
                            .on_websocket_end(&mut flow, 1000, "normal")
                            .await;
                        // `dir` says which side ended the stream, so "who closed first" is known
                        // here rather than guessed. Both are normal endings, and 1000/"normal" is
                        // what the interceptor hook is told either way.
                        ended_by = Some(if dir == Direction::ClientToServer {
                            CloseReason::ClientClosed
                        } else {
                            CloseReason::UpstreamClosed
                        });
                        break;
                    }
                }
            }
            Err(_) => {
                tracing::warn!("WebSocket Tunnel Idle Timeout");
                interceptor
                    .on_websocket_error(&mut flow, "WebSocket Idle Timeout")
                    .await;
                report_websocket_end(
                    &mut flow,
                    &on_flow,
                    CloseReason::Timeout {
                        kind: "websocket_idle".to_string(),
                    },
                );
                return Err("WebSocket Idle Timeout".into());
            }
        }
    }

    // A clean close breaks out of the loop rather than returning, so the end is reported here too.
    let reason = ended_by.unwrap_or(CloseReason::Completed);
    report_websocket_end(&mut flow, &on_flow, reason);
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
            grpc: None,
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
