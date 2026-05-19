use crate::{CoreMetrics, CoreState, CoreStatusReport, CoreStatusSnapshot, RuntimeLifecycle};
use async_trait::async_trait;
use tokio::sync::watch;

#[async_trait]
pub trait RuntimeStatusService: Send + Sync {
    fn status_snapshot(&self) -> CoreStatusSnapshot;
    async fn status_report(&self) -> CoreStatusReport;
    async fn get_metrics(&self) -> CoreMetrics;
    async fn get_metrics_prometheus_text(&self) -> String;
    fn subscribe_lifecycle(&self) -> watch::Receiver<RuntimeLifecycle>;
}

#[async_trait]
impl RuntimeStatusService for CoreState {
    fn status_snapshot(&self) -> CoreStatusSnapshot {
        CoreState::status_snapshot(self)
    }

    async fn status_report(&self) -> CoreStatusReport {
        CoreState::status_report(self).await
    }

    async fn get_metrics(&self) -> CoreMetrics {
        CoreState::get_metrics(self).await
    }

    async fn get_metrics_prometheus_text(&self) -> String {
        CoreState::get_metrics_prometheus_text(self).await
    }

    fn subscribe_lifecycle(&self) -> watch::Receiver<RuntimeLifecycle> {
        CoreState::subscribe_lifecycle(self)
    }
}
