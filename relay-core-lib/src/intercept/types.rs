use async_trait::async_trait;
use bytes::Bytes;
use http::Response;
use http_body_util::combinators::BoxBody;
use relay_core_api::flow::{Flow, HttpRequest, HttpResponse, Layer, WebSocketMessage};
use std::net::SocketAddr;
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type HttpBody = BoxBody<Bytes, BoxError>;

tokio::task_local! {
    /// Task-local variable to store the engine index for the current task.
    /// This ensures that all script executions within the same request/flow
    /// are routed to the same script engine instance, avoiding thread migration issues.
    pub static ENGINE_INDEX: usize;
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: Uuid,
    pub client_addr: SocketAddr,
    pub server_addr: Option<SocketAddr>,
    pub tls_sni: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub duration_ms: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub flows_count: u64,
}

#[derive(Debug, Clone)]
pub enum ConnectAction {
    Allow,
    Drop { reason: String },
}

#[derive(Debug, Clone)]
pub enum InterceptionResult {
    Continue,
    Drop,
    MockResponse(HttpResponse),
    ModifiedRequest(HttpRequest),
    ModifiedResponse(HttpResponse),
    ModifiedMessage(WebSocketMessage),
}

#[derive(Debug)]
pub enum RequestAction {
    Continue(HttpBody),
    Drop,
    MockResponse(Response<HttpBody>),
}

#[derive(Debug)]
pub enum ResponseAction {
    Continue(HttpBody),
    Drop,
    ModifiedResponse(Response<HttpBody>),
}

#[derive(Debug)]
pub enum WebSocketMessageAction {
    Continue(WebSocketMessage),
    Drop,
}

#[async_trait]
pub trait Interceptor: Send + Sync {
    /// Called when a new connection is accepted, before any HTTP handling.
    /// Return ConnectAction::Drop to reject the connection.
    async fn on_connect(&self, _conn: &ConnectionInfo) -> ConnectAction {
        ConnectAction::Allow
    }

    /// Called when a connection is closed (after all flows complete or on drop).
    async fn on_disconnect(&self, _conn: &ConnectionInfo, _stats: &ConnectionStats) {}

    /// Called after request headers are parsed but before body is read.
    /// Allows early termination or modification of headers/URL.
    async fn on_request_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    /// Called after full request (including body) is received.
    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError>;

    /// Called after response headers are received from upstream but before body is read.
    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    /// Called after full response (including body) is received.
    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError>;

    async fn on_websocket_message(
        &self,
        flow: &mut Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError>;

    /// Called when a WebSocket handshake completes (connection established).
    async fn on_websocket_start(&self, _flow: &mut Flow) {}

    /// Called when a WebSocket connection closes normally.
    async fn on_websocket_end(&self, _flow: &mut Flow, _close_code: u16, _close_reason: &str) {}

    /// Called when a WebSocket connection encounters an error.
    async fn on_websocket_error(&self, _flow: &mut Flow, _error: &str) {}

    /// Called before a new UDP session is created.
    /// Return InterceptionResult::Drop to block the session entirely,
    /// or Continue to allow it. Default: allow all.
    async fn on_udp_session(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }
}

// Default implementation that does nothing
pub struct NoOpInterceptor;

#[async_trait]
impl Interceptor for NoOpInterceptor {
    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        Ok(WebSocketMessageAction::Continue(message))
    }
}

use std::sync::Arc;

pub struct CompositeInterceptor {
    interceptors: Vec<Arc<dyn Interceptor>>,
}

impl CompositeInterceptor {
    pub fn new(interceptors: Vec<Arc<dyn Interceptor>>) -> Self {
        Self { interceptors }
    }
}

#[async_trait]
impl Interceptor for CompositeInterceptor {
    async fn on_connect(&self, conn: &ConnectionInfo) -> ConnectAction {
        for interceptor in &self.interceptors {
            match interceptor.on_connect(conn).await {
                ConnectAction::Drop { reason } => return ConnectAction::Drop { reason },
                ConnectAction::Allow => {}
            }
        }
        ConnectAction::Allow
    }

