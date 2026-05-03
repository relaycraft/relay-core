use async_trait::async_trait;
use relay_core_api::flow::Flow;
use relay_core_api::modification::{FlowQuery, FlowSummary};
use crate::CoreState;

#[async_trait]
pub trait FlowReadService: Send + Sync {
    async fn get_flow(&self, id: &str) -> Option<Flow>;
    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary>;
}

#[async_trait]
impl FlowReadService for CoreState {
    async fn get_flow(&self, id: &str) -> Option<Flow> {
        CoreState::get_flow(self, id.to_string()).await
    }

    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary> {
        CoreState::search_flows(self, query).await
    }
}
