use crate::CoreState;
use crate::interceptors::inspect::handle_rule_termination;
use async_trait::async_trait;
use relay_core_api::flow::{Flow, Layer};
use relay_core_api::rule::{RuleStage, RuleTraceSummary};
use relay_core_lib::intercept::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, InterceptionResult,
    Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use relay_core_lib::proxy::http_utils::mock_to_response;
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

        let ctx = engine.execute(RuleStage::RequestHeaders, flow).await;

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(&self.state, reason, flow, "request_headers", None)
                .await;
        }

        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::RequestBody) {
            let ctx = engine.execute(RuleStage::RequestBody, flow).await;
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result =
                    handle_rule_termination(&self.state, reason, flow, "request_body", None).await;
                return Ok(match result {
                    InterceptionResult::Drop => RequestAction::Drop,
                    InterceptionResult::MockResponse(res) => {
                        RequestAction::MockResponse(mock_to_response(res))
                    }
                    _ => RequestAction::Drop,
                });
            }
        }
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.state.get_rule_engine().await;
        if !engine.has_rules_for_stage(RuleStage::ResponseHeaders) {
            return InterceptionResult::Continue;
        }

        let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(&self.state, reason, flow, "response_headers", None)
                .await;
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
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result =
                    handle_rule_termination(&self.state, reason, flow, "response_body", None).await;
                return Ok(match result {
                    InterceptionResult::Drop => ResponseAction::Drop,
                    InterceptionResult::MockResponse(res) => {
                        ResponseAction::ModifiedResponse(mock_to_response(res))
                    }
                    _ => ResponseAction::Drop,
                });
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
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.messages.push(message.clone());
            }
            let ctx = engine.execute(RuleStage::WebSocketMessage, flow).await;
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result =
                    handle_rule_termination(&self.state, reason, flow, "ws_msg", Some(&message))
                        .await;
                return Ok(match result {
                    InterceptionResult::Drop => WebSocketMessageAction::Drop,
                    InterceptionResult::ModifiedMessage(msg) => {
                        WebSocketMessageAction::Continue(msg)
                    }
                    _ => WebSocketMessageAction::Continue(message),
                });
            }
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
