use crate::CoreState;
use relay_core_api::flow::FlowUpdate;
use tokio::sync::broadcast;

pub trait FlowEventHub: Send + Sync {
    fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate>;
    fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate;
    fn record_flow_events_lagged(&self, skipped: u64);
}

impl FlowEventHub for CoreState {
    fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate> {
        CoreState::subscribe_flow_updates(self)
    }

    fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate {
        CoreState::redact_flow_update_for_output(self, update)
    }

    fn record_flow_events_lagged(&self, skipped: u64) {
        CoreState::record_flow_events_lagged(self, skipped)
    }
}
