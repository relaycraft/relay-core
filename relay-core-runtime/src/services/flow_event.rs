use crate::CoreState;
use relay_core_api::event::FlowEvent;
use relay_core_api::flow::FlowUpdate;
use tokio::sync::broadcast;

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
