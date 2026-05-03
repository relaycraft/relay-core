use async_trait::async_trait;
use tokio::sync::broadcast;
use crate::audit::AuditEvent;
use crate::{CoreAuditQuery, CoreAuditSnapshot, CoreState};

#[async_trait]
pub trait AuditService: Send + Sync {
    fn audit_snapshot(&self, limit: usize) -> CoreAuditSnapshot;
    async fn query_audit_snapshot(&self, query: CoreAuditQuery) -> CoreAuditSnapshot;
    fn subscribe_audit_events(&self) -> broadcast::Receiver<AuditEvent>;
    fn record_audit_events_lagged(&self, skipped: u64);
}

#[async_trait]
impl AuditService for CoreState {
    fn audit_snapshot(&self, limit: usize) -> CoreAuditSnapshot {
        CoreState::audit_snapshot(self, limit)
    }

    async fn query_audit_snapshot(&self, query: CoreAuditQuery) -> CoreAuditSnapshot {
        CoreState::query_audit_snapshot(self, query).await
    }

    fn subscribe_audit_events(&self) -> broadcast::Receiver<AuditEvent> {
        CoreState::subscribe_audit_events(self)
    }

    fn record_audit_events_lagged(&self, skipped: u64) {
        CoreState::record_audit_events_lagged(self, skipped)
    }
}
