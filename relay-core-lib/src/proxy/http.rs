use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc::Sender, watch};

use crate::capture::loop_detection::LoopDetector;
use crate::interceptor::{
    BoxError, HttpBody, InterceptionResult, Interceptor, RequestAction, ResponseAction,
};
use crate::proxy::body_plan::{
    buffer_body_within_budget, headers_for_direction, record_decoded_body_on_flow,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::http_utils::{
    body_data_to_bytes, build_client_response_head, build_forward_request,
    build_request_body_from_flow, create_error_response, create_initial_flow, mock_to_response,
    parse_request_meta, reframe_response_headers_for_replaced_body, request_body_from_flow_len,
    update_flow_with_response_headers,
};
use crate::proxy::outbound::OutboundConnector;
use crate::proxy::tap::TapBody;
use crate::proxy::tunnel;
use crate::proxy::websocket::handle_websocket_handshake;
use crate::tls::CertificateAuthority;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use relay_core_api::flow::{Direction, FlowUpdate, Layer, ResilienceTrace};
use relay_core_api::policy::ProxyPolicy;

/// Main entry point for HTTP Proxy handling
#[allow(clippy::too_many_arguments)]
pub async fn handle_request(
    req: Request<Incoming>,
    client_addr: SocketAddr,
    on_flow: Sender<FlowUpdate>,
    ca: Arc<CertificateAuthority>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    target_addr: Option<SocketAddr>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
) -> Result<Response<HttpBody>, Infallible> {
    if req.method() == Method::CONNECT {
        // Handle CONNECT (HTTPS Tunnel)
        // Extract host from authority
        let host = if let Some(authority) = req.uri().authority() {
            authority.to_string()
        } else {
            // Fallback: try to get from Host header
            req.headers()
                .get("Host")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        };

        if host == "unknown" {
            return Ok(create_error_response(
                StatusCode::BAD_REQUEST,
                "CONNECT must have authority",
            ));
        }

        let loop_detector = loop_detector.clone();
        let policy_rx = policy_rx.clone();

        tokio::task::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    if let Err(e) = tunnel::handle_tunnel(
                        upgraded,
                        host,
                        client_addr,
                        ca,
                        on_flow,
                        connector,
                        interceptor,
                        policy_rx,
                        target_addr,
                        loop_detector,
                        circuit_breaker,
                    )
                    .await
                    {
                        tracing::error!("Tunnel error: {}", e);
                    }
                }
                Err(e) => tracing::error!("Upgrade error: {}", e),
            }
        });
        return Ok(Response::new(
            Full::new(Bytes::new()).map_err(|e| e.into()).boxed(),
        ));
    }

    // Handle Standard HTTP / WebSocket
    handle_http_request(
        req,
        client_addr,
        on_flow,
        connector,
        interceptor,
        false,
        policy_rx,
        target_addr,
        loop_detector,
        circuit_breaker,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_http_request<B>(
    req: Request<B>,
    client_addr: SocketAddr,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    is_mitm: bool,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
) -> Result<Response<HttpBody>, Infallible>
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send + Into<Bytes>,
    B::Error: Into<BoxError>,
{
    let policy = policy_rx.borrow().clone();

    // P1a: Track oversized requests for streaming-first pipeline.
    // Instead of hard-failing with PAYLOAD_TOO_LARGE, we allow the request
    // through and mark budget_exceeded so rules that need full body are skipped.
    let request_budget_exceeded = if let Some(cl) = req.headers().get(hyper::header::CONTENT_LENGTH)
        && let Ok(len) = cl.to_str().unwrap_or_default().parse::<usize>()
        && len > policy.max_body_size
    {
        true
    } else {
        false
    };

    // Create Flow
    let meta = parse_request_meta(&req, is_mitm);

    // Note: We don't read body here for streaming support
    let mut flow = create_initial_flow(meta, None, client_addr, is_mitm, false);

    // P1a: Mark budget exceeded for oversized requests
    if request_budget_exceeded {
        flow.tags.push("budget-exceeded".to_string());
        flow.resilience_trace = Some(ResilienceTrace {
            budget_exceeded: true,
            ..flow.resilience_trace.clone().unwrap_or_default()
        });
    }

    // Check for WebSocket
    if hyper_tungstenite::is_upgrade_request(&req) {
        return handle_websocket_handshake(
            req,
            client_addr,
            on_flow,
            connector,
            interceptor,
            is_mitm,
            policy_rx,
            target_addr,
            loop_detector,
        )
        .await;
    }

    if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
        tracing::error!("Failed to send flow update: {}", e);
    }

    // Phase 1: Request Headers Interception
    match interceptor.on_request_headers(&mut flow).await {
        InterceptionResult::Continue => {}
        InterceptionResult::Drop => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on drop: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::FORBIDDEN,
                "Request dropped by policy",
            ));
        }
        InterceptionResult::MockResponse(resp) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on mock: {}", e);
            }
            return Ok(mock_to_response(resp));
        }
        InterceptionResult::ModifiedRequest(_) => {}
        InterceptionResult::ModifiedResponse(res) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on modified response: {}", e);
            }
            return Ok(mock_to_response(res));
        }
        _ => {}
    }

    // Phase 2: Request Body Streaming & Interception
    let (_, body) = req.into_parts();
    let body: HttpBody = body
        .map_frame(|f| f.map_data(|d| d.into()))
        .map_err(|e| e.into())
        .boxed();

    // Wrap in TapBody for streaming visualization BEFORE interception
    let req_headers = if let Layer::Http(http) = &flow.layer {
        http.request.headers.clone()
    } else {
        vec![]
    };

    let tap_body = TapBody::new(
        body,
        flow.id.to_string(),
        on_flow.clone(),
        Direction::ClientToServer,
        policy.max_body_size,
        req_headers,
    );
    crate::metrics::inc_proxy_http_request();
    let mut current_body = tap_body.boxed();

    match interceptor.on_request(&mut flow, current_body).await {
        Ok(RequestAction::Continue(new_body)) => {
            current_body = new_body;
        }
        Ok(RequestAction::Drop) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on request drop: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::FORBIDDEN,
                "Request dropped by interceptor",
            ));
        }
        Ok(RequestAction::MockResponse(res)) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on request mock: {}", e);
            }
            let (parts, body) = res.into_parts();
            return Ok(Response::from_parts(parts, body));
        }
        Err(e) => {
            tracing::error!("Interceptor error on_request: {}", e);
            return Ok(create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Interceptor Error: {}", e),
            ));
        }
    }

    // RE2: Apply ThrottleBody if a Throttle rule set the rate in flow.meta
    if let Some(bps_str) = flow.meta.get("throttle_bytes_per_sec")
        && let Ok(bps) = bps_str.parse::<u64>()
        && bps > 0
    {
        current_body = crate::proxy::throttle::ThrottleBody::new(current_body, bps).boxed();
    }

    // Convergence: when an interceptor replaced the request body, the Flow holds the authoritative
    // bytes. Materialize them and reframe, instead of forwarding the untouched client stream.
    // A body nobody replaced keeps streaming — buffering it would cost the streaming property
    // for nothing (roadmap §22).
    let flow_request_body = match request_body_from_flow_len(&flow) {
        Some(_) => match &flow.layer {
            Layer::Http(http) => http.request.body.clone(),
            _ => None,
        },
        None => None,
    };
    let body_replaced = flow_request_body.is_some();
    if let Some(body_data) = &flow_request_body {
        current_body = build_request_body_from_flow(body_data);
    }

    let forward_req = match build_forward_request(
        &mut flow,
        current_body,
        target_addr,
        &policy,
        &loop_detector,
        body_replaced,
    ) {
        Ok(req) => req,
        Err(res) => return Ok(res),
    };

    // P3: Circuit breaker check before upstream request.
    // When going through an upstream proxy, key on the proxy address so
    // that a failing upstream proxy isolates correctly from target hosts.
    let circuit_breaker_key = connector
        .upstream_proxy_url()
        .map(|u| u.to_string())
        .unwrap_or_else(|| {
            forward_req
                .uri()
                .authority()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });
    if !circuit_breaker.allow_request(&circuit_breaker_key).await {
        tracing::warn!(
            "Circuit breaker open for upstream {}, returning 503",
            circuit_breaker_key
        );
        // P4: Record circuit breaker open in resilience trace
        flow.resilience_trace = Some(ResilienceTrace {
            circuit_open: true,
            ..flow.resilience_trace.clone().unwrap_or_default()
        });
        // The exchange is finishing: record it so duration_ms can be computed.
        flow.end_time = Some(chrono::Utc::now());
        if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
            tracing::error!("Failed to send flow update on circuit breaker: {}", e);
        }
        return Ok(create_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Circuit breaker open for upstream {}", circuit_breaker_key),
        ));
    }

    // Send Request
    let upstream_start = std::time::Instant::now();
    let target_host = forward_req.uri().host().unwrap_or("unknown").to_string();
    let target_port = forward_req.uri().port_u16().unwrap_or(
        if forward_req.uri().scheme_str() == Some("https") {
            443
        } else {
            80
        },
    );
    let res = match tokio::time::timeout(
        std::time::Duration::from_millis(policy.request_timeout_ms),
        connector.send_request(forward_req, &target_host, target_port, &mut flow),
    )
    .await
    {
        Ok(Ok(res)) => {
            circuit_breaker.record_success(&circuit_breaker_key).await;
            res
        }
        Ok(Err(e)) => {
            circuit_breaker.record_failure(&circuit_breaker_key).await;
            tracing::error!("Upstream request failed: {}", e);
            // P4: Record upstream error in resilience trace
            flow.resilience_trace = Some(ResilienceTrace {
                upstream_errors: vec![format!("Upstream Error: {}", e)],
                ..flow.resilience_trace.clone().unwrap_or_default()
            });
            if let Layer::Http(http) = &mut flow.layer {
                http.error = Some(format!("Upstream Error: {}", e));
            }
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on upstream error: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::BAD_GATEWAY,
                format!("Upstream Error: {}", e),
            ));
        }
        Err(_) => {
            circuit_breaker.record_failure(&circuit_breaker_key).await;
            tracing::error!("Upstream request timed out");
            // Record timeout in resilience trace
            // We use a single tokio::time::timeout wrapping the entire upstream
            // request, so we cannot reliably distinguish connect vs read.
            // Mark as "total" rather than guessing.
            flow.resilience_trace = Some(ResilienceTrace {
                upstream_errors: vec!["Upstream Request Timed Out".to_string()],
                timeout_type: Some("total".to_string()),
                ..flow.resilience_trace.clone().unwrap_or_default()
            });
            if let Layer::Http(http) = &mut flow.layer {
                http.error = Some("Upstream Request Timed Out".to_string());
            }
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on upstream timeout: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "Upstream Request Timed Out",
            ));
        }
    };

    // Phase 3: Response Headers Interception
    let (mut res_parts, res_body) = res.into_parts();

    let mut res_body: HttpBody = res_body
        .map_frame(|f| f.map_data(|d| d))
        .map_err(|e| e.into())
        .boxed();

    // Apply QUIC Downgrade
    apply_quic_downgrade(&mut res_parts, &mut flow, &policy);

    update_flow_with_response_headers(
        &mut flow,
        res_parts.status,
        res_parts.version,
        &res_parts.headers,
    );

    // Decide the response body's plan from two explicit inputs: whether a stage will inspect it
    // (declared by the rule engine during the request phase, because at this header moment it is too
    // late to retain anything) and how much the host wants to observe. `PassThrough` keeps
    // streaming, so an exchange nobody inspects and nobody displays pays nothing (roadmap §22).
    let response_body_plan =
        relay_core_api::body_plan::decide(relay_core_api::body_plan::BodyPlanInputs {
            has_body_stage_rules: crate::rule::stage_guard::response_body_budget(&flow).is_some(),
            has_body_hook_script: false,
            has_body_intercept: false,
            observation: policy.body_observation,
            budget: crate::rule::stage_guard::response_body_budget(&flow)
                .unwrap_or(policy.rule_body_inspect_budget),
        });

    if let relay_core_api::body_plan::BodyPlan::Buffer { limit: budget } = response_body_plan {
        let taken = std::mem::replace(
            &mut res_body,
            Full::new(Bytes::new())
                .map_err(|e| -> BoxError { e.into() })
                .boxed(),
        );
        match buffer_body_within_budget(taken, budget).await {
            Ok((snapshot, forwarded)) => {
                if snapshot.truncated {
                    flow.tags.push("rule_skipped:body_truncated".to_string());
                } else {
                    let headers = headers_for_direction(&flow, Direction::ServerToClient);
                    // Record decoded: the response body stage runs immediately after this, and a
                    // filter on a gzip/br/zstd body must see plaintext (roadmap §24.3).
                    record_decoded_body_on_flow(
                        &mut flow,
                        Direction::ServerToClient,
                        &snapshot.bytes,
                        snapshot.total_bytes,
                        &headers,
                    );
                    // Tell later interceptors the body is already on the flow.
                    crate::rule::stage_guard::mark_body_captured(&mut flow);
                }
                res_body = forwarded;
            }
            Err(e) => {
                // Never let inspection failure break traffic; the bytes were consumed while being
                // read, so the honest outcome is an empty body plus a warning.
                tracing::warn!("Failed to retain response body for rule inspection: {}", e);
            }
        }
    }

    let ttfbs_ms = upstream_start.elapsed().as_millis() as u64;
    if let Layer::Http(http) = &mut flow.layer
        && let Some(response) = &mut http.response
    {
        response.timing.time_to_first_byte = Some(ttfbs_ms);
    }

    match interceptor.on_response_headers(&mut flow).await {
        InterceptionResult::Continue => {}
        InterceptionResult::Drop => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on response drop: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::FORBIDDEN,
                "Response dropped by policy",
            ));
        }
        InterceptionResult::MockResponse(resp) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on response mock: {}", e);
            }
            return Ok(mock_to_response(resp));
        }
        InterceptionResult::ModifiedResponse(resp) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on response modification: {}", e);
            }
            return Ok(mock_to_response(resp));
        }
        _ => {}
    }

    // Phase 4: Response Body Streaming & Interception (body boxed and retained in Phase 3)
    // Wrap in TapBody for streaming visualization BEFORE interception
    let res_headers = if let Layer::Http(http) = &flow.layer {
        http.response
            .as_ref()
            .map(|r| r.headers.clone())
            .unwrap_or_default()
    } else {
        vec![]
    };

    let tap_res_body = TapBody::new(
        res_body,
        flow.id.to_string(),
        on_flow.clone(),
        Direction::ServerToClient,
        policy.max_body_size,
        res_headers,
    );
    let mut current_res_body = tap_res_body.boxed();

    match interceptor.on_response(&mut flow, current_res_body).await {
        Ok(ResponseAction::Continue(new_body)) => {
            current_res_body = new_body;
        }
        Ok(ResponseAction::Drop) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on response body drop: {}", e);
            }
            return Ok(create_error_response(
                StatusCode::FORBIDDEN,
                "Response dropped by interceptor",
            ));
        }
        Ok(ResponseAction::ModifiedResponse(res)) => {
            // The exchange is finishing: record it so duration_ms can be computed.
            flow.end_time = Some(chrono::Utc::now());
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!(
                    "Failed to send flow update on response body modification: {}",
                    e
                );
            }
            let (parts, body) = res.into_parts();
            return Ok(Response::from_parts(parts, body));
        }
        Err(e) => {
            tracing::error!("Interceptor error on_response: {}", e);
            return Ok(create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Interceptor Error: {}", e),
            ));
        }
    }

    // RE2: Apply ThrottleBody to response if a Throttle rule set the rate in flow.meta
    if let Some(bps_str) = flow.meta.get("throttle_bytes_per_sec")
        && let Ok(bps) = bps_str.parse::<u64>()
        && bps > 0
    {
        current_res_body = crate::proxy::throttle::ThrottleBody::new(current_res_body, bps).boxed();
    }

    // Convergence: the Flow is the single source of truth for the response head, so mutations made
    // by any interceptor reach the client.
    let mut res_parts = build_client_response_head(&flow, &res_parts);

    // A replaced response body makes the Flow authoritative for the body too: discard the upstream
    // stream and reframe, exactly as the request direction does.
    if let Some(body_data) = match &flow.layer {
        Layer::Http(http) => http.response.as_ref().and_then(|r| r.body.clone()),
        _ => None,
    } {
        // The replacement is plaintext authored by a rule/script/interceptor, while the upstream
        // reply may have been compressed. Re-encode to match what the client expects where we can,
        // and otherwise drop the header — never send plaintext while claiming it is compressed.
        let original_encoding = res_parts
            .headers
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let plain = body_data_to_bytes(Some(&body_data));
        let (wire_bytes, encoding) = crate::proxy::content_encoding::encode_after_rewrite(
            &plain,
            original_encoding.as_deref(),
        );
        if encoding.is_none() && original_encoding.is_some() {
            flow.tags.push("body-encoding-dropped".to_string());
        }

        let new_len = wire_bytes.len();
        current_res_body = Full::new(Bytes::from(wire_bytes))
            .map_err(|e| -> BoxError { e.into() })
            .boxed();

        // Rebuild the head so framing describes the replacement rather than the upstream stream.
        let reframed = reframe_response_headers_for_replaced_body(
            &res_parts
                .headers
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        String::from_utf8_lossy(v.as_bytes()).to_string(),
                    )
                })
                .collect::<Vec<_>>(),
            new_len,
        );
        let mut headers = hyper::HeaderMap::new();
        for (k, v) in &reframed {
            if let (Ok(name), Ok(value)) = (
                hyper::header::HeaderName::from_bytes(k.as_bytes()),
                hyper::header::HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        }
        // State the encoding actually applied, if any.
        if let Some(encoding) = encoding
            && let Ok(value) = hyper::header::HeaderValue::from_str(&encoding)
        {
            headers.insert(hyper::header::CONTENT_ENCODING, value);
        }
        res_parts.headers = headers;
    }

    // Record time-to-last-byte as total upstream-to-client latency
    if let Layer::Http(http) = &mut flow.layer
        && let Some(response) = &mut http.response
    {
        response.timing.time_to_last_byte = Some(upstream_start.elapsed().as_millis() as u64);
    }

    // Mark the exchange complete. Without this, `FlowSummary.duration_ms` — derived solely from
    // `end_time` — was always null, and consumers could not distinguish a finished exchange from a
    // stalled one. It is set after the timing fields so the emitted flow carries the full record.
    flow.end_time = Some(chrono::Utc::now());
    if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
        tracing::error!("Failed to send final flow update: {}", e);
    }

    Ok(Response::from_parts(res_parts, current_res_body))
}

pub(crate) fn apply_quic_downgrade(
    parts: &mut hyper::http::response::Parts,
    flow: &mut relay_core_api::flow::Flow,
    policy: &ProxyPolicy,
) {
    use relay_core_api::policy::QuicMode;
    if policy.quic_mode == QuicMode::Downgrade {
        if parts.headers.remove("Alt-Svc").is_some() {
            flow.tags.push("quic-downgraded".to_string());
        }
        if policy.quic_downgrade_clear_cache {
            parts.headers.insert(
                "Clear-Site-Data",
                hyper::header::HeaderValue::from_static("\"cache\""),
            );
        }
    }
}

#[cfg(test)]
mod http_tests;
