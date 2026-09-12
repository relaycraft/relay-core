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
            meta: Default::default(),
            resilience_trace: None,
            rule_variables: Default::default(),
            matched_rules: vec![],
        }
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
