use crate::log_format;
use crate::now_unix_ms;
use crate::{CoreStatusSnapshot, RuntimeLifecycle, RuntimeLifecyclePhase};
use std::sync::Mutex;
use tokio::sync::{oneshot, watch};

pub struct LifecycleManager {
    tx: watch::Sender<RuntimeLifecycle>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl LifecycleManager {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(RuntimeLifecycle::created());
        Self {
            tx,
            shutdown_tx: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> RuntimeLifecycle {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<RuntimeLifecycle> {
        self.tx.subscribe()
    }

    pub fn status_snapshot(&self) -> CoreStatusSnapshot {
        self.snapshot().into()
    }

    /// Validates the current phase and transitions to Starting.
    /// Returns Err if the proxy is already active.
    pub fn prepare_start(&self, port: u16, shutdown_tx: oneshot::Sender<()>) -> Result<(), String> {
        let current = self.snapshot();
        if current.is_active() {
            return Err(format!(
                "Proxy is already {} on port {}",
                current.phase.as_str(),
                current.port.unwrap_or(port)
            ));
        }

        let mut guard = self
            .shutdown_tx
            .lock()
            .map_err(|_| "shutdown state poisoned".to_string())?;
        *guard = Some(shutdown_tx);
        drop(guard);

        self.update(RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Starting,
            port: Some(port),
            started_at_ms: None,
            last_error: None,
        });
        Ok(())
    }

    /// Initiates shutdown. Returns Stopping if a shutdown signal was pending,
    /// or NotRunning if no proxy is running.
    pub fn stop(&self) -> Result<crate::ProxyStopResult, String> {
        let mut guard = self
            .shutdown_tx
            .lock()
            .map_err(|_| "shutdown state poisoned".to_string())?;
        let Some(tx) = guard.take() else {
            return Ok(crate::ProxyStopResult::NotRunning);
        };
        drop(guard);

        let current = self.snapshot();
        self.update(RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Stopping,
            port: current.port,
            started_at_ms: current.started_at_ms,
            last_error: current.last_error,
        });
        let _ = tx.send(());
        Ok(crate::ProxyStopResult::Stopping)
    }

    pub fn transition_to_running(&self, port: u16) {
        self.update(RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Running,
            port: Some(port),
            started_at_ms: Some(now_unix_ms()),
            last_error: None,
        });
    }

    pub fn transition_to_stopped(&self) {
        if let Ok(mut guard) = self.shutdown_tx.lock() {
            *guard = None;
        }
        self.update(RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Stopped,
            port: None,
            started_at_ms: None,
            last_error: None,
        });
    }

    pub fn transition_to_failed(&self, port: u16, error: String) {
        if let Ok(mut guard) = self.shutdown_tx.lock() {
            *guard = None;
        }
        self.update(RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Failed,
            port: Some(port),
            started_at_ms: None,
            last_error: Some(error),
        });
    }

    pub fn update(&self, lifecycle: RuntimeLifecycle) {
        let err = lifecycle
            .last_error
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("-");
        tracing::info!(
            target: "relay_core_lifecycle",
            phase = %lifecycle.phase.as_str(),
            port = %log_format::opt_u16(lifecycle.port),
            started_at_ms = %log_format::opt_u64(lifecycle.started_at_ms),
            last_error = %err,
        );
        self.tx.send_replace(lifecycle);
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}
