use crate::interceptors::inspect::handle_rule_termination;
use crate::services::{FlowEventSink, InterceptService, RuleService};
use async_trait::async_trait;
use relay_core_api::body_plan::{BodyObservation, BodyPlan, BodyPlanInputs, decide};
use relay_core_api::event::FlowEvent;
use relay_core_api::flow::{Direction, Flow, Layer};
use relay_core_api::rule::{RuleStage, RuleTraceSummary};
use relay_core_lib::interceptor::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, InterceptionResult,
    Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use relay_core_lib::proxy::body_plan::{
    buffer_prefix, headers_for_direction, record_decoded_body_on_flow,
};
use relay_core_lib::proxy::http_utils::mock_to_response;
use relay_core_lib::rule::RuleEngine;
use relay_core_lib::rule::stage_guard::{self, mark_stage_executed};
use std::sync::Arc;

pub struct RuleInterceptor {
    rules: Arc<dyn RuleService>,
    intercepts: Arc<dyn InterceptService>,
    events: Arc<dyn FlowEventSink>,
}

impl RuleInterceptor {
    pub fn new(
        rules: Arc<dyn RuleService>,
        intercepts: Arc<dyn InterceptService>,
        events: Arc<dyn FlowEventSink>,
    ) -> Self {
        Self {
            rules,
            intercepts,
            events,
        }
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
        // Observation is an explicit policy choice, not inferred: the desktop UI shows bodies while
        // a headless run does not, and inferring it would silently change what a user can see.
        observation: engine
            .policy()
            .map(|p| p.body_observation)
            .unwrap_or(BodyObservation::Off),
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
    events: &Arc<dyn FlowEventSink>,
    flow: &mut Flow,
    ctx: relay_core_lib::rule::engine::ExecutionContext,
    forwarded: HttpBody,
) -> Result<RequestAction, BoxError> {
    if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
        let result =
            handle_rule_termination(intercepts, events, reason, flow, "request_body", None).await;
        return Ok(match result {
            InterceptionResult::Drop => RequestAction::Drop,
            InterceptionResult::MockResponse(res) => {
                RequestAction::MockResponse(mock_to_response(res))
            }
            // "Resume with modifications" at the body stage. This used to fall into the catch-all
            // and become a Drop, so a user editing a request body in the desktop UI was answered
            // with a 403 whenever the desktop's own interceptor was not the one handling it — the
            // same gesture succeeded or failed depending on the host. Apply the edit instead.
            InterceptionResult::ModifiedRequest(req) => {
                // Record the edit on the Flow so filters and adapters see it, then let the proxy
                // materialize and reframe it — the same path a rule's SetRequestBody takes, so the
                // two cannot disagree about framing.
                let body_data = req.body.unwrap_or(relay_core_api::flow::BodyData {
                    encoding: "utf-8".to_string(),
                    content: String::new(),
                    size: 0,
                });
                let new_body =
                    relay_core_lib::proxy::http_utils::build_request_body_from_flow(&body_data);

                if let Layer::Http(http) = &mut flow.layer {
                    let len = body_data.size as usize;
                    http.request.headers =
                        relay_core_lib::proxy::http_utils::reframe_request_headers_for_replaced_body(
                            &http.request.headers,
                            len,
                        );
                    http.request.body = Some(body_data);
                }

                RequestAction::Continue(new_body)
            }
            // A modified *response* at the request stage is a mocked reply, not a body edit.
            InterceptionResult::ModifiedResponse(res) => {
                RequestAction::MockResponse(mock_to_response(res))
            }
            InterceptionResult::Continue => RequestAction::Continue(forwarded),
            InterceptionResult::ModifiedMessage(_) => RequestAction::Continue(forwarded),
        });
    }
    Ok(RequestAction::Continue(forwarded))
}

