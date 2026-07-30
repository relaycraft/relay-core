use async_trait::async_trait;
use base64::prelude::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use relay_core_api::flow::{BodyData, Flow, FlowUpdate, Layer, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, HttpBody, InterceptionResult, Interceptor, RequestAction, ResponseAction,
    WebSocketMessageAction,
};
use relay_core_lib::proxy::budget::BudgetedBody;
use relay_core_lib::proxy::http_utils::mock_to_response;
use relay_core_lib::rule::{RuleStage, RuleTraceSummary, TerminalReason};
use relay_core_runtime::services::{InterceptService, PolicyService, RuleService};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};

pub use relay_core_runtime::rule::InterceptRule;

pub struct TauriInterceptor<R: Runtime> {
    pub app_handle: AppHandle<R>,
    pub rules: Arc<dyn RuleService>,
    pub intercepts: Arc<dyn InterceptService>,
    pub policy: Arc<dyn PolicyService>,
    pub flow_sender: mpsc::Sender<FlowUpdate>,
}

#[async_trait]
impl<R: Runtime> Interceptor for TauriInterceptor<R> {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.rules.get_rule_engine().await;

        let ctx = engine.execute(RuleStage::RequestHeaders, flow).await;
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return self
                .handle_termination(reason, flow, "request_headers", None)
                .await;
        }

        if let RuleTraceSummary::Modified { .. } = &ctx.summary
            && let Layer::Http(http) = &flow.layer
        {
            return InterceptionResult::ModifiedRequest(http.request.clone());
        }

        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let engine = self.rules.get_rule_engine().await;
        let policy = self.policy.policy_snapshot();

        if !engine.has_rules_for_stage(RuleStage::RequestBody) {
            return Ok(RequestAction::Continue(body));
        }

        let budget = policy.rule_body_inspect_budget;
        let mut budgeted = BudgetedBody::new(body, budget);
        let body_bytes = (&mut budgeted).collect().await?.to_bytes();

        if budgeted.budget_exceeded {
            flow.tags.push("rule_skipped:budget_exceeded".to_string());
            let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
            return Ok(RequestAction::Continue(new_body));
        }

        self.update_flow_request_body(flow, &body_bytes);

        let ctx = engine.execute(RuleStage::RequestBody, flow).await;

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            let result = self
                .handle_termination(reason, flow, "request_body", None)
                .await;
            match result {
                InterceptionResult::Drop => return Ok(RequestAction::Drop),
                InterceptionResult::MockResponse(res) => {
                    return Ok(RequestAction::MockResponse(mock_to_response(res)));
                }
                InterceptionResult::ModifiedResponse(res) => {
                    return Ok(RequestAction::MockResponse(mock_to_response(res)));
                }
                InterceptionResult::ModifiedRequest(req) => {
                    let new_body_bytes = if let Some(bd) = req.body {
                        self.decode_body(&bd)
                    } else {
                        Bytes::new()
                    };
                    let new_body: HttpBody =
                        Full::new(new_body_bytes).map_err(|e| match e {}).boxed();
                    return Ok(RequestAction::Continue(new_body));
                }
                InterceptionResult::Continue => {}
                _ => return Ok(RequestAction::Drop),
            }
        }

        if let RuleTraceSummary::Modified { .. } = &ctx.summary
            && let Layer::Http(http) = &flow.layer
            && let Some(body_data) = &http.request.body
        {
            let new_bytes = self.decode_body(body_data);
            let new_body: HttpBody = Full::new(new_bytes).map_err(|e| match e {}).boxed();
            return Ok(RequestAction::Continue(new_body));
        }

        let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
        Ok(RequestAction::Continue(new_body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.rules.get_rule_engine().await;

        let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return self
                .handle_termination(reason, flow, "response_headers", None)
                .await;
        }

        if let RuleTraceSummary::Modified { .. } = &ctx.summary
            && let Layer::Http(http) = &flow.layer
            && let Some(res) = &http.response
        {
            return InterceptionResult::ModifiedResponse(res.clone());
        }

        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let engine = self.rules.get_rule_engine().await;
        let policy = self.policy.policy_snapshot();

        if !engine.has_rules_for_stage(RuleStage::ResponseBody) {
            return Ok(ResponseAction::Continue(body));
        }

        let budget = policy.rule_body_inspect_budget;
        let mut budgeted = BudgetedBody::new(body, budget);
        let body_bytes = (&mut budgeted).collect().await?.to_bytes();

        if budgeted.budget_exceeded {
            flow.tags.push("rule_skipped:budget_exceeded".to_string());
            let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
            return Ok(ResponseAction::Continue(new_body));
        }

        self.update_flow_response_body(flow, &body_bytes);

        let ctx = engine.execute(RuleStage::ResponseBody, flow).await;

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            let result = self
                .handle_termination(reason, flow, "response_body", None)
                .await;
            match result {
                InterceptionResult::Drop => return Ok(ResponseAction::Drop),
                InterceptionResult::MockResponse(res) => {
                    return Ok(ResponseAction::ModifiedResponse(mock_to_response(res)));
                }
                InterceptionResult::ModifiedResponse(res) => {
                    return Ok(ResponseAction::ModifiedResponse(mock_to_response(res)));
                }
                InterceptionResult::Continue => {}
                _ => return Ok(ResponseAction::Drop),
            }
        }

        if let RuleTraceSummary::Modified { .. } = &ctx.summary
            && let Layer::Http(http) = &flow.layer
            && let Some(res) = &http.response
            && let Some(_body_data) = &res.body
        {
            return Ok(ResponseAction::ModifiedResponse(mock_to_response(res.clone())));
        }

        let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
        Ok(ResponseAction::Continue(new_body))
    }

    async fn on_websocket_message(
        &self,
        flow: &mut Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        let engine = self.rules.get_rule_engine().await;

        if let Layer::WebSocket(ws) = &mut flow.layer {
            ws.messages.push(message.clone());

            if ws.messages.len() > 1000 {
                let excess = ws.messages.len() - 1000;
                ws.messages.drain(0..excess);
            }
        }

        let ctx = engine.execute(RuleStage::WebSocketMessage, flow).await;

        let mut modified_message = None;
        if let Layer::WebSocket(ws) = &mut flow.layer {
            modified_message = ws.messages.pop();
        }

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            match reason {
                TerminalReason::Mock => {
                    if let Some(mod_msg) = modified_message {
                        return Ok(WebSocketMessageAction::Continue(mod_msg));
                    }
                }
                _ => {
                    let result = self
                        .handle_termination(reason, flow, "ws_msg", Some(&message))
                        .await;
                    match result {
                        InterceptionResult::Drop => return Ok(WebSocketMessageAction::Drop),
                        InterceptionResult::ModifiedMessage(mod_msg) => {
                            return Ok(WebSocketMessageAction::Continue(mod_msg));
                        }
                        InterceptionResult::Continue => {
                            if let Some(mod_msg) = modified_message {
                                return Ok(WebSocketMessageAction::Continue(mod_msg));
                            }
                        }
                        _ => return Ok(WebSocketMessageAction::Continue(message)),
                    }
                }
            }
        }

        if let Some(mod_msg) = modified_message {
            Ok(WebSocketMessageAction::Continue(mod_msg))
        } else {
            Ok(WebSocketMessageAction::Continue(message))
        }
    }
}

