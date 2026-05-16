use std::sync::Arc;
use async_trait::async_trait;
use relay_core_lib::intercept::{
    Interceptor, 
    InterceptionResult, 
    RequestAction, 
    ResponseAction, 
    WebSocketMessageAction,
    HttpBody, 
    BoxError
};
use relay_core_api::flow::{Flow, Layer, HttpResponse, ResponseTiming, BodyData};
use crate::CoreState;

pub struct MetricsInterceptor {
    state: Arc<CoreState>,
}

impl MetricsInterceptor {
    pub fn new(state: Arc<CoreState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Interceptor for MetricsInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        if let Layer::Http(ref mut http) = flow.layer {
            if http.request.url.path() == "/_relay/metrics/prometheus" {
                let text = self.state.get_metrics_prometheus_text().await;

                let body_data = BodyData {
                    encoding: "utf-8".to_string(),
                    content: text.clone(),
                    size: text.len() as u64,
                };

                let response = HttpResponse {
                    status: 200,
                    status_text: "OK".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![
                        (
                            "Content-Type".to_string(),
                            "text/plain; version=0.0.4; charset=utf-8".to_string(),
                        ),
                        ("Access-Control-Allow-Origin".to_string(), "*".to_string()),
                    ],
                    cookies: vec![],
                    body: Some(body_data),
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                    },
                };

                return InterceptionResult::MockResponse(response);
            }

            if http.request.url.path() == "/_relay/metrics" {
                let metrics = self.state.get_metrics().await;
                let json = serde_json::to_string(&metrics).unwrap_or_default();
                
                let body_data = BodyData {
                    encoding: "utf-8".to_string(),
                    content: json.clone(),
                    size: json.len() as u64,
                };
                
                let response = HttpResponse {
                    status: 200,
                    status_text: "OK".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![
                        ("Content-Type".to_string(), "application/json".to_string()),
                        ("Access-Control-Allow-Origin".to_string(), "*".to_string()),
                    ],
                    cookies: vec![],
                    body: Some(body_data),
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                    },
                };
                
                return InterceptionResult::MockResponse(response);
            }
        }
        InterceptionResult::Continue
    }

    async fn on_request(&self, _flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response(&self, _flow: &mut Flow, body: HttpBody) -> Result<ResponseAction, BoxError> {
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(&self, _flow: &mut Flow, message: relay_core_api::flow::WebSocketMessage) -> Result<WebSocketMessageAction, BoxError> {
        Ok(WebSocketMessageAction::Continue(message))
    }
}
