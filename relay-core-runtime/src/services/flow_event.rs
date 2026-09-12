use crate::CoreState;
use relay_core_api::event::{CloseReason, FlowEvent};
use relay_core_api::flow::{Flow, FlowUpdate};
use tokio::sync::broadcast;

/// Turn a flow's recorded close reason into its terminal lifecycle event.
///
/// Roadmap §4-4: the reason is *stated* by whatever ended the exchange, so this maps a recorded fact
/// instead of inferring one from a snapshot. Only `Completed` is a completion — every other reason
/// describes a failure and is reported verbatim, which is what makes an upstream error
/// distinguishable from a success on the event stream.
///
/// The timestamp is the flow's own recorded end rather than the moment this message was processed:
/// it is the same fact the Flow carries, and it is always available because `start_time` precedes
/// it.
pub fn terminal_event(flow: &Flow, reason: CloseReason) -> FlowEvent {
    let at = flow.end_time.unwrap_or(flow.start_time);
    match reason {
        CloseReason::Completed => FlowEvent::Completed {
            flow_id: flow.id,
            at,
        },
        reason => FlowEvent::Errored {
            flow_id: flow.id,
            reason,
            at,
        },
    }
}

pub trait FlowEventHub: Send + Sync {
    fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate>;
    /// Typed lifecycle transitions, additive to the snapshot channel (roadmap §4-4).
    ///
    /// Defaults to an empty channel so a host that has no event source still compiles; a consumer
    /// then simply never sees a transition instead of failing to build.
    fn subscribe_flow_events(&self) -> broadcast::Receiver<FlowEvent> {
        broadcast::channel(1).1
    }
    fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate;
    fn record_flow_events_lagged(&self, skipped: u64);
}

/// Publishing side of the lifecycle channel.
///
/// Narrow on purpose: an interceptor may report what it did, but it cannot read the channel or
/// reach the rest of `CoreState` through this handle.
pub trait FlowEventSink: Send + Sync {
    /// Publish one lifecycle event. Publishing is infallible: with no subscribers nothing happens.
    fn publish_flow_event(&self, event: FlowEvent);
}

impl FlowEventHub for CoreState {
    fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate> {
        CoreState::subscribe_flow_updates(self)
    }

    fn subscribe_flow_events(&self) -> broadcast::Receiver<FlowEvent> {
        CoreState::subscribe_flow_events(self)
    }

    fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate {
        CoreState::redact_flow_update_for_output(self, update)
    }

    fn record_flow_events_lagged(&self, skipped: u64) {
        CoreState::record_flow_events_lagged(self, skipped)
    }
}

impl FlowEventSink for CoreState {
    fn publish_flow_event(&self, event: FlowEvent) {
        CoreState::publish_flow_event(self, event)
    }
}

/// Collects published events instead of broadcasting them.
///
/// A test double, in the same spirit as `Store::pool_for_tests`: asserting *which* lifecycle events
/// a code path publishes requires observing them, and a broadcast channel only lets a test see them
/// if it happens to be subscribed at the right moment.
#[derive(Default)]
pub struct RecordingFlowEventSink {
    events: std::sync::Mutex<Vec<FlowEvent>>,
}

impl RecordingFlowEventSink {
    /// Every event published so far, in order.
    pub fn recorded(&self) -> Vec<FlowEvent> {
        self.events
            .lock()
            .expect("recording sink lock should not be poisoned")
            .clone()
    }
}

impl FlowEventSink for RecordingFlowEventSink {
    fn publish_flow_event(&self, event: FlowEvent) {
        self.events
            .lock()
            .expect("recording sink lock should not be poisoned")
            .push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::terminal_event;
    use relay_core_api::event::{CloseReason, FlowEvent};
    use relay_core_api::flow::Flow;

    fn flow_with(close_reason: Option<CloseReason>) -> Flow {
        let mut flow: Flow = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-0000000000aa","start_time":"2026-05-20T10:00:00Z",
                "end_time":"2026-05-20T10:00:01Z",
                "network":{"client_ip":"127.0.0.1","client_port":1234,"server_ip":"0.0.0.0",
                           "server_port":0,"protocol":"TCP","tls":false,"tls_version":null,
                           "sni":null},
                "layer":{"type":"Http","data":{"request":{"method":"GET","url":"https://example.com/",
                          "version":"HTTP/1.1","headers":[],"cookies":[],"query":[],"body":null},
                          "response":null,"error":null}},
                "tags":[],"meta":{}}"#,
        )
        .expect("sample flow should parse");
        flow.close_reason = close_reason;
        flow
    }

    /// A recorded normal completion is a completion; anything else is a failure carrying its own
    /// reason. Before this, there was no way to tell an upstream error from a success at all.
    #[test]
    fn a_completed_reason_reports_completion() {
        let flow = flow_with(Some(CloseReason::Completed));
        let event = terminal_event(&flow, CloseReason::Completed);

        match event {
            FlowEvent::Completed { flow_id, at } => {
                assert_eq!(flow_id, flow.id);
                assert_eq!(
                    at,
                    flow.end_time.expect("sample flow has an end_time"),
                    "the event must carry the flow's recorded end, not the processing moment"
                );
            }
            other => panic!("expected a completion, got {other:?}"),
        }
    }

    /// A flow with no recorded end still yields a usable timestamp rather than an invented one.
    #[test]
    fn an_unfinished_flow_falls_back_to_its_start_time() {
        let mut flow = flow_with(Some(CloseReason::UpstreamClosed));
        flow.end_time = None;

        match terminal_event(&flow, CloseReason::UpstreamClosed) {
            FlowEvent::Errored { at, .. } => assert_eq!(at, flow.start_time),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn every_other_reason_is_reported_verbatim_as_an_error() {
        let flow = flow_with(None);
        let reasons = [
            CloseReason::UpstreamClosed,
            CloseReason::ClientClosed,
            CloseReason::Reset,
            CloseReason::PolicyDrop {
                detail: "request dropped by policy".to_string(),
            },
            CloseReason::ParserError {
                detail: "bad frame".to_string(),
            },
            CloseReason::TlsError {
                detail: "bad certificate".to_string(),
            },
            CloseReason::Timeout {
                kind: "total".to_string(),
            },
        ];

        for reason in reasons {
            let event = terminal_event(&flow, reason.clone());
            match event {
                FlowEvent::Errored {
                    flow_id: id,
                    reason: reported,
                    ..
                } => {
                    assert_eq!(id, flow.id);
                    assert_eq!(
                        reported, reason,
                        "the reported reason must be the recorded one"
                    );
                }
                other => panic!("{reason:?} should be an error, got {other:?}"),
            }
        }
    }
}