impl<R: Runtime> TauriInterceptor<R> {
    fn update_flow_request_body(&self, flow: &mut Flow, bytes: &Bytes) {
        if let Layer::Http(http) = &mut flow.layer {
            let (content, encoding) = if let Ok(s) = String::from_utf8(bytes.to_vec()) {
                (s, "utf-8".to_string())
            } else {
                (BASE64_STANDARD.encode(bytes), "base64".to_string())
            };

            http.request.body = Some(BodyData {
                encoding,
                content,
                size: bytes.len() as u64,
            });
        }
    }

    fn update_flow_response_body(&self, flow: &mut Flow, bytes: &Bytes) {
        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            let (content, encoding) = if let Ok(s) = String::from_utf8(bytes.to_vec()) {
                (s, "utf-8".to_string())
            } else {
                (BASE64_STANDARD.encode(bytes), "base64".to_string())
            };

            res.body = Some(BodyData {
                encoding,
                content,
                size: bytes.len() as u64,
            });
        }
    }

    fn decode_body(&self, body_data: &BodyData) -> Bytes {
        if body_data.encoding == "base64" {
            BASE64_STANDARD
                .decode(&body_data.content)
                .unwrap_or_default()
                .into()
        } else {
            Bytes::from(body_data.content.clone())
        }
    }

    async fn handle_termination(
        &self,
        reason: &TerminalReason,
        flow: &Flow,
        phase: &str,
        ws_message: Option<&WebSocketMessage>,
    ) -> InterceptionResult {
        match reason {
            TerminalReason::Drop | TerminalReason::Abort | TerminalReason::RateLimited => {
                InterceptionResult::Drop
            }
            TerminalReason::Mock | TerminalReason::Redirect => match &flow.layer {
                Layer::Http(http) => {
                    if let Some(res) = &http.response {
                        InterceptionResult::MockResponse(res.clone())
                    } else {
                        InterceptionResult::Drop
                    }
                }
                Layer::WebSocket(ws) => {
                    if ws.handshake_response.status != 0 && ws.handshake_response.status != 101 {
                        InterceptionResult::MockResponse(ws.handshake_response.clone())
                    } else {
                        InterceptionResult::Drop
                    }
                }
                _ => InterceptionResult::Drop,
            },
            TerminalReason::Inspect => {
                let (tx, rx) = oneshot::channel();
                let key = if let Some(msg) = ws_message {
                    format!("{}:ws_msg:{}", flow.id, msg.id)
                } else {
                    format!("{}:{}", flow.id, phase)
                };

                self.intercepts.register_intercept(key.clone(), tx).await;

                if let Some(msg) = ws_message {
                    self.intercepts
                        .set_pending_ws_message(key.clone(), msg.clone())
                        .await;
                }

                if let Err(e) = self.app_handle.emit("flow-intercept", &key) {
                    eprintln!("Failed to emit flow-intercept event: {}", e);
                }

                println!("Intercepted {}: {}", phase, key);

                match timeout(Duration::from_secs(30), rx).await {
                    Ok(Ok(result)) => {
                        println!("Resumed {}: {} with result {:?}", phase, key, result);
                        result
                    }
                    Ok(Err(_)) => {
                        eprintln!("Interception channel dropped for {}", key);
                        let _ = self
                            .intercepts
                            .resolve_intercept(key.clone(), InterceptionResult::Continue)
                            .await
                            .inspect_err(|e| {
                                eprintln!("Failed to resolve orphan intercept {}: {}", key, e)
                            });
                        InterceptionResult::Continue
                    }
                    Err(_) => {
                        eprintln!("Interception timeout for {}", key);
                        let _ = self
                            .intercepts
                            .resolve_intercept(key.clone(), InterceptionResult::Continue)
                            .await
                            .inspect_err(|e| {
                                eprintln!("Failed to resolve orphan intercept {}: {}", key, e)
                            });
                        InterceptionResult::Continue
                    }
                }
            }
        }
    }
}
