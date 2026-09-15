//! Stage-level bookkeeping for rule execution.
//!
//! The rule engine must run **once per stage per flow**, but a host adapter may register its own
//! rule-executing interceptor in addition to the runtime's `RuleInterceptor` — the Tauri desktop
//! does exactly that, because its interceptor is the one that buffers bodies before body-stage
//! rules run.
//!
//! [`CompositeInterceptor`](crate::interceptor::Interceptor) has no notion of "already handled",
//! so both members executed the same stage and every mutation was applied twice: duplicate request
//! headers reached the upstream, `Delay` slept twice and `RateLimit` double-counted.
//!
//! This module records, in [`Flow::meta`] (which is `#[serde(skip)]`, so it never reaches a wire
//! format or storage), that a stage has run. Interceptors consult it so that whichever member runs
//! first performs the work and the others skip re-execution while still doing their own
//! host-specific duties.

use relay_core_api::flow::Flow;
use relay_core_api::rule::RuleStage;

/// Key prefix for the per-stage execution marker in `Flow.meta`.
const STAGE_MARKER_PREFIX: &str = "rule_stage_executed:";

fn marker_key(stage: &RuleStage) -> String {
    format!("{STAGE_MARKER_PREFIX}{stage:?}")
}

/// Has the rule engine already run for `stage` on this flow?
///
/// Takes `&RuleStage` because `RuleStage` is not `Copy`; callers should not need to clone it.
pub fn stage_already_executed(flow: &Flow, stage: &RuleStage) -> bool {
    flow.meta.contains_key(&marker_key(stage))
}

/// Record that the rule engine has run for `stage` on this flow.
///
/// Call this immediately after `execute` returns, so that a later interceptor in the same chain
/// observes it. `Flow.meta` is per-flow and in-process only, so the marker is cleared with the flow.
pub fn mark_stage_executed(flow: &mut Flow, stage: &RuleStage) {
    flow.meta.insert(marker_key(stage), "1".to_string());
}

/// `flow.meta` key carrying the response-body inspection budget for this flow.
pub const RESPONSE_BODY_BUDGET_KEY: &str = "response_body_inspect_budget";

/// `flow.meta` key marking that something will need the response body.
///
/// The proxy must know this **before** it forwards the response: a body-stage rule can only match a
/// body that was retained, and by the time the response-header stage runs the body stream has
/// already been handed on. The rule interceptor therefore publishes the intent during the
/// request-header stage, when it can see which stages have rules.
pub const NEEDS_RESPONSE_BODY_KEY: &str = "needs_response_body";

/// Declare that the response body must be retainable, with `budget` bytes.
pub fn request_response_body(flow: &mut Flow, budget: usize) {
    flow.meta
        .insert(NEEDS_RESPONSE_BODY_KEY.to_string(), budget.to_string());
}

/// How many bytes of response body should be retained, or `None` to keep streaming.
pub fn response_body_budget(flow: &Flow) -> Option<usize> {
    flow.meta
        .get(NEEDS_RESPONSE_BODY_KEY)
        .and_then(|v| v.parse::<usize>().ok())
}

/// `flow.meta` key marking that the upstream body was already materialized and recorded.
///
/// Buffering a body is not free, and more than one interceptor in a chain may want it. The first
/// one to do so marks the flow, so a later host-specific interceptor can reuse what is already on
/// the flow instead of reading the stream a second time.
pub const BODY_CAPTURED_KEY: &str = "body_captured";

/// Record that the body for this exchange has been materialized onto the flow.
pub fn mark_body_captured(flow: &mut Flow) {
    flow.meta
        .insert(BODY_CAPTURED_KEY.to_string(), "1".to_string());
}

/// Was the body already materialized onto this flow?
pub fn body_already_captured(flow: &Flow) -> bool {
    flow.meta.contains_key(BODY_CAPTURED_KEY)
}

#[cfg(test)]
mod tests {
    use super::{mark_stage_executed, stage_already_executed};
    use relay_core_api::flow::{Flow, Layer, NetworkInfo, TransportProtocol};
    use relay_core_api::rule::RuleStage;
    use uuid::Uuid;

    fn flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: chrono::Utc::now(),
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 1,
                server_ip: "127.0.0.1".to_string(),
                server_port: 2,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Unknown,
            tags: vec![],
            meta: Default::default(),
            resilience_trace: None,
            rule_variables: Default::default(),
            matched_rules: vec![],
        }
    }

    #[test]
    fn body_capture_marker_round_trips_and_stays_out_of_json() {
        let mut flow = flow();
        assert!(!super::body_already_captured(&flow));

        super::mark_body_captured(&mut flow);
        assert!(super::body_already_captured(&flow));

        let json = serde_json::to_value(&flow).expect("serialize flow");
        assert_eq!(json.get("meta"), None);
    }

    #[test]
    fn stage_marker_is_scoped_per_stage() {
        let mut flow = flow();

        assert!(!stage_already_executed(&flow, &RuleStage::RequestHeaders));
        mark_stage_executed(&mut flow, &RuleStage::RequestHeaders);

        assert!(stage_already_executed(&flow, &RuleStage::RequestHeaders));
        // Marking one stage must not suppress another.
        assert!(!stage_already_executed(&flow, &RuleStage::ResponseHeaders));
    }

    #[test]
    fn response_body_request_is_absent_until_asked_for() {
        let mut flow = flow();
        assert_eq!(super::response_body_budget(&flow), None);

        super::request_response_body(&mut flow, 4096);
        assert_eq!(super::response_body_budget(&flow), Some(4096));
    }

    #[test]
    fn response_body_request_never_serializes_into_flow_json() {
        let mut flow = flow();
        super::request_response_body(&mut flow, 4096);

        let json = serde_json::to_value(&flow).expect("serialize flow");
        assert_eq!(
            json.get("meta"),
            None,
            "Flow.meta must stay in-process only"
        );
    }

    #[test]
    fn stage_marker_never_serializes_into_flow_json() {
        let mut flow = flow();
        mark_stage_executed(&mut flow, &RuleStage::RequestBody);

        let json = serde_json::to_value(&flow).expect("serialize flow");
        assert_eq!(
            json.get("meta"),
            None,
            "Flow.meta is #[serde(skip)]; the marker must not leak into any wire format"
        );
    }
}
