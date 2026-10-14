use crate::CoreState;
use async_trait::async_trait;
use relay_core_api::flow::Flow;
use relay_core_api::modification::{FlowQuery, FlowSummary};

#[async_trait]
pub trait FlowReadService: Send + Sync {
    async fn get_flow(&self, id: &str) -> Option<Flow>;
    /// The stored flow plus a protobuf view, when the body is gRPC.
    ///
    /// The view is computed here and is not written back. Replay, HAR, and lists keep using
    /// [`get_flow`](Self::get_flow), which returns the capture unchanged.
    async fn get_flow_detail(&self, id: &str) -> Option<Flow> {
        self.get_flow(id).await
    }
    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary>;
    /// Delete captured flows and summaries. Returns `(flows, summaries)` removed.
    async fn clear_captured_flows(&self) -> Result<(u64, u64), String>;
}

#[async_trait]
impl FlowReadService for CoreState {
    async fn get_flow(&self, id: &str) -> Option<Flow> {
        CoreState::get_flow(self, id.to_string()).await
    }

    async fn get_flow_detail(&self, id: &str) -> Option<Flow> {
        CoreState::get_flow(self, id.to_string())
            .await
            .map(|flow| self.attach_protobuf_view(flow))
    }

    async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary> {
        CoreState::search_flows(self, query).await
    }

    async fn clear_captured_flows(&self) -> Result<(u64, u64), String> {
        CoreState::clear_captured_flows(self).await
    }
}
