use relay_core_api::flow::{Flow, HttpRequest, HttpResponse, WebSocketMessage, Layer};
use async_trait::async_trait;
use http_body_util::combinators::BoxBody;
use bytes::Bytes;
use http::Response;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type HttpBody = BoxBody<Bytes, BoxError>;

tokio::task_local! {
    /// Task-local variable to store the engine index for the current task.
    /// This ensures that all script executions within the same request/flow
    /// are routed to the same script engine instance, avoiding thread migration issues.
    pub static ENGINE_INDEX: usize;
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
    async fn on_response(&self, flow: &mut Flow, body: HttpBody) -> Result<ResponseAction, BoxError>;

    async fn on_websocket_message(&self, flow: &mut Flow, message: WebSocketMessage) -> Result<WebSocketMessageAction, BoxError>;
}

// Default implementation that does nothing
pub struct NoOpInterceptor;

#[async_trait]
impl Interceptor for NoOpInterceptor {
    async fn on_request(&self, _flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response(&self, _flow: &mut Flow, body: HttpBody) -> Result<ResponseAction, BoxError> {
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(&self, _flow: &mut Flow, message: WebSocketMessage) -> Result<WebSocketMessageAction, BoxError> {
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
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let mut final_result = InterceptionResult::Continue;
        for interceptor in &self.interceptors {
            match interceptor.on_request_headers(flow).await {
                InterceptionResult::Continue => {},
                InterceptionResult::ModifiedRequest(req) => {
                    // Update flow so next interceptor sees it
                    match &mut flow.layer {
                        Layer::Http(http) => http.request = req.clone(),
                        Layer::WebSocket(ws) => ws.handshake_request = req.clone(),
                        _ => {}
                    }
                    final_result = InterceptionResult::ModifiedRequest(req);
                },
                // Short-circuiting results
                InterceptionResult::Drop => return InterceptionResult::Drop,
                InterceptionResult::MockResponse(res) => return InterceptionResult::MockResponse(res),
                InterceptionResult::ModifiedResponse(res) => return InterceptionResult::ModifiedResponse(res),
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
                },
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
                InterceptionResult::Continue => {},
                InterceptionResult::ModifiedResponse(res) => {
                    // Update flow so next interceptor sees it
                    match &mut flow.layer {
                        Layer::Http(http) => http.response = Some(res.clone()),
                        Layer::WebSocket(ws) => ws.handshake_response = res.clone(),
                        _ => {}
                    }
                    final_result = InterceptionResult::ModifiedResponse(res);
                },
                // Short-circuiting results
                InterceptionResult::Drop => return InterceptionResult::Drop,
                InterceptionResult::MockResponse(res) => return InterceptionResult::MockResponse(res),
                _ => {}
            }
        }
        final_result
    }

    async fn on_response(&self, flow: &mut Flow, body: HttpBody) -> Result<ResponseAction, BoxError> {
        let mut current_body = body;
        
        for interceptor in &self.interceptors {
            match interceptor.on_response(flow, current_body).await? {
                ResponseAction::Continue(new_body) => {
                    current_body = new_body;
                },
                ResponseAction::Drop => return Ok(ResponseAction::Drop),
                ResponseAction::ModifiedResponse(res) => return Ok(ResponseAction::ModifiedResponse(res)),
            }
        }
        Ok(ResponseAction::Continue(current_body))
    }

    async fn on_websocket_message(&self, flow: &mut Flow, message: WebSocketMessage) -> Result<WebSocketMessageAction, BoxError> {
        let mut current_message = message;
        for interceptor in &self.interceptors {
            match interceptor.on_websocket_message(flow, current_message).await? {
                WebSocketMessageAction::Continue(msg) => {
                    current_message = msg;
                },
                WebSocketMessageAction::Drop => return Ok(WebSocketMessageAction::Drop),
            }
        }
        Ok(WebSocketMessageAction::Continue(current_message))
    }
}
