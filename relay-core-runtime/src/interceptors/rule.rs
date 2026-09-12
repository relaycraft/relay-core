use crate::interceptors::inspect::handle_rule_termination;
use crate::services::{InterceptService, RuleService};
use async_trait::async_trait;
use relay_core_api::body_plan::{BodyPlan, BodyPlanInputs, decide};
use relay_core_api::flow::{Direction, Flow, Layer};
use relay_core_api::rule::{RuleStage, RuleTraceSummary};
use relay_core_lib::interceptor::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, InterceptionResult,
    Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use relay_core_lib::proxy::body_plan::{buffer_prefix, headers_for_direction, record_body_on_flow};
use relay_core_lib::proxy::http_utils::mock_to_response;
use relay_core_lib::rule::RuleEngine;
use relay_core_lib::rule::stage_guard::{self, mark_stage_executed};
use std::sync::Arc;

pub struct RuleInterceptor {
    rules: Arc<dyn RuleService>,
    intercepts: Arc<dyn InterceptService>,
}

impl RuleInterceptor {
    pub fn new(rules: Arc<dyn RuleService>, intercepts: Arc<dyn InterceptService>) -> Self {
        Self { rules, intercepts }
    }
}

/// Inputs for the BodyPlan decision, using only what this interceptor can observe.
///
/// Script hooks and manual breakpoints are separate interceptors, so they are not claimed here. The
/// budget comes from the engine's `ProxyPolicy` so every host sizes it identically; a zero budget
/// disables buffering entirely.
fn body_plan_inputs(engine: &RuleEngine, consumes_body: bool) -> BodyPlanInputs {
    BodyPlanInputs {
        has_body_stage_rules: consumes_body,
        has_body_hook_script: false,
        has_body_intercept: false,
        // The tap path already retains a bounded prefix for observation, so this decision only has
        // to account for active inspection.
        wants_observation: false,
        budget: engine
            .policy()
            .map(|p| p.rule_body_inspect_budget)
            .unwrap_or(DEFAULT_BODY_INSPECT_BUDGET),
    }
}

/// Fallback when a host builds an engine without a policy; matches
/// `ProxyPolicy::rule_body_inspect_budget`'s default so the two cannot silently diverge.
const DEFAULT_BODY_INSPECT_BUDGET: usize = 1024 * 1024;

/// Map a completed request-body stage result onto the wire action.
async fn finish_request_stage(
    intercepts: &Arc<dyn InterceptService>,
    flow: &mut Flow,
    ctx: relay_core_lib::rule::engine::ExecutionContext,
    forwarded: HttpBody,
) -> Result<RequestAction, BoxError> {
    if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
        let result = handle_rule_termination(intercepts, reason, flow, "request_body", None).await;
        return Ok(match result {
            InterceptionResult::Drop => RequestAction::Drop,
            InterceptionResult::MockResponse(res) => {
                RequestAction::MockResponse(mock_to_response(res))
            }
            _ => RequestAction::Drop,
        });
    }
    Ok(RequestAction::Continue(forwarded))
}

#[async_trait]
impl Interceptor for RuleInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.rules.get_rule_engine().await;

        // The proxy has to know whether the response body will be inspected *before* it forwards
        // the response, because a body-stage rule can only match a body that was retained. Declare
        // the intent here, where the rule set is visible; streaming is kept when nothing needs it.
        if engine.stage_consumes_body(RuleStage::ResponseBody) {
            let budget = body_plan_inputs(&engine, true).budget;
            relay_core_lib::rule::stage_guard::request_response_body(flow, budget);
        }

        if !engine.has_rules_for_stage(RuleStage::RequestHeaders) {
            return InterceptionResult::Continue;
        }

        let ctx = engine.execute(RuleStage::RequestHeaders, flow).await;
        mark_stage_executed(flow, &RuleStage::RequestHeaders);

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(
                &self.intercepts,
                reason,
                flow,
                "request_headers",
                None,
            )
            .await;
        }

        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let engine = self.rules.get_rule_engine().await;
        if !engine.has_rules_for_stage(RuleStage::RequestBody) {
            return Ok(RequestAction::Continue(body));
        }

        // Body-stage rules can only match a body they can see. Decide explicitly whether that is
        // worth the cost instead of buffering unconditionally (roadmap §3-3 BodyPlan, §22).
        let limit = match decide(body_plan_inputs(
            &engine,
            engine.stage_consumes_body(RuleStage::RequestBody),
        )) {
            BodyPlan::Buffer { limit } => limit,
            _ => {
                // Nothing here needs the bytes: run the stage against metadata only and stream on.
                let ctx = engine.execute(RuleStage::RequestBody, flow).await;
                mark_stage_executed(flow, &RuleStage::RequestBody);
                return finish_request_stage(&self.intercepts, flow, ctx, body).await;
            }
        };

        // Retain a bounded prefix while every byte still flows through untouched.
        let (snapshot, forwarded) = buffer_prefix(body, limit).into_parts();

        if snapshot.truncated {
            // A prefix is not the body: do not let rules match on it, and record why.
            flow.tags.push("rule_skipped:body_truncated".to_string());
        } else {
            let headers = headers_for_direction(flow, Direction::ClientToServer);
            record_body_on_flow(
                flow,
                Direction::ClientToServer,
                &snapshot.bytes,
                snapshot.total_bytes,
                &headers,
            );
            // Tell later interceptors in the chain not to read the stream a second time.
            stage_guard::mark_body_captured(flow);
        }

        let ctx = engine.execute(RuleStage::RequestBody, flow).await;
        mark_stage_executed(flow, &RuleStage::RequestBody);
        finish_request_stage(&self.intercepts, flow, ctx, forwarded).await
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.rules.get_rule_engine().await;
        if !engine.has_rules_for_stage(RuleStage::ResponseHeaders) {
            return InterceptionResult::Continue;
        }

        let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
        mark_stage_executed(flow, &RuleStage::ResponseHeaders);
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(
                &self.intercepts,
                reason,
                flow,
                "response_headers",
                None,
            )
            .await;
        }

        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let engine = self.rules.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::ResponseBody) {
            let ctx = engine.execute(RuleStage::ResponseBody, flow).await;
            mark_stage_executed(flow, &RuleStage::ResponseBody);
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result =
                    handle_rule_termination(&self.intercepts, reason, flow, "response_body", None)
                        .await;
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
        let engine = self.rules.get_rule_engine().await;
        if engine.has_rules_for_stage(RuleStage::WebSocketMessage) {
            if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.messages.push(message.clone());
            }
            let ctx = engine.execute(RuleStage::WebSocketMessage, flow).await;
            mark_stage_executed(flow, &RuleStage::WebSocketMessage);
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result = handle_rule_termination(
                    &self.intercepts,
                    reason,
                    flow,
                    "ws_msg",
                    Some(&message),
                )
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
