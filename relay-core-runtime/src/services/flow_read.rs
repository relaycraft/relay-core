use crate::CoreState;
use async_trait::async_trait;
use relay_core_api::flow::Flow;
use relay_core_api::modification::{FlowQuery, FlowSummary};

#[async_trait]
pub trait FlowReadService: Send + Sync {
    async fn get_flow(&self, id: &str) -> Option<Flow>;
    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary>;
    /// Delete captured flows and summaries. Returns `(flows, summaries)` removed.
    async fn clear_captured_flows(&self) -> Result<(u64, u64), String>;
}

#[async_trait]
impl FlowReadService for CoreState {
    async fn get_flow(&self, id: &str) -> Option<Flow> {
        CoreState::get_flow(self, id.to_string()).await
    }

    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary> {
        CoreState::search_flows(self, query).await
    }

    async fn clear_captured_flows(&self) -> Result<(u64, u64), String> {
        CoreState::clear_captured_flows(self).await
    }
}