/// Total bytes a body-stage rule is checked against.
///
/// Exposed in trace reasons so an operator can see *how far* off a budget was.
fn body_inspection_budget(engine: &RuleEngine) -> usize {
    engine
        .policy()
        .map(|p| p.rule_body_inspect_budget)
        .unwrap_or(DEFAULT_BODY_INSPECT_BUDGET)
}

/// Record that body-dependent rules could not run because the body exceeded the budget.
///
/// Returns how many rules were skipped. The rules are not executed: matching a body filter against a
/// truncated prefix would silently produce a wrong answer, which is worse than not running.
fn record_body_rules_skipped(engine: &RuleEngine, flow: &mut Flow, stage: &RuleStage) -> usize {
    let budget = body_inspection_budget(engine);
    let reason = format!("body exceeded the {budget}-byte inspection budget");

    let rule_ids: Vec<String> = engine
        .rules_for_stage(stage)
        .into_iter()
        .filter(|id| !id.is_empty())
        .collect();

    if rule_ids.is_empty() {
        return 0;
    }

    for rule_id in &rule_ids {
        flow.meta
            .insert(format!("rule_skipped:{rule_id}"), reason.clone());
    }
    flow.tags
        .push(format!("rule_skipped:{}", stage_debug(stage)));

    rule_ids.len()
}

fn stage_debug(stage: &RuleStage) -> String {
    format!("{stage:?}")
}

/// Publish what this stage changed, per rule.
///
/// Roadmap §4-4: `RuleTraceSummary::Modified` says rules ran, which is not enough for a UI or an
/// agent to explain a change. Each applied rule becomes one event naming the fields it touched;
/// a stage that changed nothing publishes nothing.
fn publish_mutations(
    events: &Arc<dyn FlowEventSink>,
    flow: &Flow,
    direction: &Direction,
    ctx: &relay_core_lib::rule::engine::ExecutionContext,
) {
    for mutation in &ctx.mutations {
        events.publish_flow_event(FlowEvent::MutationApplied {
            flow_id: flow.id,
            direction: direction.clone(),
            actor: format!("rule:{}", mutation.rule_id),
            fields: mutation.fields.clone(),
        });
    }
}

