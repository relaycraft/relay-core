use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc::Sender, watch};

use hyper::{Request, Response, Method, StatusCode};
use hyper::body::{Bytes, Incoming, Body};
use http_body_util::{BodyExt, Full};
use relay_core_api::flow::{FlowUpdate, Layer, Direction};
use relay_core_api::policy::ProxyPolicy;
use crate::interceptor::{Interceptor, InterceptionResult, RequestAction, ResponseAction, HttpBody, BoxError};
use crate::tls::CertificateAuthority;
use crate::proxy::http_utils::{
    create_initial_flow, 
    mock_to_response, parse_request_meta, create_error_response, HttpsClient,
    build_forward_request, update_flow_with_response_headers,
};
use crate::proxy::tunnel;
use crate::proxy::websocket::handle_websocket_handshake;
use crate::capture::loop_detection::LoopDetector;
use crate::proxy::tap::TapBody;

/// Main entry point for HTTP Proxy handling
#[allow(clippy::too_many_arguments)]
pub async fn handle_request(
    req: Request<Incoming>,
    client_addr: SocketAddr,
    on_flow: Sender<FlowUpdate>,
    ca: Arc<CertificateAuthority>,
    client: Arc<HttpsClient>,
    interceptor: Arc<dyn Interceptor>,
    target_addr: Option<SocketAddr>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    loop_detector: Arc<LoopDetector>,
) -> Result<Response<HttpBody>, Infallible>
{
    if req.method() == Method::CONNECT {
        // Handle CONNECT (HTTPS Tunnel)
        // Extract host from authority
        let host = if let Some(authority) = req.uri().authority() {
            authority.to_string()
        } else {
            // Fallback: try to get from Host header
             req.headers().get("Host")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        };

        if host == "unknown" {
             return Ok(create_error_response(StatusCode::BAD_REQUEST, "CONNECT must have authority"));
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
                        client,
                        interceptor,
                        policy_rx,
                        target_addr,
                        loop_detector,
                    ).await {
                        tracing::error!("Tunnel error: {}", e);
                    }
                },
                Err(e) => tracing::error!("Upgrade error: {}", e),
            }
        });
        return Ok(Response::new(Full::new(Bytes::new()).map_err(|e| e.into()).boxed()));
    }

    // Handle Standard HTTP / WebSocket
    handle_http_request(req, client_addr, on_flow, client, interceptor, false, policy_rx, target_addr, loop_detector).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_http_request<B>(
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
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send + Into<Bytes>,
    B::Error: Into<BoxError>,
{
    let policy = policy_rx.borrow().clone();
    
    // Check Content-Length against policy
    if let Some(cl) = req.headers().get(hyper::header::CONTENT_LENGTH)
        && let Ok(len) = cl.to_str().unwrap_or_default().parse::<usize>()
            && len > policy.max_body_size {
                return Ok(create_error_response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"));
            }

    // Create Flow
    let meta = parse_request_meta(&req, is_mitm);
    
    // Note: We don't read body here for streaming support
    let mut flow = create_initial_flow(meta, None, client_addr, is_mitm, false);
    
    // Check for WebSocket
    if hyper_tungstenite::is_upgrade_request(&req) {
        return handle_websocket_handshake(req, client_addr, on_flow, client, interceptor, is_mitm, policy_rx, target_addr, loop_detector).await;
    }

    if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
        tracing::error!("Failed to send flow update: {}", e);
    }

    // Phase 1: Request Headers Interception
    match interceptor.on_request_headers(&mut flow).await {
        InterceptionResult::Continue => {},
        InterceptionResult::Drop => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on drop: {}", e);
             }
             return Ok(create_error_response(StatusCode::FORBIDDEN, "Request dropped by policy"));
        },
        InterceptionResult::MockResponse(resp) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on mock: {}", e);
             }
             return Ok(mock_to_response(resp));
        },
        InterceptionResult::ModifiedRequest(_) => {},
        InterceptionResult::ModifiedResponse(res) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on modified response: {}", e);
             }
             return Ok(mock_to_response(res));
        },
        _ => {}
    }

    // Phase 2: Request Body Streaming & Interception
    let (_, body) = req.into_parts();
    let body: HttpBody = body.map_frame(|f| f.map_data(|d| d.into())).map_err(|e| e.into()).boxed();
    
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
    let mut current_body = tap_body.boxed();
    
    match interceptor.on_request(&mut flow, current_body).await {
        Ok(RequestAction::Continue(new_body)) => {
            current_body = new_body;
        },
        Ok(RequestAction::Drop) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on request drop: {}", e);
             }
             return Ok(create_error_response(StatusCode::FORBIDDEN, "Request dropped by interceptor"));
        },
        Ok(RequestAction::MockResponse(res)) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on request mock: {}", e);
             }
             let (parts, body) = res.into_parts();
             return Ok(Response::from_parts(parts, body));
        },
        Err(e) => {
             tracing::error!("Interceptor error on_request: {}", e);
             return Ok(create_error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Interceptor Error: {}", e)));
        }
    }
    
    let forward_req = match build_forward_request(&mut flow, current_body, target_addr, &policy, &loop_detector) {
        Ok(req) => req,
        Err(res) => return Ok(res),
    };
    
    // Send Request
    let res = match tokio::time::timeout(std::time::Duration::from_millis(policy.request_timeout_ms), client.request(forward_req)).await {
        Ok(Ok(res)) => res,
        Ok(Err(e)) => {
            tracing::error!("Upstream request failed: {}", e);
             if let Layer::Http(http) = &mut flow.layer {
                http.error = Some(format!("Upstream Error: {}", e));
            }
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on upstream error: {}", e);
            }
            return Ok(create_error_response(StatusCode::BAD_GATEWAY, format!("Upstream Error: {}", e)));
        },
        Err(_) => {
            tracing::error!("Upstream request timed out");
             if let Layer::Http(http) = &mut flow.layer {
                http.error = Some("Upstream Request Timed Out".to_string());
            }
            if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                tracing::error!("Failed to send flow update on upstream timeout: {}", e);
            }
            return Ok(create_error_response(StatusCode::GATEWAY_TIMEOUT, "Upstream Request Timed Out"));
        }
    };
    
    // Phase 3: Response Headers Interception
    let (mut res_parts, res_body) = res.into_parts();

    // Apply QUIC Downgrade
    apply_quic_downgrade(&mut res_parts, &mut flow, &policy);
    
    update_flow_with_response_headers(&mut flow, res_parts.status, res_parts.version, &res_parts.headers);
    
    match interceptor.on_response_headers(&mut flow).await {
        InterceptionResult::Continue => {},
        InterceptionResult::Drop => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on response drop: {}", e);
             }
             return Ok(create_error_response(StatusCode::FORBIDDEN, "Response dropped by policy"));
        },
        InterceptionResult::MockResponse(resp) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on response mock: {}", e);
             }
             return Ok(mock_to_response(resp));
        },
        InterceptionResult::ModifiedResponse(resp) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on response modification: {}", e);
             }
             return Ok(mock_to_response(resp));
        },
        _ => {}
    }
    
    // Phase 4: Response Body Streaming & Interception
    let res_body: HttpBody = res_body.map_frame(|f| f.map_data(|d| d)).map_err(|e| e.into()).boxed();
    
    // Wrap in TapBody for streaming visualization BEFORE interception
    let res_headers = if let Layer::Http(http) = &flow.layer {
        http.response.as_ref().map(|r| r.headers.clone()).unwrap_or_default()
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
        },
        Ok(ResponseAction::Drop) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on response body drop: {}", e);
             }
             return Ok(create_error_response(StatusCode::FORBIDDEN, "Response dropped by interceptor"));
        },
        Ok(ResponseAction::ModifiedResponse(res)) => {
             if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
                 tracing::error!("Failed to send flow update on response body modification: {}", e);
             }
             let (parts, body) = res.into_parts();
             return Ok(Response::from_parts(parts, body));
        },
        Err(e) => {
             tracing::error!("Interceptor error on_response: {}", e);
             return Ok(create_error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Interceptor Error: {}", e)));
        }
    }

    if let Err(e) = on_flow.send(FlowUpdate::Full(Box::new(flow.clone()))).await {
        tracing::error!("Failed to send final flow update: {}", e);
    }
    
    Ok(Response::from_parts(res_parts, current_res_body))
}

pub(crate) fn apply_quic_downgrade(parts: &mut hyper::http::response::Parts, flow: &mut relay_core_api::flow::Flow, policy: &ProxyPolicy) {
    use relay_core_api::policy::QuicMode;
    if policy.quic_mode == QuicMode::Downgrade {
         if parts.headers.remove("Alt-Svc").is_some() {
             flow.tags.push("quic-downgraded".to_string());
         }
         if policy.quic_downgrade_clear_cache {
             parts.headers.insert("Clear-Site-Data", hyper::header::HeaderValue::from_static("\"cache\""));
        }
    }
}

#[cfg(test)]
mod http_tests;
