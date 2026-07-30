use crate::audit::AuditActor;
use crate::{CoreInterceptSnapshot, CoreState};
use async_trait::async_trait;
use relay_core_api::flow::WebSocketMessage;
use relay_core_api::modification::FlowModification;
use relay_core_lib::InterceptionResult;
use tokio::sync::oneshot;

#[async_trait]
pub trait InterceptService: Send + Sync {
    async fn register_intercept(&self, key: String, tx: oneshot::Sender<InterceptionResult>);
    async fn set_pending_ws_message(&self, key: String, message: WebSocketMessage);
    async fn resolve_intercept(
        &self,
        key: String,
        result: InterceptionResult,
    ) -> Result<(), String>;
    async fn resolve_intercept_with_modifications_from(
        &self,
        actor: AuditActor,
        key: String,
        action: &str,
        mods: Option<FlowModification>,
    ) -> Result<(), String>;
    async fn is_flow_intercepted(&self, flow_id: String) -> bool;
    async fn intercept_snapshot(&self) -> CoreInterceptSnapshot;
}

#[async_trait]
impl InterceptService for CoreState {
    async fn register_intercept(&self, key: String, tx: oneshot::Sender<InterceptionResult>) {
        CoreState::register_intercept(self, key, tx).await
    }

    async fn set_pending_ws_message(&self, key: String, message: WebSocketMessage) {
        CoreState::set_pending_ws_message(self, key, message).await
    }

    async fn resolve_intercept(
        &self,
        key: String,
        result: InterceptionResult,
    ) -> Result<(), String> {
        CoreState::resolve_intercept(self, key, result).await
    }

    async fn resolve_intercept_with_modifications_from(
        &self,
        actor: AuditActor,
        key: String,
        action: &str,
        mods: Option<FlowModification>,
    ) -> Result<(), String> {
        CoreState::resolve_intercept_with_modifications_from(self, actor, key, action, mods).await
    }

    async fn is_flow_intercepted(&self, flow_id: String) -> bool {
        CoreState::is_flow_intercepted(self, flow_id).await
    }

    async fn intercept_snapshot(&self) -> CoreInterceptSnapshot {
        CoreState::intercept_snapshot(self).await
    }
}