/// Expose what a stage actually decided.
///
/// `ExecutionContext.trace` is discarded by every production caller, so before this a stage that
/// matched nothing, skipped everything, or failed an action was indistinguishable from one that was
/// never reached. The summary is logged, and failures also reach the metrics counter.
fn report_stage_outcome(
    rules: &Arc<dyn RuleService>,
    stage: &RuleStage,
    ctx: &relay_core_lib::rule::engine::ExecutionContext,
) {
    let failed = ctx
        .trace
        .iter()
        .filter(|event| matches!(event.outcome, relay_core_api::rule::RuleOutcome::Failed(_)))
        .count();

    if failed > 0 {
        for event in &ctx.trace {
            if let relay_core_api::rule::RuleOutcome::Failed(reason) = &event.outcome {
                tracing::warn!(
                    rule_id = %event.rule_id,
                    stage = ?stage,
                    "Rule action failed: {}",
                    reason
                );
            }
        }
        for _ in 0..failed {
            rules.report_rule_exec_error();
        }
    }

    let executed = ctx.trace.len();
    if executed > 0 {
        tracing::debug!(
            stage = ?stage,
            evaluated = executed,
            failed,
            "Rule stage completed"
        );
    }
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
        report_stage_outcome(&self.rules, &RuleStage::RequestHeaders, &ctx);
        publish_mutations(&self.events, flow, &Direction::ClientToServer, &ctx);
        mark_stage_executed(flow, &RuleStage::RequestHeaders);

        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(
                &self.intercepts,
                &self.events,
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
        let plan = decide(body_plan_inputs(
            &engine,
            engine.stage_consumes_body(RuleStage::RequestBody),
        ));

        let limit = match plan {
            BodyPlan::Buffer { limit } => limit,
            // Observation only: the bounded prefix is retained by the tap path, which wraps the
            // body for exactly this purpose and streams it untouched. Draining here to snapshot the
            // prefix would materialize the whole body and destroy the streaming property this plan
            // exists to preserve, so the stage simply runs against metadata and streams on.
            BodyPlan::Capture { .. } => {
                let ctx = engine.execute(RuleStage::RequestBody, flow).await;
                publish_mutations(&self.events, flow, &Direction::ClientToServer, &ctx);
                mark_stage_executed(flow, &RuleStage::RequestBody);
                return finish_request_stage(&self.intercepts, &self.events, flow, ctx, body).await;
            }
            BodyPlan::PassThrough => {
                // Nothing here needs the bytes: run the stage against metadata only and stream on.
                let ctx = engine.execute(RuleStage::RequestBody, flow).await;
                publish_mutations(&self.events, flow, &Direction::ClientToServer, &ctx);
                mark_stage_executed(flow, &RuleStage::RequestBody);
                return finish_request_stage(&self.intercepts, &self.events, flow, ctx, body).await;
            }
        };

        // Retain a bounded prefix while every byte still flows through untouched.
        let (snapshot, forwarded) = buffer_prefix(body, limit).into_parts();

        if snapshot.truncated {
            // A prefix is not the body. Rules that need the body cannot be evaluated at all, so they
            // are skipped explicitly rather than matched against a partial body — and the skip is
            // recorded as a trace event and a metric, because "my body rule did not fire" was
            // previously unanswerable.
            flow.tags.push("rule_skipped:body_truncated".to_string());

            let skipped = record_body_rules_skipped(&engine, flow, &RuleStage::RequestBody);
            if skipped > 0 {
                for _ in 0..skipped {
                    self.rules.report_rule_exec_error();
                }
                return Ok(RequestAction::Continue(forwarded));
            }
        } else {
            let headers = headers_for_direction(flow, Direction::ClientToServer);
            // Record decoded, so a body filter matches plaintext rather than compressed bytes.
            record_decoded_body_on_flow(
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
        report_stage_outcome(&self.rules, &RuleStage::RequestBody, &ctx);
        publish_mutations(&self.events, flow, &Direction::ClientToServer, &ctx);
        mark_stage_executed(flow, &RuleStage::RequestBody);
        finish_request_stage(&self.intercepts, &self.events, flow, ctx, forwarded).await
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.rules.get_rule_engine().await;
        if !engine.has_rules_for_stage(RuleStage::ResponseHeaders) {
            return InterceptionResult::Continue;
        }

        let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
        report_stage_outcome(&self.rules, &RuleStage::ResponseHeaders, &ctx);
        publish_mutations(&self.events, flow, &Direction::ServerToClient, &ctx);
        mark_stage_executed(flow, &RuleStage::ResponseHeaders);
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return handle_rule_termination(
                &self.intercepts,
                &self.events,
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
            report_stage_outcome(&self.rules, &RuleStage::ResponseBody, &ctx);
            publish_mutations(&self.events, flow, &Direction::ServerToClient, &ctx);
            mark_stage_executed(flow, &RuleStage::ResponseBody);
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                let result = handle_rule_termination(
                    &self.intercepts,
                    &self.events,
                    reason,
                    flow,
                    "response_body",
                    None,
                )
                .await;
                return Ok(match result {
                    InterceptionResult::Drop => ResponseAction::Drop,
                    InterceptionResult::MockResponse(res) => {
                        ResponseAction::ModifiedResponse(mock_to_response(res))
                    }
                    // "Resume with modifications" at the response-body stage: build the reply from
                    // the edited Flow rather than dropping it. The catch-all used to turn this into a
                    // Drop, so resuming a breakpoint with an edit produced a 403.
                    InterceptionResult::ModifiedResponse(res) => {
                        ResponseAction::ModifiedResponse(mock_to_response(res))
                    }
                    InterceptionResult::Continue => ResponseAction::Continue(body),
                    // A body edit arrives as a modified request/response on the Flow; the proxy
                    // materializes it, so continuing is the correct wire action.
                    InterceptionResult::ModifiedRequest(_) => ResponseAction::Continue(body),
                    InterceptionResult::ModifiedMessage(_) => ResponseAction::Continue(body),
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
            report_stage_outcome(&self.rules, &RuleStage::WebSocketMessage, &ctx);
            publish_mutations(&self.events, flow, &message.direction, &ctx);
            mark_stage_executed(flow, &RuleStage::WebSocketMessage);
            if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
                // `Action::MockWebSocketMessage` replaces the frame with the one the rule produced,
                // which by construction is the message the stage just appended. Routing it through
                // the generic HTTP-oriented termination path turned a mock into a dropped frame.
                if matches!(reason, relay_core_api::rule::TerminalReason::Mock)
                    && let Layer::WebSocket(ws) = &flow.layer
                    && let Some(mocked) = ws.messages.last()
                {
                    return Ok(WebSocketMessageAction::Continue(mocked.clone()));
                }

                let result = handle_rule_termination(
                    &self.intercepts,
                    &self.events,
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

#[cfg(test)]
mod stage_outcome_tests {
    use super::report_stage_outcome;
    use async_trait::async_trait;
    use relay_core_api::rule::{RuleOutcome, RuleStage, RuleTraceSummary};
    use relay_core_lib::rule::RuleExecutionEvent;
    use relay_core_lib::rule::engine::ExecutionContext;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts metric reports so the wiring can be asserted without a runtime.
    struct CountingRules {
        reported: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl crate::services::RuleService for CountingRules {
        async fn get_rules(&self) -> Vec<relay_core_lib::rule::Rule> {
            Vec::new()
        }
        fn report_rule_exec_error(&self) {
            self.reported.fetch_add(1, Ordering::Relaxed);
        }
        async fn get_rule_engine(&self) -> Arc<relay_core_lib::rule::RuleEngine> {
            Arc::new(relay_core_lib::rule::RuleEngine::new(
                Vec::new(),
                Vec::new(),
                None,
                None,
            ))
        }
        async fn upsert_rule_from(
            &self,
            _actor: crate::audit::AuditActor,
            _operation: &str,
            _target: String,
            _details: serde_json::Value,
            _rule: relay_core_lib::rule::Rule,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn delete_rule_from(
            &self,
            _actor: crate::audit::AuditActor,
            _operation: &str,
            _target: String,
            _details: serde_json::Value,
            _rule_id: &str,
        ) -> Result<bool, String> {
            Ok(false)
        }
        async fn create_mock_response_rule_from(
            &self,
            _actor: crate::audit::AuditActor,
            _target: String,
            _details: serde_json::Value,
            _config: crate::rule::MockResponseRuleConfig,
        ) -> Result<String, String> {
            Ok(String::new())
        }
        async fn create_intercept_rule_from(
            &self,
            _actor: crate::audit::AuditActor,
            _target: String,
            _details: serde_json::Value,
            _config: crate::rule::InterceptRuleConfig,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn ctx_with(outcomes: Vec<RuleOutcome>) -> ExecutionContext {
        ExecutionContext {
            trace: outcomes
                .into_iter()
                .enumerate()
                .map(|(i, outcome)| RuleExecutionEvent {
                    rule_id: format!("r{i}"),
                    stage: RuleStage::RequestHeaders,
                    matched: true,
                    duration_us: 0,
                    outcome,
                })
                .collect(),
            variables: Default::default(),
            policy: None,
            summary: RuleTraceSummary::NoMatch,
            state_store: Arc::new(
                relay_core_lib::rule::engine::state::InMemoryRuleStateStore::new(),
            ),
            throttle_bytes_per_sec: None,
            connect_override: None,
            mutations: vec![],
        }
    }

    /// `relay_core_rule_exec_errors_total` had no producer anywhere, so it read as a permanent zero.
    #[test]
    fn failed_actions_reach_the_error_counter() {
        let reported = Arc::new(AtomicUsize::new(0));
        let rules: Arc<dyn crate::services::RuleService> = Arc::new(CountingRules {
            reported: reported.clone(),
        });

        let ctx = ctx_with(vec![
            RuleOutcome::MatchedAndExecuted,
            RuleOutcome::Failed("boom".to_string()),
            RuleOutcome::Failed("boom again".to_string()),
        ]);

        report_stage_outcome(&rules, &RuleStage::RequestHeaders, &ctx);

        assert_eq!(
            reported.load(Ordering::Relaxed),
            2,
            "every failed action must reach the error counter"
        );
    }

    #[test]
    fn a_clean_stage_reports_no_errors() {
        let reported = Arc::new(AtomicUsize::new(0));
        let rules: Arc<dyn crate::services::RuleService> = Arc::new(CountingRules {
            reported: reported.clone(),
        });

        let ctx = ctx_with(vec![RuleOutcome::MatchedAndExecuted]);
        report_stage_outcome(&rules, &RuleStage::RequestHeaders, &ctx);

        assert_eq!(reported.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod intercept_resolution_tests {
    use super::finish_request_stage;
    use crate::services::{FlowEventSink, RecordingFlowEventSink};
    use async_trait::async_trait;
    use relay_core_api::event::FlowEvent;
    use relay_core_api::flow::{
        BodyData, Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol,
    };
    use relay_core_api::modification::FlowModification;
    use relay_core_api::rule::RuleTraceSummary;
    use relay_core_lib::InterceptionResult;
    use relay_core_lib::rule::engine::ExecutionContext;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    /// The service the resolver consults when a breakpoint is resumed.
    struct ResolvingIntercepts {
        resolution: InterceptionResult,
    }

    #[async_trait]
    impl crate::services::InterceptService for ResolvingIntercepts {
        async fn register_intercept(&self, _key: String, tx: oneshot::Sender<InterceptionResult>) {
            let _ = tx.send(self.resolution.clone());
        }
        async fn set_pending_ws_message(
            &self,
            _key: String,
            _message: relay_core_api::flow::WebSocketMessage,
        ) {
        }
        async fn resolve_intercept(
            &self,
            _key: String,
            _result: InterceptionResult,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn resolve_intercept_with_modifications_from(
            &self,
            _actor: crate::audit::AuditActor,
            _key: String,
            _action: &str,
            _mods: Option<FlowModification>,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn is_flow_intercepted(&self, _flow_id: String) -> bool {
            true
        }
        async fn intercept_snapshot(&self) -> crate::CoreInterceptSnapshot {
            crate::CoreInterceptSnapshot {
                pending_count: 0,
                ws_pending_count: 0,
                items: Vec::new(),
            }
        }
    }

    fn flow_with_request_body() -> Flow {
        Flow {
            id: uuid::Uuid::new_v4(),
            start_time: chrono::Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 1,
                server_ip: "127.0.0.1".to_string(),
                server_port: 2,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "POST".to_string(),
                    url: url::Url::parse("http://example.com/").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![("content-type".to_string(), "text/plain".to_string())],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: Default::default(),
            resilience_trace: None,
            rule_variables: Default::default(),
            matched_rules: vec![],
        }
    }

    fn terminated_ctx() -> ExecutionContext {
        ExecutionContext {
            trace: vec![],
            variables: Default::default(),
            policy: None,
            summary: RuleTraceSummary::Terminated {
                rule_id: "inspect-rule".to_string(),
                reason: relay_core_api::rule::TerminalReason::Inspect,
            },
            state_store: Arc::new(
                relay_core_lib::rule::engine::state::InMemoryRuleStateStore::new(),
            ),
            throttle_bytes_per_sec: None,
            connect_override: None,
            mutations: vec![],
        }
    }

    /// Resuming a body-stage breakpoint with an edit must apply the edit.
    ///
    /// `apply_flow_modification` returns `ModifiedRequest` for every request phase, and the mapping
    /// used to fall through to a catch-all `Drop` — so "resume with modifications" answered 403.
    #[tokio::test]
    async fn resuming_a_request_body_intercept_with_an_edit_applies_it() {
        let intercepts: Arc<dyn crate::services::InterceptService> =
            Arc::new(ResolvingIntercepts {
                resolution: InterceptionResult::ModifiedRequest(HttpRequest {
                    method: "POST".to_string(),
                    url: url::Url::parse("http://example.com/").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: "EDITED-BY-USER".to_string(),
                        size: 14,
                    }),
                }),
            });

        let mut flow = flow_with_request_body();
        let body: relay_core_lib::interceptor::HttpBody =
            http_body_util::BodyExt::boxed(http_body_util::BodyExt::map_err(
                http_body_util::Full::new(bytes::Bytes::from_static(b"original")),
                |e: std::convert::Infallible| -> relay_core_lib::interceptor::BoxError {
                    match e {}
                },
            ));

        let events: Arc<dyn crate::services::FlowEventSink> =
            Arc::new(crate::services::RecordingFlowEventSink::default());

        let action = finish_request_stage(&intercepts, &events, &mut flow, terminated_ctx(), body)
            .await
            .expect("intercept resolution should not error");

        match action {
            relay_core_lib::interceptor::RequestAction::Continue(new_body) => {
                let bytes = http_body_util::BodyExt::collect(new_body)
                    .await
                    .expect("collect")
                    .to_bytes();
                assert_eq!(
                    &bytes[..],
                    b"EDITED-BY-USER",
                    "the edited body must be what is forwarded"
                );
            }
            other => panic!("a resumed body breakpoint must continue, got {other:?}"),
        }
    }
    /// A breakpoint that pauses is a state change a consumer must learn about: `event: intercept`
    /// was documented as this signal and had no producer, so a UI could only poll the intercept list.
    #[tokio::test]
    async fn a_paused_breakpoint_publishes_pause_and_resolution_events() {
        let events = Arc::new(RecordingFlowEventSink::default());
        let sink: Arc<dyn FlowEventSink> = events.clone();
        let intercepts: Arc<dyn crate::services::InterceptService> =
            Arc::new(ResolvingIntercepts {
                resolution: InterceptionResult::Continue,
            });

        let mut flow = flow_with_request_body();
        let flow_id = flow.id;
        let body: relay_core_lib::interceptor::HttpBody =
            http_body_util::BodyExt::boxed(http_body_util::BodyExt::map_err(
                http_body_util::Full::new(bytes::Bytes::from_static(b"original")),
                |e: std::convert::Infallible| -> relay_core_lib::interceptor::BoxError {
                    match e {}
                },
            ));

        let _ = finish_request_stage(&intercepts, &sink, &mut flow, terminated_ctx(), body).await;

        let recorded = events.recorded();
        assert_eq!(
            recorded.len(),
            2,
            "expected a pause and a resolution: {recorded:?}"
        );

        match &recorded[0] {
            FlowEvent::InterceptPaused { flow_id: id, phase } => {
                assert_eq!(*id, flow_id, "the pause must name the flow it is holding");
                assert_eq!(phase, "request_body");
            }
            other => panic!("expected InterceptPaused first, got {other:?}"),
        }

        match &recorded[1] {
            FlowEvent::InterceptResolved {
                flow_id: id,
                phase,
                mutated,
            } => {
                assert_eq!(*id, flow_id);
                assert_eq!(phase, "request_body");
                assert!(!mutated, "an unmodified continue must not claim a mutation");
            }
            other => panic!("expected InterceptResolved second, got {other:?}"),
        }
    }

    /// Resuming with an edit is a mutation, and the event has to say so — otherwise a consumer
    /// cannot distinguish "the user changed something" from "the user clicked continue".
    #[tokio::test]
    async fn a_breakpoint_resumed_with_an_edit_reports_a_mutation() {
        let events = Arc::new(RecordingFlowEventSink::default());
        let sink: Arc<dyn FlowEventSink> = events.clone();
        let intercepts: Arc<dyn crate::services::InterceptService> =
            Arc::new(ResolvingIntercepts {
                resolution: InterceptionResult::ModifiedRequest(HttpRequest {
                    method: "POST".to_string(),
                    url: url::Url::parse("http://example.com/").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: "EDITED-BY-USER".to_string(),
                        size: 14,
                    }),
                }),
            });

        let mut flow = flow_with_request_body();
        let body: relay_core_lib::interceptor::HttpBody =
            http_body_util::BodyExt::boxed(http_body_util::BodyExt::map_err(
                http_body_util::Full::new(bytes::Bytes::from_static(b"original")),
                |e: std::convert::Infallible| -> relay_core_lib::interceptor::BoxError {
                    match e {}
                },
            ));

        let _ = finish_request_stage(&intercepts, &sink, &mut flow, terminated_ctx(), body).await;

        match events.recorded().last() {
            Some(FlowEvent::InterceptResolved { mutated, .. }) => {
                assert!(*mutated, "an edited resume must be reported as a mutation");
            }
            other => panic!("expected a resolution event, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod body_budget_skip_tests {
    use super::record_body_rules_skipped;
    use relay_core_api::flow::{Flow, Layer, NetworkInfo, TransportProtocol};
    use relay_core_api::rule::{Action, BodySource, Filter, Rule, RuleStage, RuleTermination};
    use relay_core_lib::rule::RuleEngine;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn flow() -> Flow {
        Flow {
            id: uuid::Uuid::new_v4(),
            start_time: chrono::Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 1,
                server_ip: "127.0.0.1".to_string(),
                server_port: 2,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Unknown,
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: HashMap::new(),
            matched_rules: vec![],
        }
    }

    fn body_rule(id: &str) -> Rule {
        Rule {
            id: id.to_string(),
            name: id.to_string(),
            active: true,
            stage: RuleStage::RequestBody,
            priority: 0,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![Action::SetRequestBody {
                body: BodySource::Text("replaced".to_string()),
            }],
            constraints: None,
        }
    }

    /// When a body exceeds the budget its rules cannot be evaluated, so they are skipped — and the
    /// skip must be recorded, because a body rule silently not firing is otherwise unanswerable.
    #[test]
    fn skipped_body_rules_are_recorded_per_rule_with_a_reason() {
        let policy = relay_core_api::policy::ProxyPolicy {
            rule_body_inspect_budget: 512,
            ..Default::default()
        };
        let engine = RuleEngine::new(
            vec![body_rule("body-a"), body_rule("body-b")],
            vec![],
            Some(Arc::new(policy)),
            None,
        );

        let mut flow = flow();
        let skipped = record_body_rules_skipped(&engine, &mut flow, &RuleStage::RequestBody);

        assert_eq!(skipped, 2, "both body rules should be reported as skipped");
        for id in ["body-a", "body-b"] {
            let reason = flow
                .meta
                .get(&format!("rule_skipped:{id}"))
                .unwrap_or_else(|| panic!("{id} must have a recorded skip reason"));
            assert!(
                reason.contains("512"),
                "the reason should name the budget so it is actionable, got: {reason}"
            );
        }
    }

    /// A rule at another stage must not be reported as skipped by the body stage.
    #[test]
    fn only_rules_for_the_stage_are_reported() {
        let engine = RuleEngine::new(vec![body_rule("body-a")], vec![], None, None);
        let mut flow = flow();

        assert_eq!(
            record_body_rules_skipped(&engine, &mut flow, &RuleStage::ResponseBody),
            0,
            "a RequestBody rule must not be reported when the response stage is skipped"
        );
    }

    /// The skip must not silently change non-body stages.
    #[test]
    fn no_rules_means_nothing_to_report() {
        let engine = RuleEngine::new(vec![], vec![], None, None);
        let mut flow = flow();
        assert_eq!(
            record_body_rules_skipped(&engine, &mut flow, &RuleStage::RequestBody),
            0
        );
    }
}