    async fn on_disconnect(&self, conn: &ConnectionInfo, stats: &ConnectionStats) {
        for interceptor in &self.interceptors {
            interceptor.on_disconnect(conn, stats).await;
        }
    }

    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let mut final_result = InterceptionResult::Continue;
        for interceptor in &self.interceptors {
            match interceptor.on_request_headers(flow).await {
                InterceptionResult::Continue => {}
                InterceptionResult::ModifiedRequest(req) => {
                    // Update flow so next interceptor sees it
                    match &mut flow.layer {
                        Layer::Http(http) => http.request = req.clone(),
                        Layer::WebSocket(ws) => ws.handshake_request = req.clone(),
                        _ => {}
                    }
                    final_result = InterceptionResult::ModifiedRequest(req);
                }
                // Short-circuiting results
                InterceptionResult::Drop => return InterceptionResult::Drop,
                InterceptionResult::MockResponse(res) => {
                    return InterceptionResult::MockResponse(res);
                }
                InterceptionResult::ModifiedResponse(res) => {
                    return InterceptionResult::ModifiedResponse(res);
                }
                _ => {}
            }
        }
        final_result
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let mut current_body = body;

        for interceptor in &self.interceptors {
            match interceptor.on_request(flow, current_body).await? {
                RequestAction::Continue(new_body) => {
                    current_body = new_body;
                }
                RequestAction::Drop => return Ok(RequestAction::Drop),
                RequestAction::MockResponse(res) => return Ok(RequestAction::MockResponse(res)),
            }
        }
        Ok(RequestAction::Continue(current_body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let mut final_result = InterceptionResult::Continue;
        for interceptor in &self.interceptors {
            match interceptor.on_response_headers(flow).await {
                InterceptionResult::Continue => {}
                InterceptionResult::ModifiedResponse(res) => {
                    // Update flow so next interceptor sees it
                    match &mut flow.layer {
                        Layer::Http(http) => http.response = Some(res.clone()),
                        Layer::WebSocket(ws) => ws.handshake_response = res.clone(),
                        _ => {}
                    }
                    final_result = InterceptionResult::ModifiedResponse(res);
                }
                // Short-circuiting results
                InterceptionResult::Drop => return InterceptionResult::Drop,
                InterceptionResult::MockResponse(res) => {
                    return InterceptionResult::MockResponse(res);
                }
                _ => {}
            }
        }
        final_result
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let mut current_body = body;

        for interceptor in &self.interceptors {
            match interceptor.on_response(flow, current_body).await? {
                ResponseAction::Continue(new_body) => {
                    current_body = new_body;
                }
                ResponseAction::Drop => return Ok(ResponseAction::Drop),
                ResponseAction::ModifiedResponse(res) => {
                    return Ok(ResponseAction::ModifiedResponse(res));
                }
            }
        }
        Ok(ResponseAction::Continue(current_body))
    }

    async fn on_websocket_message(
        &self,
        flow: &mut Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        let mut current_message = message;
        for interceptor in &self.interceptors {
            match interceptor
                .on_websocket_message(flow, current_message)
                .await?
            {
                WebSocketMessageAction::Continue(msg) => {
                    current_message = msg;
                }
                WebSocketMessageAction::Drop => return Ok(WebSocketMessageAction::Drop),
            }
        }
        Ok(WebSocketMessageAction::Continue(current_message))
    }

    async fn on_websocket_start(&self, flow: &mut Flow) {
        for interceptor in &self.interceptors {
            interceptor.on_websocket_start(flow).await;
        }
    }

    async fn on_websocket_end(&self, flow: &mut Flow, close_code: u16, close_reason: &str) {
        for interceptor in &self.interceptors {
            interceptor
                .on_websocket_end(flow, close_code, close_reason)
                .await;
        }
    }

    async fn on_websocket_error(&self, flow: &mut Flow, error: &str) {
        for interceptor in &self.interceptors {
            interceptor.on_websocket_error(flow, error).await;
        }
    }

    async fn on_udp_session(&self, flow: &mut Flow) -> InterceptionResult {
        for interceptor in &self.interceptors {
            match interceptor.on_udp_session(flow).await {
                InterceptionResult::Drop => return InterceptionResult::Drop,
                InterceptionResult::Continue => {}
                other => {
                    tracing::warn!(
                        "Unexpected intercept result for UDP: {:?}, treating as Continue",
                        other
                    );
                }
            }
        }
        InterceptionResult::Continue
    }
}
