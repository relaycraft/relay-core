use crate::CoreState;
use async_trait::async_trait;
use relay_core_api::flow::{Flow, Layer};
use relay_core_api::rule::RuleStage;
use relay_core_lib::intercept::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, InterceptionResult,
    Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use std::sync::Arc;

pub struct RuleInterceptor {
    state: Arc<CoreState>,
}

impl RuleInterceptor {
    pub fn new(state: Arc<CoreState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Interceptor for RuleInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.state.get_rule_engine().await;
        if !engine.has_rules_for_stage(RuleStage::RequestHeaders) {
            return InterceptionResult::Continue;
        }
        let had_response = matches!(&flow.layer, Layer::Http(http) if http.response.is_some());
        let ctx = engine.execute(RuleStage::RequestHeaders, flow).await;
        if ctx.is_terminated() {
            let resp = match &flow.layer {
                Layer::Http(http) if !had_response => http.response.clone(),
                _ => None,
            };
            if let Some(resp) = resp {
                return InterceptionResult::MockResponse(resp);
            }
            return InterceptionResult::Drop;
        }
        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::RequestBody) {
            let ctx = engine.execute(RuleStage::RequestBody, flow).await;
            if ctx.is_terminated() {
                return Ok(RequestAction::Drop);
            }
        }
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.state.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::ResponseHeaders) {
            let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
            if ctx.is_terminated() {
                return InterceptionResult::Drop;
            }
        }
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::ResponseBody) {
            let ctx = engine.execute(RuleStage::ResponseBody, flow).await;
            if ctx.is_terminated() {
                return Ok(ResponseAction::Drop);
            }
        }
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        flow: &mut Flow,
        message: relay_core_api::flow::WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::WebSocketMessage) {
            engine.execute(RuleStage::WebSocketMessage, flow).await;
        }
        Ok(WebSocketMessageAction::Continue(message))
    }

    async fn on_connect(&self, _conn: &ConnectionInfo) -> ConnectAction {
        ConnectAction::Allow
    }

    async fn on_disconnect(&self, _conn: &ConnectionInfo, _stats: &ConnectionStats) {}

    async fn on_websocket_start(&self, _flow: &mut Flow) {}

    async fn on_websocket_end(&self, _flow: &mut Flow, _close_code: u16, _close_reason: &str) {}

    async fn on_websocket_error(&self, _flow: &mut Flow, _error: &str) {}
}
