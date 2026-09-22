//! The main Rust API for embedding RelayCore.
//!
//! Most users should start here. This crate owns the runtime state, proxy lifecycle,
//! intercept rules, policy management, and event streams.
//!
//! > **Note:** The `relay-core` crate name was unavailable on crates.io.
//! > `relay-core-runtime` is the official main package for RelayCore.
//!
//! ```toml
//! [dependencies]
//! relay-core-runtime = "0.1"
//! relay-core-http = "0.1"   # optional REST/SSE adapter
//! ```
//!
//! Common types are re-exported for convenience:
//! ```rust
//! use relay_core_runtime::CoreState;
//! use relay_core_runtime::flow::Flow;
//! use relay_core_runtime::policy::ProxyPolicy;
//! use relay_core_runtime::audit::AuditActor;
//! ```

use relay_core_api::event::FlowEvent;
use relay_core_api::flow::{BodyData, Direction, Flow, FlowUpdate, Layer, WebSocketMessage};
use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch, RedactionPolicy};
#[cfg(all(target_os = "linux", feature = "transparent-linux"))]
use relay_core_lib::capture::LinuxOriginalDstProvider;
#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
use relay_core_lib::capture::MacOsOriginalDstProvider;
#[cfg(target_os = "windows")]
use relay_core_lib::capture::WindowsOriginalDstProvider;
use relay_core_lib::capture::udp::UdpProxy;
use relay_core_lib::capture::{OriginalDstProvider, TcpCaptureSource, TransparentTcpCaptureSource};
use relay_core_lib::interceptor::{CompositeInterceptor, Interceptor};
use relay_core_lib::tls::CertificateAuthority;
#[cfg(feature = "script")]
use relay_core_script::ScriptInterceptor;
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use tracing::{debug, error};

use crate::audit::{AuditActor, AuditEvent, AuditEventKind, AuditOutcome};
use crate::lifecycle::LifecycleManager;
use crate::rule::{
    InterceptRule, InterceptRuleConfig, MockResponseRuleConfig, build_intercept_rules,
    build_mock_response_rule,
};
use relay_core_api::modification::{FlowQuery, FlowSummary};
use relay_core_lib::rule::Rule;
use relay_core_lib::rule::engine::RuleEngine;
use relay_core_storage::store::{AuditEventRecord, Store};
use serde_json::json;

pub mod actors;
pub mod audit;
pub mod interceptors;
pub mod lifecycle;
mod log_format;
pub mod modification;
pub mod paths;
pub mod rule;
pub mod services;

// ── Re-exports for user convenience ──
// Users can do `use relay_core_runtime::flow::Flow;` without knowing relay_core_api exists.
pub use paths::CaPaths;
pub use relay_core_api::{flow, policy};
pub use relay_core_lib::InterceptionResult;
pub use relay_core_lib::rule as lib_rule;

use actors::flow_store::{FlowStoreActor, FlowStoreMessage};
use actors::intercept_broker::{InterceptBrokerActor, InterceptBrokerMessage};
use actors::rule_store::{RuleStoreActor, RuleStoreMessage};

pub mod rule_engine {
    pub use relay_core_lib::rule_engine::*;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CoreMetrics {
    pub flows_total: usize,
    pub flows_in_memory: usize,
    pub flows_dropped: usize,
    pub intercepts_pending: usize,
    pub ws_pending_messages: usize,
    pub oldest_intercept_age_ms: Option<u64>,
    pub oldest_ws_message_age_ms: Option<u64>,
    pub rule_exec_errors: usize,
    pub audit_events_total: usize,
    pub audit_events_failed: usize,
    pub flow_events_lagged_total: usize,
    pub audit_events_lagged_total: usize,
    /// O4: Total bodies degraded (budget exceeded)
    pub proxy_body_degraded_total: usize,
    /// O4: Total HTTP requests processed through streaming pipeline
    pub proxy_http_request_total: usize,
    /// O4: Total sandbox rejections
    pub proxy_sandbox_reject_total: usize,
    /// O4: Total invalid status code rejections (strict_http_semantics)
    pub proxy_invalid_status_total: usize,
    /// Requests refused because an upstream's circuit was open. Never attempted, so it must not be
    /// read as an upstream failure count.
    pub proxy_circuit_rejected_total: usize,
    /// Flows the proxy itself dropped, e.g. a WebSocket frame that could not be delivered.
    ///
    /// Distinct from `flows_dropped`, which counts updates lost on the runtime's internal channel:
    /// this one is incremented at ~20 sites inside the proxy, and was previously unreadable — a real
    /// signal with no exporter.
    pub proxy_flows_dropped_total: usize,
    /// O4: Total bodies processed in tap (streaming) mode
    pub proxy_stream_mode_tap_total: usize,
    /// O4: Total bodies degraded from tap to pass-through
    pub proxy_stream_mode_degrade_total: usize,
    /// O4: Total bytes transmitted to upstream (client→proxy→server)
    pub proxy_bytes_sent_total: u64,
    /// O4: Total bytes received from upstream (server→proxy→client)
    pub proxy_bytes_recv_total: u64,
}

impl CoreMetrics {
    pub fn to_prometheus_text(&self) -> String {
        let oldest_intercept_age_ms = self.oldest_intercept_age_ms.unwrap_or(0);
        let oldest_ws_message_age_ms = self.oldest_ws_message_age_ms.unwrap_or(0);
        format!(
            "relay_core_flows_total {}\n\
relay_core_flows_in_memory {}\n\
relay_core_flows_dropped_total {}\n\
relay_core_intercepts_pending {}\n\
relay_core_ws_pending_messages {}\n\
relay_core_oldest_intercept_age_ms {}\n\
relay_core_oldest_ws_message_age_ms {}\n\
relay_core_rule_exec_errors_total {}\n\
relay_core_audit_events_total {}\n\
relay_core_audit_events_failed_total {}\n\
relay_core_flow_events_lagged_total {}\n\
relay_core_audit_events_lagged_total {}\n\
relay_core_proxy_body_degraded_total {}\n\
relay_core_proxy_http_request_total {}\n\
relay_core_proxy_sandbox_reject_total {}\n\
relay_core_proxy_invalid_status_total {}\n\
relay_core_proxy_circuit_rejected_total {}\n\
relay_core_proxy_flows_dropped_total {}\n\
relay_core_proxy_stream_mode_tap_total {}\n\
relay_core_proxy_stream_mode_degrade_total {}\n\
relay_core_proxy_bytes_sent_total {}\n\
relay_core_proxy_bytes_recv_total {}\n",
            self.flows_total,
            self.flows_in_memory,
            self.flows_dropped,
            self.intercepts_pending,
            self.ws_pending_messages,
            oldest_intercept_age_ms,
            oldest_ws_message_age_ms,
            self.rule_exec_errors,
            self.audit_events_total,
            self.audit_events_failed,
            self.flow_events_lagged_total,
            self.audit_events_lagged_total,
            self.proxy_body_degraded_total,
            self.proxy_http_request_total,
            self.proxy_sandbox_reject_total,
            self.proxy_invalid_status_total,
            self.proxy_circuit_rejected_total,
            self.proxy_flows_dropped_total,
            self.proxy_stream_mode_tap_total,
            self.proxy_stream_mode_degrade_total,
            self.proxy_bytes_sent_total,
            self.proxy_bytes_recv_total,
        )
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CoreStatusSnapshot {
    pub phase: RuntimeLifecyclePhase,
    pub running: bool,
    pub port: Option<u16>,
    pub uptime: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CoreStatusReport {
    pub status: CoreStatusSnapshot,
    pub metrics: CoreMetrics,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PendingInterceptItem {
    pub key: String,
    pub flow_id: String,
    pub phase: String,
    pub url: String,
    pub method: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CoreInterceptSnapshot {
    pub pending_count: usize,
    pub ws_pending_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<PendingInterceptItem>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CoreAuditSnapshot {
    pub events: Vec<AuditEvent>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CoreAuditQuery {
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub actor: Option<AuditActor>,
    pub kind: Option<AuditEventKind>,
    pub outcome: Option<AuditOutcome>,
    pub limit: usize,
}

impl Default for CoreAuditQuery {
    fn default() -> Self {
        Self {
            since_ms: None,
            until_ms: None,
            actor: None,
            kind: None,
            outcome: None,
            limit: 50,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecyclePhase {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl RuntimeLifecyclePhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RuntimeLifecycle {
    pub phase: RuntimeLifecyclePhase,
    pub port: Option<u16>,
    pub started_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

impl RuntimeLifecycle {
    pub fn created() -> Self {
        Self {
            phase: RuntimeLifecyclePhase::Created,
            port: None,
            started_at_ms: None,
            last_error: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.phase.is_active()
    }

    pub fn uptime_seconds(&self) -> Option<u64> {
        let started_at_ms = self.started_at_ms?;
        let now_ms = now_unix_ms();
        Some(now_ms.saturating_sub(started_at_ms) / 1_000)
    }
}

pub enum ProxySpawnResult {
    Started(tokio::task::JoinHandle<()>),
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyStopResult {
    Stopping,
    NotRunning,
}

impl From<RuntimeLifecycle> for CoreStatusSnapshot {
    fn from(lifecycle: RuntimeLifecycle) -> Self {
        Self {
            running: lifecycle.is_active(),
            uptime: lifecycle.uptime_seconds(),
            port: lifecycle.port,
            last_error: lifecycle.last_error,
            phase: lifecycle.phase,
        }
    }
}

pub struct CoreState {
    flow_store: mpsc::Sender<FlowStoreMessage>,
    intercept_broker: mpsc::Sender<InterceptBrokerMessage>,
    rule_store: mpsc::Sender<RuleStoreMessage>,
    store: Option<Store>,
    #[cfg(feature = "script")]
    pub script_interceptor: Arc<ScriptInterceptor>,
    /// Source of the script that last loaded successfully. A failed reload leaves this in place.
    #[cfg(feature = "script")]
    loaded_script: std::sync::RwLock<Option<String>>,
    pub policy_tx: watch::Sender<ProxyPolicy>,
    /// Redaction view used when persisting, kept in step with `policy_tx`'s redaction section.
    pub(crate) redaction_handle: Arc<std::sync::RwLock<RedactionPolicy>>,
    /// Connection string for the store, kept so retention can prune in the background.
    db_url: Option<String>,
    /// How much history to keep. Seeded from [`ProxyPolicy::default`], so a host that never
    /// touches policy still prunes. An explicit unbounded policy turns pruning into a no-op.
    retention: Arc<std::sync::RwLock<relay_core_storage::RetentionPolicy>>,
    /// The background prune loop is started at most once; later policy edits only update `retention`.
    retention_task_started: AtomicBool,
    pub flows_dropped: Arc<AtomicUsize>,
    audit_events_total: Arc<AtomicUsize>,
    audit_events_failed: Arc<AtomicUsize>,
    flow_events_lagged_total: Arc<AtomicUsize>,
    audit_events_lagged_total: Arc<AtomicUsize>,
    /// 内部广播 channel：Tauri、Probe 等多个消费者均可订阅
    flow_broadcast_tx: broadcast::Sender<FlowUpdate>,
    /// Typed lifecycle transitions (roadmap §4-4). Additive to the snapshot channel above: a
    /// consumer that needs "what just happened" no longer has to diff successive snapshots.
    flow_event_broadcast_tx: broadcast::Sender<FlowEvent>,
    audit_broadcast_tx: broadcast::Sender<AuditEvent>,
    audit_history: Arc<Mutex<VecDeque<AuditEvent>>>,
    lifecycle: LifecycleManager,
    /// Port last written into `capture_exclude` because the proxy was listening there. `0` means
    /// none. Kept so stopping or moving the proxy removes that port and no other entry.
    proxy_exclude_port: AtomicU16,
}

/// Removes the proxy listen port from `capture_exclude` when `run_proxy` returns.
struct ClearProxyExclude {
    state: Arc<CoreState>,
}

impl Drop for ClearProxyExclude {
    fn drop(&mut self) {
        self.state.set_listening_proxy_port(None);
    }
}

fn storage_retention(
    retention: &relay_core_api::policy::RetentionPolicy,
) -> relay_core_storage::RetentionPolicy {
    relay_core_storage::RetentionPolicy {
        max_flows: retention.max_flows,
        max_age_secs: retention.max_age_secs,
        max_audit_events: retention.max_audit_events,
    }
}

impl CoreState {
    pub async fn new(db_url: Option<String>) -> Self {
        const AUDIT_HISTORY_LIMIT: usize = 200;

        let store = if let Some(url) = db_url.clone() {
            match Store::connect(&url).await {
                Ok(s) => {
                    if let Err(e) = s.init().await {
                        tracing::error!("Failed to init store: {}", e);
                    }
                    Some(s)
                }
                Err(e) => {
                    tracing::error!("Failed to connect to store: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let (flow_tx, flow_rx) = mpsc::channel(10000);
        // Persistence-time redaction. Seeded from the default policy and updated by
        // `update_policy_from`, which is the single place a policy change is applied.
        let redaction_handle = Arc::new(std::sync::RwLock::new(RedactionPolicy::default()));
        let flow_actor = FlowStoreActor::new(flow_rx, store.clone(), redaction_handle.clone());
        tokio::spawn(flow_actor.run());

        let (intercept_tx, intercept_rx) = mpsc::channel(1000);
        let intercept_actor = InterceptBrokerActor::new(intercept_rx);
        tokio::spawn(intercept_actor.run());

        let (rule_tx, rule_rx) = mpsc::channel(100);
        let rule_actor = RuleStoreActor::new(rule_rx, store.clone());
        tokio::spawn(rule_actor.run());

        let (policy_tx, _) = watch::channel(ProxyPolicy::default());
        let (flow_broadcast_tx, _) = broadcast::channel(1000);
        let (flow_event_broadcast_tx, _) = broadcast::channel(1000);
        let (audit_broadcast_tx, _) = broadcast::channel(256);

        #[cfg(feature = "script")]
        let script_interceptor = ScriptInterceptor::new()
            .await
            .expect("Failed to initialize ScriptInterceptor");
        let state = Self {
            flow_store: flow_tx,
            intercept_broker: intercept_tx,
            rule_store: rule_tx,
            store,
            #[cfg(feature = "script")]
            script_interceptor: Arc::new(script_interceptor),
            #[cfg(feature = "script")]
            loaded_script: std::sync::RwLock::new(None),
            policy_tx,
            redaction_handle,
            db_url,
            retention: Arc::new(std::sync::RwLock::new(storage_retention(
                &ProxyPolicy::default().retention,
            ))),
            retention_task_started: AtomicBool::new(false),
            flows_dropped: Arc::new(AtomicUsize::new(0)),
            audit_events_total: Arc::new(AtomicUsize::new(0)),
            audit_events_failed: Arc::new(AtomicUsize::new(0)),
            flow_events_lagged_total: Arc::new(AtomicUsize::new(0)),
            audit_events_lagged_total: Arc::new(AtomicUsize::new(0)),
            flow_broadcast_tx,
            flow_event_broadcast_tx,
            audit_broadcast_tx,
            audit_history: Arc::new(Mutex::new(VecDeque::with_capacity(AUDIT_HISTORY_LIMIT))),
            lifecycle: LifecycleManager::new(),
            proxy_exclude_port: AtomicU16::new(0),
        };
        state.ensure_retention_task();
        state
    }

    pub async fn get_metrics(&self) -> CoreMetrics {
        let (flow_tx, flow_rx) = oneshot::channel();
        let _ = self
            .flow_store
            .send(FlowStoreMessage::GetMetrics(flow_tx))
            .await;
        let (flows_total, flows_in_memory) = flow_rx.await.unwrap_or((0, 0));

        let (int_tx, int_rx) = oneshot::channel();
        let _ = self
            .intercept_broker
            .send(InterceptBrokerMessage::GetMetrics { respond_to: int_tx })
            .await;
        let (
            intercepts_pending,
            ws_pending_messages,
            oldest_intercept_age_ms,
            oldest_ws_message_age_ms,
        ) = int_rx.await.unwrap_or((0, 0, None, None));

        let (rule_tx, rule_rx) = oneshot::channel();
        let _ = self
            .rule_store
            .send(RuleStoreMessage::GetMetrics(rule_tx))
            .await;
        let rule_exec_errors = rule_rx.await.unwrap_or(0);

        CoreMetrics {
            flows_total,
            flows_in_memory,
            flows_dropped: self.flows_dropped.load(Ordering::Relaxed),
            intercepts_pending,
            ws_pending_messages,
            oldest_intercept_age_ms,
            oldest_ws_message_age_ms,
            rule_exec_errors,
            audit_events_total: self.audit_events_total.load(Ordering::Relaxed),
            audit_events_failed: self.audit_events_failed.load(Ordering::Relaxed),
            flow_events_lagged_total: self.flow_events_lagged_total.load(Ordering::Relaxed),
            audit_events_lagged_total: self.audit_events_lagged_total.load(Ordering::Relaxed),
            proxy_body_degraded_total: relay_core_lib::metrics::get_proxy_body_degraded(),
            proxy_http_request_total: relay_core_lib::metrics::get_proxy_http_request(),
            proxy_sandbox_reject_total: relay_core_lib::metrics::get_proxy_sandbox_reject(),
            proxy_invalid_status_total: relay_core_lib::metrics::get_proxy_invalid_status(),
            proxy_circuit_rejected_total: relay_core_lib::metrics::get_circuit_rejected(),
            proxy_flows_dropped_total: relay_core_lib::metrics::get_flows_dropped(),
            proxy_stream_mode_tap_total: relay_core_lib::metrics::get_proxy_stream_mode_tap(),
            proxy_stream_mode_degrade_total: relay_core_lib::metrics::get_proxy_stream_mode_degrade(
            ),
            proxy_bytes_sent_total: relay_core_lib::metrics::get_bytes_sent(),
            proxy_bytes_recv_total: relay_core_lib::metrics::get_bytes_recv(),
        }
    }

    pub async fn get_metrics_prometheus_text(&self) -> String {
        let mut text = self.get_metrics().await.to_prometheus_text();
        #[cfg(feature = "script")]
        {
            text.push_str(&self.script_interceptor.metrics.prometheus_lines());
        }
        text
    }

    pub fn status_snapshot(&self) -> CoreStatusSnapshot {
        self.lifecycle().into()
    }

    pub async fn status_report(&self) -> CoreStatusReport {
        CoreStatusReport {
            status: self.status_snapshot(),
            metrics: self.get_metrics().await,
        }
    }

    pub async fn intercept_snapshot(&self) -> CoreInterceptSnapshot {
        let metrics = self.get_metrics().await;
        let items = self.list_pending_intercept_items().await;
        CoreInterceptSnapshot {
            pending_count: metrics.intercepts_pending,
            ws_pending_count: metrics.ws_pending_messages,
            items,
        }
    }

    pub async fn list_pending_intercept_items(&self) -> Vec<PendingInterceptItem> {
        let keys = self.list_pending_intercept_keys().await;
        let mut items = Vec::with_capacity(keys.len());
        for (key, created_at_ms) in keys {
            let (flow_id, phase) = parse_intercept_key(&key);
            let (url, method) = if let Some(flow) = self.get_flow(flow_id.clone()).await {
                flow_url_method(&flow)
            } else {
                (String::new(), String::new())
            };
            items.push(PendingInterceptItem {
                key,
                flow_id,
                phase,
                url,
                method,
                created_at_ms,
            });
        }
        items
    }

    async fn list_pending_intercept_keys(&self) -> Vec<(String, u64)> {
        let (tx, rx) = oneshot::channel();
        if self
            .intercept_broker
            .send(InterceptBrokerMessage::ListPendingIntercepts { respond_to: tx })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub fn audit_snapshot(&self, limit: usize) -> CoreAuditSnapshot {
        let events = self.recent_audit_events();
        let start = events.len().saturating_sub(limit);
        CoreAuditSnapshot {
            events: events.into_iter().skip(start).collect(),
        }
    }

    /// Audit events matching `query`, newest first.
    ///
    /// Persisted events and the in-memory tail are **merged**, because persistence is asynchronous:
    /// reading only the store means an event that happened a millisecond ago is invisible, and a
    /// caller that just caused it is told "nothing happened". The in-memory history also holds the
    /// newest events when no store is configured at all.
    pub async fn query_audit_snapshot(&self, query: CoreAuditQuery) -> CoreAuditSnapshot {
        let limit = query.limit.clamp(1, 500);

        let mut events: Vec<AuditEvent> = Vec::new();
        if let Some(store) = &self.store {
            let rows = store
                .query_audit_events(
                    query.since_ms,
                    query.until_ms,
                    query.actor.as_ref().map(AuditActor::as_str),
                    query.kind.as_ref().map(AuditEventKind::as_str),
                    query.outcome.as_ref().map(AuditOutcome::as_str),
                    // A store read is capped by the same limit, so the in-memory tail can be added
                    // without the result growing past what the caller asked for.
                    limit,
                )
                .await
                .unwrap_or_default();

            for row in rows {
                if let Ok(event) = serde_json::from_value::<AuditEvent>(row) {
                    events.push(event);
                }
            }
        }

        let mut in_memory: Vec<AuditEvent> = self
            .recent_audit_events()
            .into_iter()
            .filter(|event| {
                query
                    .since_ms
                    .map(|v| event.timestamp_ms >= v)
                    .unwrap_or(true)
            })
            .filter(|event| {
                query
                    .until_ms
                    .map(|v| event.timestamp_ms <= v)
                    .unwrap_or(true)
            })
            .filter(|event| {
                query
                    .actor
                    .as_ref()
                    .map(|v| &event.actor == v)
                    .unwrap_or(true)
            })
            .filter(|event| {
                query
                    .kind
                    .as_ref()
                    .map(|v| &event.kind == v)
                    .unwrap_or(true)
            })
            .filter(|event| {
                query
                    .outcome
                    .as_ref()
                    .map(|v| &event.outcome == v)
                    .unwrap_or(true)
            })
            .collect();

        // Oldest-to-newest in memory; reverse for the newest-first order callers expect.
        in_memory.reverse();
        events.extend(in_memory);

        // An event can be in both places once its write lands; keep one copy of each.
        let mut seen = std::collections::HashSet::new();
        events.retain(|event| seen.insert(event.id.clone()));

        events.sort_by_key(|event| std::cmp::Reverse(event.timestamp_ms));
        events.truncate(limit);

        CoreAuditSnapshot { events }
    }

    pub async fn get_flow(&self, id: String) -> Option<Flow> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .flow_store
            .send(FlowStoreMessage::GetFlow {
                id: id.clone(),
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send GetFlow request: {}", e);
            if let Some(store) = &self.store {
                return store
                    .load_flow(&id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_value::<Flow>(value).ok());
            }
            return None;
        }
        let flow = rx
            .await
            .map_err(|e| {
                error!("Failed to receive Flow response: {}", e);
                e
            })
            .unwrap_or(None);
        if let Some(flow) = flow {
            return Some(redact_flow(flow, &self.current_redaction_policy()));
        }
        if let Some(store) = &self.store {
            return store
                .load_flow(&id)
                .await
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_value::<Flow>(value).ok())
                .map(|flow| redact_flow(flow, &self.current_redaction_policy()));
        }
        None
    }

    pub async fn get_rules(&self) -> Vec<Rule> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.rule_store.send(RuleStoreMessage::GetRules(tx)).await {
            error!("Failed to send GetRules request: {}", e);
            return Vec::new();
        }
        rx.await
            .map_err(|e| {
                error!("Failed to receive Rules response: {}", e);
                e
            })
            .unwrap_or_default()
    }

    pub async fn set_rules(&self, rules: Vec<Rule>) {
        let _ = self
            .set_rules_from(
                AuditActor::Runtime,
                "rules.replace",
                "rule_set".to_string(),
                json!({}),
                rules,
            )
            .await;
    }

    pub async fn upsert_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: serde_json::Value,
        rule: Rule,
    ) -> Result<(), String> {
        let rule_id = rule.id.clone();
        let mut rules = self.get_rules().await;
        rules.retain(|existing| existing.id != rule_id);
        rules.push(rule);
        self.set_rules_from(actor, operation, target, details, rules)
            .await
    }

    /// Count a rule action failure so it is visible in metrics, not only in a discarded trace.
    pub fn report_rule_exec_error(&self) {
        if let Err(e) = self.rule_store.try_send(RuleStoreMessage::ReportExecError) {
            debug!("Failed to report rule exec error: {}", e);
        }
    }

    pub async fn delete_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: serde_json::Value,
        rule_id: &str,
    ) -> Result<bool, String> {
        let mut rules = self.get_rules().await;
        let before = rules.len();
        rules.retain(|rule| rule.id != rule_id);
        if rules.len() == before {
            return Ok(false);
        }
        self.set_rules_from(actor, operation, target, details, rules)
            .await
            .map(|_| true)
    }

    pub async fn create_mock_response_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: serde_json::Value,
        config: MockResponseRuleConfig,
    ) -> Result<String, String> {
        let rule_id = config.rule_id.clone();
        let rule = build_mock_response_rule(config);
        self.upsert_rule_from(actor, "rule.mock_create", target, details, rule)
            .await
            .map(|_| rule_id)
    }

    pub async fn create_intercept_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: serde_json::Value,
        config: InterceptRuleConfig,
    ) -> Result<String, String> {
        let rule_id = config.rule_id.clone();
        let mut rules = self.get_rules().await;
        // Replace rather than append. Appending on a repeated call left two rules sharing an id — or
        // `{id}-0` / `{id}-1` for a two-stage rule — which made them indistinguishable and impossible
        // to remove by id. A caller re-sending an id means "this rule", not "one more like it".
        let stage_prefix = format!("{rule_id}-");
        rules.retain(|existing| existing.id != rule_id && !existing.id.starts_with(&stage_prefix));
        rules.extend(build_intercept_rules(config));
        self.set_rules_from(actor, "rule.intercept_create", target, details, rules)
            .await
            .map(|_| rule_id)
    }

    pub async fn upsert_legacy_intercept_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: serde_json::Value,
        rule: InterceptRule,
    ) -> Result<(), String> {
        let rule_id = rule.id.clone();
        let mut rules = self.get_rules().await;
        rules.retain(|existing| {
            existing.id != rule_id && !existing.id.starts_with(&format!("{}-", rule_id))
        });
        rules.extend(rule.to_rules());
        self.set_rules_from(
            actor,
            "rule.intercept_legacy_upsert",
            target,
            details,
            rules,
        )
        .await
    }

    pub async fn set_rules_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: serde_json::Value,
        rules: Vec<Rule>,
    ) -> Result<(), String> {
        let rule_count = rules.len();

        // Reject unusable rules before they are stored. Compilation degrades an invalid regex, glob
        // or CIDR into a sentinel that never matches, so a rule with a typo used to be accepted,
        // reported as enabled, and silently do nothing (roadmap §24.6). Validating here covers every
        // path into the rule store, and reporting *all* problems at once avoids a fix-one-at-a-time
        // loop for the caller.
        let report = relay_core_lib::rule::engine::loader::validate_rules(rules.iter());

        // Warnings do not block: a rule staged before its actions are configured is a normal
        // workflow, but it is worth recording so "enabled but doing nothing" is diagnosable.
        for warning in &report.warnings {
            tracing::warn!("Rule validation warning: {}", warning);
        }

        if !report.is_ok() {
            let summary = report
                .errors
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ");

            self.record_audit_event(AuditEvent::new(
                actor,
                AuditEventKind::RuleChanged,
                target,
                AuditOutcome::Failed,
                json!({
                    "operation": operation,
                    "rule_count": rule_count,
                    "details": details,
                    "error": summary,
                    "rejected": report.errors.len(),
                    "warnings": report.warnings.len(),
                }),
            ));

            return Err(format!("rule validation failed: {summary}"));
        }

        if let Err(e) = self
            .rule_store
            .send(RuleStoreMessage::SetRules(rules))
            .await
        {
            error!("Failed to send SetRules request: {}", e);
            self.record_audit_event(AuditEvent::new(
                actor,
                AuditEventKind::RuleChanged,
                target,
                AuditOutcome::Failed,
                json!({
                    "operation": operation,
                    "rule_count": rule_count,
                    "details": details,
                    "error": e.to_string()
                }),
            ));
            return Err(e.to_string());
        }

        self.record_audit_event(AuditEvent::new(
            actor,
            AuditEventKind::RuleChanged,
            target,
            AuditOutcome::Success,
            json!({
                "operation": operation,
                "rule_count": rule_count,
                "details": details
            }),
        ));
        Ok(())
    }

    pub async fn set_legacy_rules(&self, rules: Vec<InterceptRule>) {
        let mut new_rules = Vec::new();
        for rule in rules {
            new_rules.extend(rule.to_rules());
        }
        self.set_rules(new_rules).await;
    }

    pub async fn get_rule_engine(&self) -> Arc<RuleEngine> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .rule_store
            .send(RuleStoreMessage::GetRuleEngine(tx))
            .await
        {
            error!("Failed to send GetRuleEngine request: {}", e);
            return Arc::new(RuleEngine::new(Vec::new(), Vec::new(), None, None));
        }
        rx.await
            .map_err(|e| {
                error!("Failed to receive RuleEngine response: {}", e);
                e
            })
            .unwrap_or_else(|_| Arc::new(RuleEngine::new(Vec::new(), Vec::new(), None, None)))
    }

    /// Retroactively apply the current redaction policy to history already on disk.
    ///
    /// Enabling redaction used to affect only new writes, so a database that had run without it kept
    /// its original secrets indefinitely. Returns how many rows were rewritten, and does nothing when
    /// redaction is disabled — a pass that would change nothing is not worth the IO.
    pub async fn redact_stored_history(&self) -> Option<u64> {
        let url = self.db_url.clone()?;
        let policy = self.current_redaction_policy();
        if !policy.enabled {
            return Some(0);
        }

        let store = match Store::connect(&url).await {
            Ok(store) => store,
            Err(e) => {
                tracing::error!("Retroactive redaction: could not open the store: {}", e);
                return None;
            }
        };

        Self::redact_history_with(&store, &policy).await
    }
    /// Rewrite stored history under a redaction policy.
    ///
    /// Shared by the explicit [`CoreState::redact_stored_history`] entry point and by
    /// [`CoreState::update_policy_from`], which runs it the moment redaction is switched on —
    /// otherwise enabling redaction would only protect writes made *after* the switch, leaving every
    /// secret already on disk readable forever.
    async fn redact_history_with(store: &Store, policy: &RedactionPolicy) -> Option<u64> {
        if !policy.enabled {
            return Some(0);
        }

        // Reuse the exact helpers the output and persistence paths use, so the three cannot diverge.
        let flows = store
            .redact_existing_flows(|value| {
                match serde_json::from_value::<Flow>(value.clone()) {
                    Ok(flow) => serde_json::to_value(redact_flow(flow, policy)).unwrap_or_default(),
                    // Unreadable row: leave it untouched rather than replacing it with null.
                    Err(_) => value.clone(),
                }
            })
            .await;

        let summaries = store
            .redact_existing_flow_summaries(|value| {
                match serde_json::from_value::<FlowSummary>(value.clone()) {
                    Ok(summary) => serde_json::to_value(redact_flow_summary(summary, policy))
                        .unwrap_or_default(),
                    Err(_) => value.clone(),
                }
            })
            .await;

        match (flows, summaries) {
            (Ok(flows), Ok(summaries)) => {
                tracing::info!(
                    "Retroactive redaction rewrote {} flows and {} summaries",
                    flows,
                    summaries
                );
                Some(flows + summaries)
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::error!("Retroactive redaction failed: {}", e);
                None
            }
        }
    }

    /// Apply the policy's storage bounds.
    ///
    /// The default policy is already bounded. An unbounded value is written through as well, so a
    /// host can turn pruning off; the background task notices and skips the pass.
    fn apply_retention_from(&self, retention: &relay_core_api::policy::RetentionPolicy) {
        self.set_retention_policy(storage_retention(retention));
    }

    pub fn set_retention_policy(&self, policy: relay_core_storage::RetentionPolicy) {
        if let Ok(mut current) = self.retention.write() {
            *current = policy;
        }
        self.ensure_retention_task();
    }

    /// Start the prune loop the first time a bound exists. Later updates only change the policy
    /// the loop already reads.
    fn ensure_retention_task(&self) {
        if self.retention_policy().is_unbounded() {
            return;
        }
        if self.retention_task_started.swap(true, Ordering::Relaxed) {
            return;
        }
        self.spawn_retention_task();
    }

    /// The active retention policy.
    pub fn retention_policy(&self) -> relay_core_storage::RetentionPolicy {
        self.retention
            .read()
            .map(|p| *p)
            .unwrap_or_else(|_| relay_core_storage::RetentionPolicy::unbounded())
    }

    /// Run one prune pass now, returning what was removed. Useful for tests and for an operator
    /// triggered cleanup.
    pub async fn prune_now(&self) -> Option<relay_core_storage::PrunedCounts> {
        let url = self.db_url.clone()?;
        let policy = self.retention_policy();
        if policy.is_unbounded() {
            return None;
        }

        let store = match Store::connect(&url).await {
            Ok(store) => store,
            Err(e) => {
                tracing::error!("Retention: could not open the store: {}", e);
                return None;
            }
        };

        match store.prune(policy).await {
            Ok(counts) => {
                if counts.total() > 0 {
                    tracing::info!(
                        "Retention pruned {} flows, {} summaries, {} audit events",
                        counts.flows,
                        counts.flow_summaries,
                        counts.audit_events
                    );
                }
                Some(counts)
            }
            Err(e) => {
                tracing::error!("Retention prune failed: {}", e);
                None
            }
        }
    }

    fn spawn_retention_task(&self) {
        let Some(url) = self.db_url.clone() else {
            return;
        };
        let retention = self.retention.clone();

        tokio::spawn(async move {
            // A short initial delay avoids competing with startup, and the interval keeps the work
            // predictable rather than tied to traffic.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                ticker.tick().await;

                let policy = retention
                    .read()
                    .map(|p| *p)
                    .unwrap_or_else(|_| relay_core_storage::RetentionPolicy::unbounded());
                if policy.is_unbounded() {
                    continue;
                }

                match Store::connect(&url).await {
                    Ok(store) => {
                        if let Err(e) = store.prune(policy).await {
                            tracing::error!("Retention prune failed: {}", e);
                        }
                    }
                    Err(e) => tracing::error!("Retention: could not open the store: {}", e),
                }
            }
        });
    }

    pub fn update_policy(&self, policy: ProxyPolicy) {
        self.update_policy_from(AuditActor::Runtime, "policy".to_string(), policy);
    }

    pub fn patch_policy_from(&self, actor: AuditActor, target: String, patch: ProxyPolicyPatch) {
        let mut policy = self.policy_snapshot();
        policy.apply_patch(patch);
        self.update_policy_from(actor, target, policy);
    }

    pub fn policy_snapshot(&self) -> ProxyPolicy {
        self.policy_tx.borrow().clone()
    }

    pub fn update_policy_from(&self, actor: AuditActor, target: String, policy: ProxyPolicy) {
        let details = json!({
            "strict_http_semantics": policy.strict_http_semantics,
            "request_timeout_ms": policy.request_timeout_ms,
            "max_body_size": policy.max_body_size,
            "transparent_enabled": policy.transparent_enabled,
            "redaction_enabled": policy.redaction.enabled,
            "redact_bodies": policy.redaction.redact_bodies
        });

        // Read the previous value before overwriting it: turning redaction *on* has to cover what is
        // already on disk, or the secrets written under the old policy stay readable forever.
        let redaction_was_enabled = self.current_redaction_policy().enabled;
        let redaction_now_enabled = policy.redaction.enabled;

        // Keep the persistence-time redaction view in step with the output-time view.
        if let Ok(mut current) = self.redaction_handle.write() {
            *current = policy.redaction.clone();
        }
        let effective_policy = policy.clone();
        self.policy_tx.send_replace(policy);

        // Retention travels with policy, so a host that can set policy can bound storage. Applying
        // it here (rather than behind a dedicated command) is what makes the bound reachable at all.
        self.apply_retention_from(&effective_policy.retention);

        if should_redact_history(redaction_was_enabled, redaction_now_enabled)
            && let Some(store) = self.store.clone()
            && tokio::runtime::Handle::try_current().is_ok()
        {
            let redaction = effective_policy.redaction;
            tokio::spawn(async move {
                if let Some(rows) = CoreState::redact_history_with(&store, &redaction).await {
                    tracing::info!(
                        "Redaction enabled: rewrote {} previously stored row(s)",
                        rows
                    );
                }
            });
        }
        self.record_audit_event(AuditEvent::new(
            actor,
            AuditEventKind::PolicyUpdated,
            target,
            AuditOutcome::Success,
            details,
        ));
    }

    pub async fn register_intercept(&self, key: String, tx: oneshot::Sender<InterceptionResult>) {
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::RegisterIntercept { key, tx })
            .await
        {
            error!("Failed to send RegisterIntercept request: {}", e);
        }
    }

    pub async fn resolve_intercept(
        &self,
        key: String,
        result: InterceptionResult,
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::ResolveIntercept {
                key,
                result,
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send ResolveIntercept request: {}", e);
            return Err(e.to_string());
        }
        rx.await.map_err(|_| "Actor dropped".to_string())?
    }

    pub async fn get_pending_ws_message(&self, key: String) -> Option<WebSocketMessage> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::GetPendingWebSocketMessage {
                key,
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send GetPendingWebSocketMessage request: {}", e);
            return None;
        }
        rx.await
            .map_err(|e| {
                error!("Failed to receive WebSocketMessage response: {}", e);
                e
            })
            .unwrap_or(None)
    }

    pub async fn set_pending_ws_message(&self, key: String, message: WebSocketMessage) {
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::SetPendingWebSocketMessage { key, message })
            .await
        {
            error!("Failed to send SetPendingWebSocketMessage request: {}", e);
        }
    }

    pub async fn is_intercept_pending(&self, key: String) -> bool {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::GetPendingIntercept {
                key,
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send GetPendingIntercept request: {}", e);
            return false;
        }
        rx.await
            .map_err(|e| {
                error!("Failed to receive PendingIntercept response: {}", e);
                e
            })
            .unwrap_or(false)
    }

    pub async fn is_flow_intercepted(&self, flow_id: String) -> bool {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .intercept_broker
            .send(InterceptBrokerMessage::GetPendingInterceptByFlowId {
                flow_id,
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send GetPendingInterceptByFlowId request: {}", e);
            return false;
        }
        rx.await
            .map_err(|e| {
                error!("Failed to receive PendingInterceptByFlowId response: {}", e);
                e
            })
            .unwrap_or(false)
    }

    pub fn upsert_flow(&self, flow: Box<Flow>) {
        if let Err(e) = self.flow_store.try_send(FlowStoreMessage::UpsertFlow(flow)) {
            // Drop flow if channel is full
            error!("FlowStore dropped flow: {}", e);
            self.flows_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn append_ws_message(&self, flow_id: String, message: WebSocketMessage) {
        if let Err(e) = self
            .flow_store
            .try_send(FlowStoreMessage::AppendWebSocketMessage { flow_id, message })
        {
            error!("FlowStore dropped WS message: {}", e);
            self.flows_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn update_http_body(&self, flow_id: String, body: BodyData, direction: Direction) {
        if let Err(e) = self.flow_store.try_send(FlowStoreMessage::UpdateHttpBody {
            flow_id,
            body,
            direction,
        }) {
            error!("FlowStore dropped HTTP body: {}", e);
            self.flows_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record trailers that arrived after the response body.
    ///
    /// They arrive last, so they cannot be part of the original snapshot: a capture without them
    /// shows that a gRPC call happened but not whether it succeeded.
    pub fn set_response_trailers(&self, flow_id: String, trailers: Vec<(String, String)>) {
        if let Err(e) = self
            .flow_store
            .try_send(FlowStoreMessage::SetResponseTrailers { flow_id, trailers })
        {
            error!("FlowStore dropped response trailers: {}", e);
            self.flows_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// P1: Tag a flow as budget-exceeded (body too large for full rule inspection)
    pub fn tag_flow_budget_exceeded(&self, flow_id: String, direction: Direction) {
        if let Err(e) = self
            .flow_store
            .try_send(FlowStoreMessage::TagBudgetExceeded { flow_id, direction })
        {
            error!("FlowStore dropped budget-exceeded tag: {}", e);
            self.flows_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 订阅 FlowUpdate 广播。调用者获得独立 Receiver，lag 时自动跳过过期消息。
    pub fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate> {
        self.flow_broadcast_tx.subscribe()
    }

    /// Subscribe to typed lifecycle events. Independent receiver per consumer; a lagging consumer
    /// skips ahead rather than stalling the producer.
    pub fn subscribe_flow_events(&self) -> broadcast::Receiver<FlowEvent> {
        self.flow_event_broadcast_tx.subscribe()
    }

    /// Publish one lifecycle event.
    ///
    /// Never blocks and never fails the caller: with no subscribers this is a no-op, which keeps a
    /// headless run from paying for a channel nobody reads.
    pub fn publish_flow_event(&self, event: FlowEvent) {
        let _ = self.flow_event_broadcast_tx.send(event);
    }

    pub fn subscribe_audit_events(&self) -> broadcast::Receiver<AuditEvent> {
        self.audit_broadcast_tx.subscribe()
    }

    fn current_redaction_policy(&self) -> RedactionPolicy {
        self.policy_tx.borrow().redaction.clone()
    }

    pub fn record_flow_events_lagged(&self, skipped: u64) {
        self.flow_events_lagged_total
            .fetch_add(skipped as usize, Ordering::Relaxed);
    }

    pub fn record_audit_events_lagged(&self, skipped: u64) {
        self.audit_events_lagged_total
            .fetch_add(skipped as usize, Ordering::Relaxed);
    }

    pub fn lifecycle(&self) -> RuntimeLifecycle {
        self.lifecycle.snapshot()
    }

    pub fn subscribe_lifecycle(&self) -> watch::Receiver<RuntimeLifecycle> {
        self.lifecycle.subscribe()
    }

    fn prepare_start(&self, port: u16, shutdown_tx: oneshot::Sender<()>) -> Result<(), String> {
        self.lifecycle.prepare_start(port, shutdown_tx)
    }

    pub fn stop_proxy(&self) -> Result<ProxyStopResult, String> {
        self.lifecycle.stop()
    }

    pub fn recent_audit_events(&self) -> Vec<AuditEvent> {
        self.audit_history
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 搜索内存中的 Flow 列表，返回轻量摘要。
    pub async fn search_flows(&self, query: FlowQuery) -> Vec<FlowSummary> {
        let redaction = self.current_redaction_policy();
        if let Some(store) = &self.store {
            return store
                .query_flow_summaries(&query)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|summary| redact_flow_summary(summary, &redaction))
                .collect();
        }
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .flow_store
            .send(FlowStoreMessage::SearchFlows {
                query,
                respond_to: tx,
            })
            .await
        {
            error!("Failed to send SearchFlows request: {}", e);
            return Vec::new();
        }
        rx.await
            .unwrap_or_default()
            .into_iter()
            .map(|summary| redact_flow_summary(summary, &redaction))
            .collect()
    }

    /// Drop captured flows from memory and from the database. Rules and audit events stay.
    ///
    /// The delete runs on the flow actor so it cannot race an in-flight persist.
    pub async fn clear_captured_flows(&self) -> Result<(u64, u64), String> {
        let (tx, rx) = oneshot::channel();
        self.flow_store
            .send(FlowStoreMessage::ClearCaptured { respond_to: tx })
            .await
            .map_err(|e| format!("flow store is gone: {e}"))?;
        rx.await
            .map_err(|e| format!("flow store dropped the clear: {e}"))?
    }

    pub fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate {
        let redaction = self.current_redaction_policy();
        redact_flow_update(update, &redaction)
    }

    /// 解除截获并可选地应用修改。
    ///
    /// `action` 为 `"drop"` 时直接丢弃；其他值（含 `"continue"`）则：
    /// - 若 `mods` 为 None，直接 Continue；
    /// - 若 `mods` 存在，根据 `key` 格式决定修改请求/响应还是 WebSocket 消息。
    ///
    /// `key` 格式约定：
    /// - `"<flow_id>:<phase>"` — 修改请求或响应（phase 以 "request"/"response" 开头）
    /// - `"<flow_id>:ws_msg:<msg_id>"` — 修改 WebSocket 消息
    pub async fn resolve_intercept_with_modifications(
        &self,
        key: String,
        action: &str,
        mods: Option<relay_core_api::modification::FlowModification>,
    ) -> Result<(), String> {
        self.resolve_intercept_with_modifications_from(AuditActor::Runtime, key, action, mods)
            .await
    }

    pub async fn resolve_intercept_with_modifications_from(
        &self,
        actor: AuditActor,
        key: String,
        action: &str,
        mods: Option<relay_core_api::modification::FlowModification>,
    ) -> Result<(), String> {
        let modified_fields = modification_field_names(mods.as_ref());
        let result = match action {
            "drop" => InterceptionResult::Drop,
            _ => match mods {
                None => InterceptionResult::Continue,
                Some(m) => {
                    let parts: Vec<&str> = key.splitn(4, ':').collect();
                    match parts.as_slice() {
                        [flow_id, phase] => {
                            if let Some(flow) = self.get_flow(flow_id.to_string()).await {
                                modification::apply_flow_modification(&flow, phase, m)
                            } else {
                                InterceptionResult::Continue
                            }
                        }
                        // ws_msg 格式：<flow_id>:ws_msg:<msg_id>
                        // msg_id 本身是 UUID（含 -），不含 :，所以 splitn(4) 给出恰好 3 段
                        [_, "ws_msg", _] => {
                            if let Some(msg) = self.get_pending_ws_message(key.clone()).await {
                                modification::apply_ws_modification(&msg, m)
                            } else {
                                InterceptionResult::Continue
                            }
                        }
                        _ => InterceptionResult::Continue,
                    }
                }
            },
        };
        let outcome = self.resolve_intercept(key.clone(), result).await;
        let audit_outcome = if outcome.is_ok() {
            AuditOutcome::Success
        } else {
            AuditOutcome::Failed
        };
        let error_message = outcome.as_ref().err().cloned();

        self.record_audit_event(AuditEvent::new(
            actor,
            AuditEventKind::InterceptResolved,
            key,
            audit_outcome,
            json!({
                "action": action,
                "has_modifications": !modified_fields.is_empty(),
                "modified_fields": modified_fields,
                "error": error_message
            }),
        ));
        outcome
    }

    #[cfg(feature = "script")]
    pub async fn load_script_from(
        &self,
        actor: AuditActor,
        target: String,
        script: &str,
    ) -> Result<(), String> {
        let result = self
            .script_interceptor
            .load_script(script)
            .await
            .map_err(|e| e.to_string());

        let outcome = if result.is_ok() {
            AuditOutcome::Success
        } else {
            AuditOutcome::Failed
        };
        let error_message = result.as_ref().err().cloned();

        self.record_audit_event(AuditEvent::new(
            actor,
            AuditEventKind::ScriptReloaded,
            target,
            outcome,
            json!({
                "script_bytes": script.len(),
                "error": error_message
            }),
        ));

        if result.is_ok()
            && let Ok(mut loaded) = self.loaded_script.write()
        {
            *loaded = Some(script.to_string());
        }

        result
    }

    /// The script currently loaded, if one has loaded successfully.
    #[cfg(feature = "script")]
    pub fn current_script(&self) -> Option<String> {
        self.loaded_script
            .read()
            .ok()
            .and_then(|script| script.clone())
    }

    /// S5: Set the env var whitelist for relay.env() in user scripts.
    #[cfg(feature = "script")]
    pub async fn set_script_env_allow(&self, env_allow: std::collections::HashSet<String>) {
        self.script_interceptor.set_env_allow(env_allow).await;
    }

    /// Allow `relay.fetch` to reach the given hosts (empty set means "any", once enabled).
    ///
    /// Both halves are needed: enabling with an empty allowlist means "any host", so a host that
    /// wants a narrow allowlist must pass it, and a host that wants none must leave this unset —
    /// `relay.fetch` stays disabled until a host asks for it.
    #[cfg(feature = "script")]
    pub async fn set_script_fetch_allow(
        &self,
        enabled: bool,
        allow_hosts: std::collections::HashSet<String>,
    ) {
        self.script_interceptor
            .set_fetch_allow(enabled, allow_hosts)
            .await;
    }

    pub(crate) fn record_audit_event(&self, event: AuditEvent) {
        const AUDIT_HISTORY_LIMIT: usize = 200;
        self.audit_events_total.fetch_add(1, Ordering::Relaxed);
        if event.outcome == AuditOutcome::Failed {
            self.audit_events_failed.fetch_add(1, Ordering::Relaxed);
        }

        let details = log_format::audit_details_log(&event.kind, &event.details);
        tracing::info!(
            target: "relay_core_audit",
            event_id = %event.id,
            actor = %event.actor.as_str(),
            kind = %event.kind.as_str(),
            target = %event.target,
            outcome = %event.outcome.as_str(),
            details = %details
        );

        if let Ok(mut history) = self.audit_history.lock() {
            if history.len() >= AUDIT_HISTORY_LIMIT {
                history.pop_front();
            }
            history.push_back(event.clone());
        }

        if let Some(store) = self.store.clone() {
            let event_json = serde_json::to_value(&event).unwrap_or_default();
            let event_id = event.id.clone();
            let timestamp_ms = event.timestamp_ms;
            let actor = event.actor.as_str().to_string();
            let kind = event.kind.as_str().to_string();
            let target = event.target.clone();
            let outcome = event.outcome.as_str().to_string();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(e) = store
                        .save_audit_event(AuditEventRecord {
                            id: &event_id,
                            timestamp_ms,
                            actor: &actor,
                            kind: &kind,
                            target: &target,
                            outcome: &outcome,
                            content: &event_json,
                        })
                        .await
                    {
                        tracing::error!("Failed to persist audit event: {}", e);
                    }
                });
            }
        }

        let _ = self.audit_broadcast_tx.send(event);
    }

    fn transition_to_running(&self, port: u16) {
        self.lifecycle.transition_to_running(port);
    }

    /// Record the port the proxy just bound, or clear it when the proxy stops.
    ///
    /// Only the port this process installed is removed. The control API, MCP, and entries an
    /// operator added stay in the list.
    fn set_listening_proxy_port(&self, port: Option<u16>) {
        let new = port.unwrap_or(0);
        let old = self.proxy_exclude_port.swap(new, Ordering::AcqRel);
        if old == new {
            return;
        }
        let mut policy = self.policy_snapshot();
        relay_core_api::policy::replace_proxy_capture_exclude(
            &mut policy.capture_exclude,
            (old != 0).then_some(old),
            port,
        );
        self.update_policy_from(AuditActor::Runtime, "proxy.listen".to_string(), policy);
    }

    /// Flip transparent mode without replacing the rest of the policy.
    ///
    /// A fresh [`ProxyPolicy::default`] here used to wipe `capture_exclude` and body observation
    /// at the moment the proxy started.
    fn apply_transparent_flag(&self, enabled: bool) {
        let mut policy = self.policy_snapshot();
        if policy.transparent_enabled == enabled {
            return;
        }
        policy.transparent_enabled = enabled;
        let target = if enabled {
            "proxy.transparent"
        } else {
            "proxy.standard"
        };
        self.update_policy_from(AuditActor::Runtime, target.to_string(), policy);
    }

    fn transition_to_stopped(&self) {
        self.lifecycle.transition_to_stopped();
    }

    fn transition_to_failed(&self, port: u16, error: String) {
        self.lifecycle.transition_to_failed(port, error);
    }

    pub fn spawn_proxy(
        self: &Arc<Self>,
        config: ProxyConfig,
        sink: mpsc::Sender<FlowUpdate>,
        extra_interceptor: Option<Arc<dyn Interceptor>>,
    ) -> Result<ProxySpawnResult, String> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        match self.prepare_start(config.port, shutdown_tx) {
            Ok(()) => {}
            Err(error) if error.contains("already") => return Ok(ProxySpawnResult::AlreadyRunning),
            Err(error) => return Err(error),
        }
        let state = self.clone();
        Ok(ProxySpawnResult::Started(tokio::spawn(async move {
            if let Err(error) = state
                .run_proxy(config, sink, extra_interceptor, shutdown_rx)
                .await
            {
                error!("Proxy failed: {}", error);
            }
        })))
    }

    pub async fn start_proxy(
        self: &Arc<Self>,
        config: ProxyConfig,
        sink: mpsc::Sender<FlowUpdate>,
        extra_interceptor: Option<Arc<dyn Interceptor>>,
    ) -> Result<(), String> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        self.prepare_start(config.port, shutdown_tx)?;
        self.run_proxy(config, sink, extra_interceptor, shutdown_rx)
            .await
    }

    async fn run_proxy(
        self: &Arc<Self>,
        config: ProxyConfig,
        sink: mpsc::Sender<FlowUpdate>,
        extra_interceptor: Option<Arc<dyn Interceptor>>,
        shutdown_rx: oneshot::Receiver<()>,
    ) -> Result<(), String> {
        let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
        let state = self.clone();

        if let Some(parent) = config.ca_cert_path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let ca = CertificateAuthority::load_or_create(&config.ca_cert_path, &config.ca_key_path)
            .map_err(|e| format!("Failed to load/create CA: {}", e))?;
        let ca = Arc::new(ca);

        #[cfg(feature = "script")]
        let script_interceptor = self.script_interceptor.clone();

        #[cfg(feature = "script")]
        let mut interceptors: Vec<Arc<dyn Interceptor>> = vec![script_interceptor];

        #[cfg(not(feature = "script"))]
        let mut interceptors: Vec<Arc<dyn Interceptor>> = vec![];

        interceptors.push(Arc::new(interceptors::rule::RuleInterceptor::new(
            self.clone(),
            self.clone(),
            self.clone(),
        )));

        interceptors.push(Arc::new(interceptors::metrics::MetricsInterceptor::new(
            self.clone(),
        )));

        if let Some(interceptor) = extra_interceptor {
            interceptors.push(interceptor);
        }

        let interceptor = Arc::new(CompositeInterceptor::new(interceptors));
        let (proxy_tx, mut proxy_rx) = mpsc::channel::<FlowUpdate>(1000);

        // Endpoints the host asked not to see in its own flow list. Applied here rather than at the
        // proxy's emit sites because this is the single point every update passes through: one check
        // covers all of them, including the incremental updates that carry only a flow id.
        // Read on each flow, not once at startup: the proxy port is written into the list only
        // after this task is spawned, when the listen socket has bound.
        tokio::spawn(async move {
            // Flow ids that matched an exclusion, so their body/trailer/message updates are skipped
            // too. Bounded: a long run must not accumulate ids forever, and the incremental updates
            // arrive while the id is still recent.
            let mut excluded_ids: std::collections::HashSet<uuid::Uuid> =
                std::collections::HashSet::new();
            let mut excluded_order: std::collections::VecDeque<uuid::Uuid> =
                std::collections::VecDeque::new();
            const EXCLUDED_ID_MEMORY: usize = 4096;

            while let Some(update) = proxy_rx.recv().await {
                // A request to an excluded endpoint is still forwarded — only the record of it is
                // dropped, because "do not show me my own UI's traffic" is a display concern, not a
                // reason to break the traffic.
                if let FlowUpdate::Full(flow) = &update {
                    let target = match &flow.layer {
                        relay_core_api::flow::Layer::Http(http) => Some(&http.request.url),
                        relay_core_api::flow::Layer::WebSocket(ws) => {
                            Some(&ws.handshake_request.url)
                        }
                        _ => None,
                    };
                    // Matched on the parsed URL the flow already carries, rather than stringifying it
                    // so the matcher can parse it again — this runs for every update.
                    let excluded = target.is_some_and(|url| {
                        let capture_exclude = state.policy_tx.borrow().capture_exclude.clone();
                        url.host_str().is_some_and(|host| {
                            relay_core_api::policy::is_capture_excluded_parts(
                                host,
                                url.port_or_known_default(),
                                &capture_exclude,
                            )
                        })
                    });
                    if excluded {
                        excluded_ids.insert(flow.id);
                        excluded_order.push_back(flow.id);
                        if excluded_order.len() > EXCLUDED_ID_MEMORY
                            && let Some(oldest) = excluded_order.pop_front()
                        {
                            excluded_ids.remove(&oldest);
                        }
                        continue;
                    }
                }

                let flow_id = match &update {
                    FlowUpdate::Full(flow) => Some(flow.id),
                    FlowUpdate::WebSocketMessage { flow_id, .. }
                    | FlowUpdate::HttpBody { flow_id, .. }
                    | FlowUpdate::BodyBudgetExceeded { flow_id, .. }
                    | FlowUpdate::ResponseTrailers { flow_id, .. } => {
                        uuid::Uuid::parse_str(flow_id).ok()
                    }
                };
                if flow_id.is_some_and(|id| excluded_ids.contains(&id)) {
                    continue;
                }

                match update.clone() {
                    FlowUpdate::Full(flow) => {
                        // A flow that recorded how it ended is finished, and the reason it gives is
                        // the fact the typed event model needs. Deriving this from the snapshot
                        // instead — "end_time is set, so call it complete" — would report every
                        // upstream failure and policy drop as a success.
                        if let Some(reason) = flow.close_reason.clone() {
                            let event = services::terminal_event(&flow, reason);
                            state.publish_flow_event(event);
                        }
                        state.upsert_flow(flow);
                    }
                    FlowUpdate::WebSocketMessage { flow_id, message } => {
                        state.append_ws_message(flow_id, message);
                    }
                    FlowUpdate::HttpBody {
                        flow_id,
                        direction,
                        body,
                    } => {
                        state.update_http_body(flow_id, body, direction);
                    }
                    FlowUpdate::BodyBudgetExceeded { flow_id, direction } => {
                        // P1: Tag the flow as budget-exceeded and update resilience trace
                        state.tag_flow_budget_exceeded(flow_id, direction);
                    }
                    FlowUpdate::ResponseTrailers { flow_id, trailers } => {
                        state.set_response_trailers(flow_id, trailers);
                    }
                }

                let _ = state.flow_broadcast_tx.send(update.clone());

                if sink.try_send(update).is_err() {
                    relay_core_lib::metrics::inc_flows_dropped();
                }
            }
        });

        if let Some(udp_port) = config.udp_tproxy_port {
            let udp_proxy_tx = proxy_tx.clone();
            let udp_interceptor = interceptor.clone();
            let udp_addr = SocketAddr::from(([0, 0, 0, 0], udp_port));
            tokio::spawn(async move {
                match tokio::net::UdpSocket::bind(udp_addr).await {
                    Ok(socket) => {
                        let proxy = UdpProxy::new(socket, std::time::Duration::from_secs(60))
                            .with_interceptor(udp_interceptor);
                        if let Err(e) = proxy.run(udp_proxy_tx).await {
                            error!("UDP TPROXY failed: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Failed to bind UDP TPROXY socket: {}", e);
                    }
                }
            });
        }

        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                let message = format!("Failed to bind to address {}: {}", addr, e);
                self.transition_to_failed(config.port, message.clone());
                return Err(message);
            }
        };

        self.transition_to_running(config.port);
        // The list follows the bound port. A later stop removes it, so a service on the old port
        // is captured again once this proxy is no longer listening there.
        self.set_listening_proxy_port(Some(config.port));
        let _clear_proxy_exclude = ClearProxyExclude {
            state: Arc::clone(self),
        };
        self.apply_transparent_flag(config.transparent);
        let shutdown_rx = Some(shutdown_rx);

        if config.transparent {
            let policy_rx = self.policy_tx.subscribe();

            let provider: Arc<dyn OriginalDstProvider> = {
                let mut addrs = BTreeSet::new();
                if let Ok(local) = listener.local_addr() {
                    addrs.insert(local);
                }

                #[cfg(all(target_os = "linux", feature = "transparent-linux"))]
                {
                    Arc::new(LinuxOriginalDstProvider::new(addrs))
                }
                #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
                {
                    match MacOsOriginalDstProvider::new(addrs.clone()) {
                        Ok(provider) => Arc::new(provider),
                        Err(e) => {
                            error!("Failed to initialize macOS PF provider: {}", e);
                            Arc::new(relay_core_lib::capture::NoOpOriginalDstProvider::new(addrs))
                        }
                    }
                }
                #[cfg(target_os = "windows")]
                {
                    let filter =
                        "outbound and !loopback and (tcp.DstPort == 80 or tcp.DstPort == 443)"
                            .to_string();
                    let port = config.port;
                    tokio::spawn(async move {
                        relay_core_lib::capture::windows::start_windivert_capture(filter, port)
                            .await;
                    });

                    Arc::new(WindowsOriginalDstProvider::new(addrs))
                }
                #[cfg(not(any(
                    all(target_os = "linux", feature = "transparent-linux"),
                    all(target_os = "macos", feature = "transparent-macos"),
                    target_os = "windows"
                )))]
                {
                    Arc::new(NoOpOriginalDstProvider::new(addrs))
                }
            };

            let source = TransparentTcpCaptureSource::new(listener, provider);
            let result = relay_core_lib::start_proxy(
                source,
                proxy_tx,
                interceptor,
                ca,
                policy_rx,
                None,
                shutdown_rx,
                Some(self.get_rule_engine().await),
            )
            .await
            .map_err(|e| e.to_string());
            if let Err(error) = &result {
                self.transition_to_failed(config.port, error.clone());
            } else {
                self.transition_to_stopped();
            }
            result
        } else {
            let source = TcpCaptureSource::new(listener);
            let policy_rx = self.policy_tx.subscribe();

            let result = relay_core_lib::start_proxy(
                source,
                proxy_tx,
                interceptor,
                ca,
                policy_rx,
                None,
                shutdown_rx,
                Some(self.get_rule_engine().await),
            )
            .await
            .map_err(|e| e.to_string());
            if let Err(error) = &result {
                self.transition_to_failed(config.port, error.clone());
            } else {
                self.transition_to_stopped();
            }
            result
        }
    }
}

fn redact_flow_update(update: FlowUpdate, redaction: &RedactionPolicy) -> FlowUpdate {
    match update {
        FlowUpdate::Full(flow) => FlowUpdate::Full(Box::new(redact_flow(*flow, redaction))),
        FlowUpdate::WebSocketMessage {
            flow_id,
            mut message,
        } => {
            message.content = redact_body(message.content, redaction);
            FlowUpdate::WebSocketMessage { flow_id, message }
        }
        FlowUpdate::HttpBody {
            flow_id,
            direction,
            body,
        } => FlowUpdate::HttpBody {
            flow_id,
            direction,
            body: redact_body(body, redaction),
        },
        FlowUpdate::BodyBudgetExceeded { flow_id, direction } => {
            FlowUpdate::BodyBudgetExceeded { flow_id, direction }
        }
        // `grpc-message` and friends are server-authored free text and can echo request content, so
        // they go through the same name-based screening as headers.
        FlowUpdate::ResponseTrailers { flow_id, trailers } => FlowUpdate::ResponseTrailers {
            flow_id,
            trailers: trailers
                .into_iter()
                .map(|(name, value)| {
                    let redacted = if redaction.enabled
                        && redaction
                            .sensitive_header_names
                            .iter()
                            .any(|n| n.eq_ignore_ascii_case(&name))
                    {
                        "[REDACTED]".to_string()
                    } else {
                        value
                    };
                    (name, redacted)
                })
                .collect(),
        },
    }
}

pub(crate) fn redact_flow(mut flow: Flow, redaction: &RedactionPolicy) -> Flow {
    if !redaction.enabled {
        return flow;
    }
    match &mut flow.layer {
        Layer::Http(http) => {
            redact_http_request(&mut http.request, redaction);
            if let Some(response) = &mut http.response {
                redact_headers(&mut response.headers, redaction);
                response.body = response
                    .body
                    .take()
                    .map(|body| redact_body(body, redaction));
            }
        }
        Layer::WebSocket(ws) => {
            redact_http_request(&mut ws.handshake_request, redaction);
            redact_headers(&mut ws.handshake_response.headers, redaction);
            ws.handshake_response.body = ws
                .handshake_response
                .body
                .take()
                .map(|body| redact_body(body, redaction));
            for message in &mut ws.messages {
                message.content = redact_body(message.content.clone(), redaction);
            }
        }
        _ => {}
    }
    flow
}

fn parse_intercept_key(key: &str) -> (String, String) {
    if let Some((flow_id, rest)) = key.split_once(':') {
        (flow_id.to_string(), rest.to_string())
    } else {
        (key.to_string(), String::new())
    }
}

fn flow_url_method(flow: &Flow) -> (String, String) {
    match &flow.layer {
        Layer::Http(http) => (http.request.url.to_string(), http.request.method.clone()),
        Layer::WebSocket(ws) => (
            ws.handshake_request.url.to_string(),
            ws.handshake_request.method.clone(),
        ),
        _ => (String::new(), String::new()),
    }
}

pub(crate) fn redact_flow_summary(
    mut summary: FlowSummary,
    redaction: &RedactionPolicy,
) -> FlowSummary {
    if !redaction.enabled {
        return summary;
    }
    summary.url = redact_url_string(&summary.url, redaction);
    summary
}

fn redact_http_request(
    request: &mut relay_core_api::flow::HttpRequest,
    redaction: &RedactionPolicy,
) {
    redact_headers(&mut request.headers, redaction);
    redact_query_pairs(&mut request.query, redaction);
    request.url = redact_url(&request.url, redaction);
    request.body = request.body.take().map(|body| redact_body(body, redaction));
}

fn redact_headers(headers: &mut [(String, String)], redaction: &RedactionPolicy) {
    if !redaction.enabled {
        return;
    }
    let sensitive = redaction_set(&redaction.sensitive_header_names);
    for (name, value) in headers.iter_mut() {
        if sensitive.contains(&name.to_ascii_lowercase()) {
            *value = "[REDACTED]".to_string();
        }
    }
}

fn redact_query_pairs(query: &mut [(String, String)], redaction: &RedactionPolicy) {
    if !redaction.enabled {
        return;
    }
    let sensitive = redaction_set(&redaction.sensitive_query_keys);
    for (name, value) in query.iter_mut() {
        if sensitive.contains(&name.to_ascii_lowercase()) {
            *value = "[REDACTED]".to_string();
        }
    }
}

fn redact_url(url: &url::Url, redaction: &RedactionPolicy) -> url::Url {
    if !redaction.enabled {
        return url.clone();
    }
    let sensitive = redaction_set(&redaction.sensitive_query_keys);
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let key = k.to_string();
            let value = if sensitive.contains(&key.to_ascii_lowercase()) {
                "[REDACTED]".to_string()
            } else {
                v.to_string()
            };
            (key, value)
        })
        .collect();
    let mut next = url.clone();
    if pairs.is_empty() {
        return next;
    }
    next.query_pairs_mut().clear();
    for (k, v) in pairs {
        next.query_pairs_mut().append_pair(&k, &v);
    }
    next
}

fn redact_url_string(input: &str, redaction: &RedactionPolicy) -> String {
    match url::Url::parse(input) {
        Ok(url) => redact_url(&url, redaction).to_string(),
        Err(_) => input.to_string(),
    }
}

fn redact_body(mut body: BodyData, redaction: &RedactionPolicy) -> BodyData {
    if redaction.enabled && redaction.redact_bodies {
        body.content = "[REDACTED]".to_string();
    }
    body
}

fn redaction_set(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

/// Does this policy change mean history already on disk must be rewritten?
///
/// Only the off → on transition: re-running the pass on every policy edit would rewrite the whole
/// database for changes that cannot have exposed anything new, and turning redaction *off* must
/// never rewrite (the user is asking to stop, not to lose data).
fn should_redact_history(was_enabled: bool, now_enabled: bool) -> bool {
    !was_enabled && now_enabled
}

/// Default port for the REST/SSE HTTP API.
///
/// Shared so a host can exclude its own API from the capture without hard-coding the number.
pub const DEFAULT_HTTP_API_PORT: u16 = 8082;

/// Default port for the CLI control API.
pub const DEFAULT_CONTROL_PORT: u16 = 8081;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub port: u16,
    pub ca_cert_path: std::path::PathBuf,
    pub ca_key_path: std::path::PathBuf,
    pub transparent: bool,
    pub udp_tproxy_port: Option<u16>,
}

impl ProxyConfig {
    pub fn new(
        port: u16,
        ca_cert_path: std::path::PathBuf,
        ca_key_path: std::path::PathBuf,
    ) -> Self {
        Self {
            port,
            ca_cert_path,
            ca_key_path,
            transparent: false,
            udp_tproxy_port: None,
        }
    }

    pub fn from_app_data_dir(
        app_data_dir: impl Into<std::path::PathBuf>,
        port: u16,
    ) -> Result<Self, String> {
        let app_data_dir = app_data_dir.into();
        if !app_data_dir.exists() {
            std::fs::create_dir_all(&app_data_dir).map_err(|e| e.to_string())?;
        }

        Ok(Self::new(
            port,
            app_data_dir.join("ca_cert.pem"),
            app_data_dir.join("ca_key.pem"),
        ))
    }

    pub fn with_transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    pub fn with_udp_tproxy_port(mut self, udp_tproxy_port: Option<u16>) -> Self {
        self.udp_tproxy_port = udp_tproxy_port;
        self
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn modification_field_names(
    mods: Option<&relay_core_api::modification::FlowModification>,
) -> Vec<&'static str> {
    let Some(mods) = mods else {
        return Vec::new();
    };

    let mut fields = Vec::new();
    if mods.method.is_some() {
        fields.push("method");
    }
    if mods.url.is_some() {
        fields.push("url");
    }
    if mods.request_headers.is_some() {
        fields.push("request_headers");
    }
    if mods.request_header_upserts.is_some() {
        fields.push("request_header_upserts");
    }
    if mods.request_body.is_some() {
        fields.push("request_body");
    }
    if mods.status_code.is_some() {
        fields.push("status_code");
    }
    if mods.response_headers.is_some() {
        fields.push("response_headers");
    }
    if mods.response_header_upserts.is_some() {
        fields.push("response_header_upserts");
    }
    if mods.response_body.is_some() {
        fields.push("response_body");
    }
    if mods.message_content.is_some() {
        fields.push("message_content");
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use relay_core_api::flow::{
        BodyData, Flow, FlowUpdate, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
        ResponseTiming, TransportProtocol,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::time::{Duration, sleep};
    use url::Url;
    use uuid::Uuid;

    static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn sqlite_url() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let pid = std::process::id();
        let seq = TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let db_dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join("test-dbs");
        std::fs::create_dir_all(&db_dir).expect("create test db dir");
        let db_path = db_dir.join(format!(
            "relay-core-runtime-test-{}-{}-{}.db",
            pid, nanos, seq
        ));
        format!("sqlite://{}?mode=rwc", db_path.display())
    }

    #[tokio::test]
    async fn the_capture_exclude_follows_the_listening_proxy_port() {
        let state = CoreState::new(None).await;
        state.policy_tx.send_modify(|policy| {
            policy.capture_exclude =
                vec!["127.0.0.1:8082".to_string(), "localhost:8082".to_string()];
        });

        state.set_listening_proxy_port(Some(18080));
        let listening = state.policy_snapshot().capture_exclude;
        assert!(listening.iter().any(|entry| entry == "127.0.0.1:18080"));
        assert!(listening.iter().any(|entry| entry == "localhost:18080"));
        assert!(listening.iter().all(|entry| entry != "127.0.0.1:8080"));
        assert!(listening.iter().any(|entry| entry == "127.0.0.1:8082"));

        state.set_listening_proxy_port(None);
        let stopped = state.policy_snapshot().capture_exclude;
        assert!(stopped.iter().all(|entry| !entry.ends_with(":18080")));
        assert!(stopped.iter().any(|entry| entry == "127.0.0.1:8082"));
    }

    async fn wait_for_audit_rows(store: &relay_core_storage::store::Store) -> i64 {
        for _ in 0..50 {
            if let Ok(count) = store.count_audit_events().await
                && count > 0
            {
                return count;
            }
            sleep(Duration::from_millis(20)).await;
        }
        0
    }

    fn sample_http_flow(host: &str, path: &str, method: &str, status: u16, ts: i64) -> Flow {
        let start_time =
            chrono::DateTime::<Utc>::from_timestamp_millis(ts).expect("timestamp should be valid");
        let request_url =
            Url::parse(&format!("http://{}{}", host, path)).expect("url should parse");
        Flow {
            id: Uuid::new_v4(),
            start_time,
            end_time: Some(start_time),
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12000,
                server_ip: "127.0.0.1".to_string(),
                server_port: 8080,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: method.to_string(),
                    url: request_url,
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: Some(HttpResponse {
                    status,
                    status_text: "OK".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    fn sample_sensitive_http_flow(ts: i64) -> Flow {
        let start_time =
            chrono::DateTime::<Utc>::from_timestamp_millis(ts).expect("timestamp should be valid");
        let request_url = Url::parse("http://api.example.com/private?token=abc123&ok=1")
            .expect("url should parse");
        Flow {
            id: Uuid::new_v4(),
            start_time,
            end_time: Some(start_time),
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12000,
                server_ip: "127.0.0.1".to_string(),
                server_port: 8080,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: request_url,
                    version: "HTTP/1.1".to_string(),
                    headers: vec![
                        (
                            "Authorization".to_string(),
                            "Bearer secret-token".to_string(),
                        ),
                        ("X-Normal".to_string(), "visible".to_string()),
                    ],
                    cookies: vec![],
                    query: vec![
                        ("token".to_string(), "abc123".to_string()),
                        ("ok".to_string(), "1".to_string()),
                    ],
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: "secret request body".to_string(),
                        size: 19,
                        grpc: None,
                    }),
                },
                response: Some(HttpResponse {
                    status: 200,
                    status_text: "OK".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![
                        ("Set-Cookie".to_string(), "session=abcd".to_string()),
                        ("X-Response".to_string(), "visible".to_string()),
                    ],
                    cookies: vec![],
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: "secret response body".to_string(),
                        size: 20,
                        grpc: None,
                    }),
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[tokio::test]
    async fn set_rules_from_records_audit_event() {
        let state = CoreState::new(None).await;

        state
            .set_rules_from(
                AuditActor::Http,
                "rule.upsert",
                "rule-1".to_string(),
                json!({ "route": "/api/v1/rules" }),
                Vec::new(),
            )
            .await
            .expect("set rules should succeed");

        let events = state.recent_audit_events();
        let event = events.last().expect("audit event should exist");
        assert_eq!(event.actor, AuditActor::Http);
        assert_eq!(event.kind, AuditEventKind::RuleChanged);
        assert_eq!(event.outcome, AuditOutcome::Success);
        assert_eq!(event.target, "rule-1");
        assert_eq!(event.details["operation"], "rule.upsert");
        assert_eq!(event.details["details"]["route"], "/api/v1/rules");
    }

    #[tokio::test]
    async fn upsert_rule_from_replaces_existing_rule_and_records_audit_event() {
        let state = CoreState::new(None).await;

        state
            .upsert_rule_from(
                AuditActor::Probe,
                "rule.upsert",
                "rule-1".to_string(),
                json!({ "tool": "set_rule" }),
                Rule {
                    id: "rule-1".to_string(),
                    name: "first".to_string(),
                    active: true,
                    stage: relay_core_lib::rule::RuleStage::RequestHeaders,
                    priority: 1,
                    termination: relay_core_lib::rule::RuleTermination::Continue,
                    filter: relay_core_lib::rule::Filter::Url(
                        relay_core_lib::rule::StringMatcher::Contains("a".to_string()),
                    ),
                    actions: vec![],
                    constraints: None,
                },
            )
            .await
            .expect("initial upsert should succeed");

        state
            .upsert_rule_from(
                AuditActor::Probe,
                "rule.upsert",
                "rule-1".to_string(),
                json!({ "tool": "set_rule" }),
                Rule {
                    id: "rule-1".to_string(),
                    name: "second".to_string(),
                    active: true,
                    stage: relay_core_lib::rule::RuleStage::RequestHeaders,
                    priority: 2,
                    termination: relay_core_lib::rule::RuleTermination::Continue,
                    filter: relay_core_lib::rule::Filter::Url(
                        relay_core_lib::rule::StringMatcher::Contains("b".to_string()),
                    ),
                    actions: vec![],
                    constraints: None,
                },
            )
            .await
            .expect("replacement upsert should succeed");

        let rules = state.get_rules().await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "second");
        let event = state
            .recent_audit_events()
            .last()
            .cloned()
            .expect("audit event");
        assert_eq!(event.details["operation"], "rule.upsert");
        assert_eq!(event.details["details"]["tool"], "set_rule");
    }

    #[tokio::test]
    async fn delete_rule_from_returns_false_when_rule_missing() {
        let state = CoreState::new(None).await;

        let deleted = state
            .delete_rule_from(
                AuditActor::Http,
                "rule.delete",
                "missing".to_string(),
                json!({ "route": "/api/v1/rules/{id}" }),
                "missing",
            )
            .await
            .expect("delete should not fail");

        assert!(!deleted);
        assert!(state.recent_audit_events().is_empty());
    }

    #[tokio::test]
    async fn create_mock_response_rule_from_adds_rule_and_records_audit_event() {
        let state = CoreState::new(None).await;

        let rule_id = state
            .create_mock_response_rule_from(
                AuditActor::Http,
                "api-mock-1".to_string(),
                json!({ "route": "/api/v1/mock", "status": 201 }),
                MockResponseRuleConfig {
                    rule_id: "api-mock-1".to_string(),
                    url_pattern: "example.com".to_string(),
                    name: "api-mock:example.com".to_string(),
                    status: 201,
                    content_type: "application/json".to_string(),
                    body: "{\"ok\":true}".to_string(),
                },
            )
            .await
            .expect("mock rule should be created");

        assert_eq!(rule_id, "api-mock-1");
        let rules = state.get_rules().await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "api-mock-1");
        let event = state
            .recent_audit_events()
            .last()
            .cloned()
            .expect("audit event");
        assert_eq!(event.details["operation"], "rule.mock_create");
        assert_eq!(event.details["details"]["route"], "/api/v1/mock");
    }

    #[tokio::test]
    async fn create_intercept_rule_from_adds_stop_rule_and_records_audit_event() {
        let state = CoreState::new(None).await;

        let rule_id = state
            .create_intercept_rule_from(
                AuditActor::Http,
                "intercept-1".to_string(),
                json!({ "route": "/api/v1/intercepts", "phase": "request" }),
                InterceptRuleConfig {
                    rule_id: "intercept-1".to_string(),
                    active: true,
                    url_pattern: "example.com".to_string(),
                    method: None,
                    phase: "request".to_string(),
                    name: "api-intercept:example.com".to_string(),
                    priority: 100,
                    termination: relay_core_lib::rule::RuleTermination::Stop,
                },
            )
            .await
            .expect("intercept rule should be created");

        assert_eq!(rule_id, "intercept-1");
        let rules = state.get_rules().await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "intercept-1");
        assert_eq!(rules[0].name, "api-intercept:example.com");
        let event = state
            .recent_audit_events()
            .last()
            .cloned()
            .expect("audit event");
        assert_eq!(event.details["operation"], "rule.intercept_create");
        assert_eq!(event.details["details"]["route"], "/api/v1/intercepts");
    }

    #[tokio::test]
    async fn upsert_legacy_intercept_rule_from_replaces_existing_family() {
        let state = CoreState::new(None).await;

        state
            .upsert_legacy_intercept_rule_from(
                AuditActor::Tauri,
                "legacy-1".to_string(),
                json!({ "command": "set_intercept_rule" }),
                InterceptRule {
                    id: "legacy-1".to_string(),
                    active: true,
                    url_pattern: "example.com".to_string(),
                    method: None,
                    phase: "both".to_string(),
                },
            )
            .await
            .expect("initial family upsert should succeed");
        assert_eq!(state.get_rules().await.len(), 2);

        state
            .upsert_legacy_intercept_rule_from(
                AuditActor::Tauri,
                "legacy-1".to_string(),
                json!({ "command": "set_intercept_rule" }),
                InterceptRule {
                    id: "legacy-1".to_string(),
                    active: true,
                    url_pattern: "example.org".to_string(),
                    method: Some("POST".to_string()),
                    phase: "request".to_string(),
                },
            )
            .await
            .expect("replacement family upsert should succeed");

        let rules = state.get_rules().await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "legacy-1");
        let event = state
            .recent_audit_events()
            .last()
            .cloned()
            .expect("audit event");
        assert_eq!(event.details["operation"], "rule.intercept_legacy_upsert");
        assert_eq!(event.details["details"]["command"], "set_intercept_rule");
    }

    #[tokio::test]
    async fn resolve_intercept_failure_records_failed_audit_event() {
        let state = CoreState::new(None).await;

        let result = state
            .resolve_intercept_with_modifications_from(
                AuditActor::Probe,
                "missing-flow:request".to_string(),
                "drop",
                None,
            )
            .await;

        assert!(result.is_err());
        let events = state.recent_audit_events();
        let event = events.last().expect("audit event should exist");
        assert_eq!(event.actor, AuditActor::Probe);
        assert_eq!(event.kind, AuditEventKind::InterceptResolved);
        assert_eq!(event.outcome, AuditOutcome::Failed);
        assert_eq!(event.details["action"], "drop");
        assert!(
            event.details["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Interception not found")
        );
    }

    #[tokio::test]
    async fn lifecycle_prepare_start_and_stop_updates_snapshot() {
        let state = CoreState::new(None).await;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        state
            .prepare_start(8080, shutdown_tx)
            .expect("prepare start should succeed");
        let lifecycle = state.lifecycle();
        assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Starting);
        assert_eq!(lifecycle.port, Some(8080));
        assert!(lifecycle.started_at_ms.is_none());
        assert!(lifecycle.last_error.is_none());

        assert_eq!(
            state.stop_proxy().expect("stop should succeed"),
            ProxyStopResult::Stopping
        );
        let lifecycle = state.lifecycle();
        assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Stopping);
        assert_eq!(lifecycle.port, Some(8080));
        assert!(shutdown_rx.await.is_ok());
    }

    #[tokio::test]
    async fn status_snapshot_derives_runtime_facing_fields() {
        let state = CoreState::new(None).await;
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        state
            .prepare_start(8080, shutdown_tx)
            .expect("prepare start should succeed");

        let status = state.status_snapshot();
        assert_eq!(status.phase, RuntimeLifecyclePhase::Starting);
        assert!(status.running);
        assert_eq!(status.port, Some(8080));
        assert!(status.uptime.is_none());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn status_report_combines_status_and_metrics() {
        let state = CoreState::new(None).await;
        let report = state.status_report().await;

        assert_eq!(report.status.phase, RuntimeLifecyclePhase::Created);
        assert!(!report.status.running);
        assert_eq!(report.metrics.intercepts_pending, 0);
        assert_eq!(report.metrics.ws_pending_messages, 0);
        assert_eq!(report.metrics.oldest_intercept_age_ms, None);
        assert_eq!(report.metrics.oldest_ws_message_age_ms, None);
        assert_eq!(report.metrics.audit_events_total, 0);
        assert_eq!(report.metrics.audit_events_failed, 0);
        assert_eq!(report.metrics.flow_events_lagged_total, 0);
        assert_eq!(report.metrics.audit_events_lagged_total, 0);
    }

    #[test]
    fn proxy_config_new_and_transport_setters_preserve_values() {
        let config = ProxyConfig::new(
            8080,
            std::path::PathBuf::from("/tmp/ca_cert.pem"),
            std::path::PathBuf::from("/tmp/ca_key.pem"),
        )
        .with_transparent(true)
        .with_udp_tproxy_port(Some(15000));

        assert_eq!(config.port, 8080);
        assert_eq!(
            config.ca_cert_path,
            std::path::PathBuf::from("/tmp/ca_cert.pem")
        );
        assert_eq!(
            config.ca_key_path,
            std::path::PathBuf::from("/tmp/ca_key.pem")
        );
        assert!(config.transparent);
        assert_eq!(config.udp_tproxy_port, Some(15000));
    }

    #[test]
    fn proxy_config_from_app_data_dir_creates_default_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("relaycraft-runtime-config-{}", unique));

        let config =
            ProxyConfig::from_app_data_dir(dir.clone(), 8899).expect("config should build");

        assert!(dir.exists());
        assert_eq!(config.port, 8899);
        assert_eq!(config.ca_cert_path, dir.join("ca_cert.pem"));
        assert_eq!(config.ca_key_path, dir.join("ca_key.pem"));
        assert!(!config.transparent);
        assert!(config.udp_tproxy_port.is_none());
    }

    #[tokio::test]
    async fn intercept_snapshot_maps_pending_counts() {
        let state = CoreState::new(None).await;
        let snapshot = state.intercept_snapshot().await;

        assert_eq!(snapshot.pending_count, 0);
        assert_eq!(snapshot.ws_pending_count, 0);
    }

    #[tokio::test]
    async fn audit_snapshot_returns_latest_events_in_order() {
        let state = CoreState::new(None).await;
        state.record_audit_event(AuditEvent::new(
            AuditActor::Runtime,
            AuditEventKind::RuleChanged,
            "first",
            AuditOutcome::Success,
            json!({ "index": 1 }),
        ));
        state.record_audit_event(AuditEvent::new(
            AuditActor::Http,
            AuditEventKind::PolicyUpdated,
            "second",
            AuditOutcome::Success,
            json!({ "index": 2 }),
        ));

        let snapshot = state.audit_snapshot(1);

        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].target, "second");
        assert_eq!(snapshot.events[0].details["index"], 2);
    }

    /// Persistence is asynchronous, so an event recorded a moment ago may not be in the store yet.
    /// A query that reads only the store therefore answers "nothing happened" to the caller that
    /// just caused it — which is exactly what a fresh lifecycle change looks like.
    #[tokio::test]
    async fn a_persisted_state_still_reports_an_event_recorded_a_moment_ago() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("audit.db").display()
        );
        let state = CoreState::new(Some(db)).await;
        assert!(state.store.is_some(), "this test needs the store path");

        state.record_audit_event(AuditEvent::new(
            AuditActor::Cli,
            AuditEventKind::ProxyLifecycleChanged,
            "proxy:8080",
            AuditOutcome::Success,
            json!({ "change": "started", "requested_by": "cli:relay start" }),
        ));

        let snapshot = state
            .query_audit_snapshot(CoreAuditQuery {
                kind: Some(AuditEventKind::ProxyLifecycleChanged),
                limit: 1,
                ..Default::default()
            })
            .await;

        assert_eq!(
            snapshot.events.len(),
            1,
            "the event must be visible immediately"
        );
        assert_eq!(snapshot.events[0].details["change"], "started");
        assert_eq!(
            snapshot.events[0].details["requested_by"],
            "cli:relay start"
        );
    }

    #[tokio::test]
    async fn query_audit_snapshot_filters_in_memory_events() {
        let state = CoreState::new(None).await;
        state.record_audit_event(AuditEvent::new(
            AuditActor::Http,
            AuditEventKind::RuleChanged,
            "rule-1",
            AuditOutcome::Success,
            json!({ "idx": 1 }),
        ));
        state.record_audit_event(AuditEvent::new(
            AuditActor::Probe,
            AuditEventKind::PolicyUpdated,
            "policy",
            AuditOutcome::Failed,
            json!({ "idx": 2 }),
        ));

        let snapshot = state
            .query_audit_snapshot(CoreAuditQuery {
                actor: Some(AuditActor::Probe),
                kind: Some(AuditEventKind::PolicyUpdated),
                outcome: Some(AuditOutcome::Failed),
                limit: 10,
                ..Default::default()
            })
            .await;

        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].actor, AuditActor::Probe);
        assert_eq!(snapshot.events[0].kind, AuditEventKind::PolicyUpdated);
        assert_eq!(snapshot.events[0].outcome, AuditOutcome::Failed);
    }

    #[tokio::test]
    async fn query_audit_snapshot_reads_persisted_events_when_storage_enabled() {
        let state = CoreState::new(Some(sqlite_url())).await;
        state.update_policy_from(
            AuditActor::Http,
            "policy".to_string(),
            ProxyPolicy {
                transparent_enabled: true,
                ..Default::default()
            },
        );

        let mut snapshot = CoreAuditSnapshot { events: Vec::new() };
        for _ in 0..10 {
            snapshot = state
                .query_audit_snapshot(CoreAuditQuery {
                    actor: Some(AuditActor::Http),
                    kind: Some(AuditEventKind::PolicyUpdated),
                    limit: 10,
                    ..Default::default()
                })
                .await;
            if !snapshot.events.is_empty() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert!(!snapshot.events.is_empty());
        assert_eq!(snapshot.events[0].actor, AuditActor::Http);
        assert_eq!(snapshot.events[0].kind, AuditEventKind::PolicyUpdated);
    }

    #[tokio::test]
    async fn prepare_start_rejects_second_active_start() {
        let state = CoreState::new(None).await;
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        state
            .prepare_start(8080, shutdown_tx)
            .expect("first start should succeed");

        let (second_tx, _second_rx) = oneshot::channel();
        let error = state
            .prepare_start(8081, second_tx)
            .expect_err("second active start should be rejected");
        assert!(error.contains("already"));
    }

    #[test]
    fn update_policy_records_audit_event() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");
        let state = runtime.block_on(CoreState::new(None));

        state.update_policy_from(
            AuditActor::Runtime,
            "policy".to_string(),
            ProxyPolicy {
                transparent_enabled: true,
                ..Default::default()
            },
        );

        let events = state.recent_audit_events();
        let event = events.last().expect("audit event should exist");
        assert_eq!(event.kind, AuditEventKind::PolicyUpdated);
        assert_eq!(event.outcome, AuditOutcome::Success);
        assert_eq!(event.details["transparent_enabled"], true);
    }

    #[test]
    fn patch_policy_updates_redaction_without_replacing_other_fields() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");
        let state = runtime.block_on(CoreState::new(None));
        let original_timeout = state.policy_snapshot().request_timeout_ms;

        state.patch_policy_from(
            AuditActor::Runtime,
            "policy.patch".to_string(),
            relay_core_api::policy::ProxyPolicyPatch {
                redaction: Some(relay_core_api::policy::RedactionPolicyPatch {
                    enabled: Some(true),
                    redact_bodies: Some(true),
                    ..Default::default()
                }),
                upstream: None,
                retention: None,
            },
        );

        let policy = state.policy_snapshot();
        assert_eq!(policy.request_timeout_ms, original_timeout);
        assert!(policy.redaction.enabled);
        assert!(policy.redaction.redact_bodies);

        let events = state.recent_audit_events();
        let event = events.last().expect("audit event should exist");
        assert_eq!(event.kind, AuditEventKind::PolicyUpdated);
        assert_eq!(event.details["redaction_enabled"], true);
    }

    #[tokio::test]
    async fn metrics_include_audit_and_lagged_event_counters() {
        let state = CoreState::new(None).await;

        state.update_policy_from(
            AuditActor::Runtime,
            "policy".to_string(),
            ProxyPolicy::default(),
        );
        let _ = state
            .resolve_intercept_with_modifications_from(
                AuditActor::Probe,
                "missing-flow:request".to_string(),
                "drop",
                None,
            )
            .await;

        state.record_flow_events_lagged(3);
        state.record_audit_events_lagged(5);

        let metrics = state.get_metrics().await;
        assert_eq!(metrics.audit_events_total, 2);
        assert_eq!(metrics.audit_events_failed, 1);
        assert_eq!(metrics.flow_events_lagged_total, 3);
        assert_eq!(metrics.audit_events_lagged_total, 5);
    }

    #[tokio::test]
    async fn prometheus_metrics_text_contains_observability_fields() {
        let state = CoreState::new(None).await;
        state.record_flow_events_lagged(2);
        state.record_audit_events_lagged(4);

        let text = state.get_metrics_prometheus_text().await;
        assert!(text.contains("relay_core_flow_events_lagged_total 2"));
        assert!(text.contains("relay_core_audit_events_lagged_total 4"));
        assert!(text.contains("relay_core_oldest_intercept_age_ms 0"));
        assert!(text.contains("relay_core_oldest_ws_message_age_ms 0"));
    }

    /// History written while redaction was off must be rewritten when it is turned on.
    ///
    /// Enabling redaction previously only affected new writes, so secrets persisted earlier stayed on
    /// disk for the life of the database.
    #[tokio::test]
    async fn enabling_redaction_rewrites_history_already_on_disk() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;

        // Redaction is on by default now, and this test is about the off → on transition, so the
        // starting state has to be stated explicitly rather than inherited.
        state.update_policy(ProxyPolicy {
            redaction: RedactionPolicy {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        });

        // Persist while redaction is off, so the raw secret really is written.
        let mut flow = sample_http_flow("api.example.com", "/old", "GET", 200, 1_700_000_030_000);
        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
            http.request.headers.push((
                "authorization".to_string(),
                "Bearer legacy-secret".to_string(),
            ));
        }
        state.upsert_flow(Box::new(flow.clone()));
        sleep(Duration::from_millis(80)).await;

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        let before = store
            .load_flow(&flow.id.to_string())
            .await
            .expect("load")
            .expect("row");
        assert!(
            before.to_string().contains("legacy-secret"),
            "precondition: the secret is on disk before redaction is enabled"
        );

        // Turning redaction on must rewrite history by itself: an entry point only a test can reach
        // protects nothing. The pass runs in the background, so wait for the outcome rather than for
        // a return value.
        state.update_policy(ProxyPolicy {
            redaction: RedactionPolicy {
                enabled: true,
                sensitive_header_names: vec!["authorization".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut redacted = false;
        while tokio::time::Instant::now() < deadline {
            let row = store
                .load_flow(&flow.id.to_string())
                .await
                .expect("load")
                .expect("row");
            if !row.to_string().contains("legacy-secret") {
                redacted = true;
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert!(
            redacted,
            "enabling redaction must rewrite the secret already on disk, without being asked twice"
        );

        let after = store
            .load_flow(&flow.id.to_string())
            .await
            .expect("load")
            .expect("row");
        assert!(
            !after.to_string().contains("legacy-secret"),
            "history persisted before redaction was enabled must be rewritten"
        );
    }

    /// Asking for a second pass after the automatic one is a no-op.
    ///
    /// This is what makes the automatic pass safe to run on every off → on transition: it cannot
    /// churn the database repeatedly, because a row that is already redacted is not rewritten again.
    #[tokio::test]
    async fn a_second_retroactive_pass_rewrites_nothing() {
        let state = CoreState::new(Some(sqlite_url())).await;
        state.update_policy(ProxyPolicy {
            redaction: RedactionPolicy {
                enabled: true,
                sensitive_header_names: vec!["authorization".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });

        assert_eq!(
            state.redact_stored_history().await,
            Some(0),
            "history is already redacted, so an explicit pass has nothing to rewrite"
        );
    }

    /// Only the off → on transition rewrites history.
    #[test]
    fn only_turning_redaction_on_rewrites_history() {
        assert!(should_redact_history(false, true));
        assert!(!should_redact_history(false, false));
        assert!(
            !should_redact_history(true, true),
            "re-applying an already-on policy must not rewrite the database again"
        );
        assert!(
            !should_redact_history(true, false),
            "turning redaction off is not a request to rewrite history"
        );
    }

    /// With redaction off, a retroactive pass must not touch anything.
    #[tokio::test]
    async fn retroactive_redaction_is_a_no_op_when_disabled() {
        let state = CoreState::new(Some(sqlite_url())).await;
        assert_eq!(
            state.redact_stored_history().await,
            Some(0),
            "a disabled policy must not rewrite history"
        );
    }

    /// An unusable rule must be refused at the door, not stored and silently ignored.
    #[tokio::test]
    async fn storing_a_rule_with_an_invalid_regex_is_rejected() {
        let state = CoreState::new(None).await;

        let rule = Rule {
            id: "bad-regex".to_string(),
            name: "bad regex".to_string(),
            active: true,
            stage: relay_core_lib::rule::RuleStage::RequestHeaders,
            priority: 0,
            termination: relay_core_lib::rule::RuleTermination::Continue,
            filter: relay_core_lib::rule::Filter::Url(relay_core_lib::rule::StringMatcher::Regex(
                "([unclosed".to_string(),
            )),
            actions: vec![relay_core_lib::rule::Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
            constraints: None,
        };

        let result = state
            .upsert_rule_from(
                AuditActor::Runtime,
                "test",
                "rule".to_string(),
                serde_json::json!({}),
                rule,
            )
            .await;

        let err = result.expect_err("an invalid rule must not be accepted");
        assert!(
            err.contains("validation failed"),
            "the error must say why, got: {err}"
        );
        assert!(
            err.contains("url"),
            "the error must locate the offending pattern, got: {err}"
        );
        assert!(
            state.get_rules().await.is_empty(),
            "a rejected rule must not be stored"
        );
    }

    /// A usable rule must still be accepted, so validation does not become a blanket rejection.
    #[tokio::test]
    async fn storing_a_valid_rule_still_succeeds() {
        let state = CoreState::new(None).await;

        let rule = Rule {
            id: "good".to_string(),
            name: "good".to_string(),
            active: true,
            stage: relay_core_lib::rule::RuleStage::RequestHeaders,
            priority: 0,
            termination: relay_core_lib::rule::RuleTermination::Continue,
            filter: relay_core_lib::rule::Filter::Host(relay_core_lib::rule::StringMatcher::Regex(
                "^api\\.example\\.com$".to_string(),
            )),
            actions: vec![relay_core_lib::rule::Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
            constraints: None,
        };

        state
            .upsert_rule_from(
                AuditActor::Runtime,
                "test",
                "rule".to_string(),
                serde_json::json!({}),
                rule,
            )
            .await
            .expect("a valid rule must be accepted");

        assert_eq!(state.get_rules().await.len(), 1);
    }

    /// Re-sending the same intercept rule must replace it, not add a second copy.
    ///
    /// The rule id is what a caller uses to remove it again, so two rules sharing one id are not just
    /// untidy: neither can be addressed. An agent that retries a tool call is the ordinary way this
    /// happens.
    #[tokio::test]
    async fn creating_the_same_intercept_rule_twice_replaces_it() {
        use crate::rule::InterceptRuleConfig;

        let state = CoreState::new(None).await;
        let config = || InterceptRuleConfig {
            rule_id: "probe-intercept-abc".to_string(),
            active: true,
            url_pattern: "example.com/api".to_string(),
            method: None,
            phase: "request".to_string(),
            name: "probe-intercept:example.com/api".to_string(),
            priority: 100,
            termination: relay_core_lib::rule::RuleTermination::Stop,
        };

        for _ in 0..3 {
            state
                .create_intercept_rule_from(
                    AuditActor::Probe,
                    "probe-intercept-abc".to_string(),
                    json!({ "tool": "set_intercept" }),
                    config(),
                )
                .await
                .expect("rule creation should succeed");
        }

        let rules = state.get_rules().await;
        let matching: Vec<&relay_core_lib::rule::Rule> = rules
            .iter()
            .filter(|r| r.id.starts_with("probe-intercept-abc"))
            .collect();

        assert_eq!(
            matching.len(),
            1,
            "three identical calls must leave one rule, got {:?}",
            matching.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
    }

    /// Persisting flows with a bound set must actually evict old rows through the runtime, not just
    /// through the storage API.
    #[tokio::test]
    async fn retention_evicts_persisted_flows_through_the_runtime() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;
        state.set_retention_policy(relay_core_storage::RetentionPolicy {
            max_flows: Some(2),
            ..Default::default()
        });

        for (i, ts) in [1_700_000_001_000i64, 1_700_000_002_000, 1_700_000_003_000]
            .into_iter()
            .enumerate()
        {
            let flow = sample_http_flow("api.example.com", &format!("/f{i}"), "GET", 200, ts);
            state.upsert_flow(Box::new(flow));
            // Let the actor drain so the row is actually written before pruning.
            sleep(Duration::from_millis(30)).await;
        }
        sleep(Duration::from_millis(80)).await;

        let pruned = state.prune_now().await.expect("prune should report counts");
        assert!(
            pruned.total() > 0,
            "a bounded policy must evict something once flows exceed the bound"
        );

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        assert_eq!(
            store.count_flows().await.expect("count"),
            2,
            "the store must retain exactly the configured number of flows"
        );
    }

    /// A bound must be reachable by setting policy, not only by calling the runtime's own setter.
    ///
    /// `set_retention_policy` had no caller outside this test module, so the pruning implementation
    /// was unreachable from every host: the documented answer to "the database grows forever" could
    /// not be used by anyone. Policy is what every host already sets, so the bound travels with it.
    #[tokio::test]
    async fn a_policy_can_bound_storage_from_any_host() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;

        for (i, ts) in [1_700_000_011_000i64, 1_700_000_012_000, 1_700_000_013_000]
            .into_iter()
            .enumerate()
        {
            let flow = sample_http_flow("api.example.com", &format!("/p{i}"), "GET", 200, ts);
            state.upsert_flow(Box::new(flow));
            sleep(Duration::from_millis(30)).await;
        }
        sleep(Duration::from_millis(80)).await;

        // The host-facing path: one policy update, no direct call to the storage setter.
        // Age is left unset so this asserts the count bound, not the clock on the sample flows.
        state.update_policy(ProxyPolicy {
            retention: relay_core_api::policy::RetentionPolicy {
                max_flows: Some(1),
                max_age_secs: None,
                max_audit_events: None,
            },
            ..Default::default()
        });

        let pruned = state.prune_now().await.expect("prune should report counts");
        assert!(
            pruned.total() > 0,
            "a policy that bounds storage must take effect for the host that set it"
        );

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        assert_eq!(
            store.count_flows().await.expect("count"),
            1,
            "the store must honour the bound carried by policy"
        );
    }

    /// An explicit unbounded policy must not delete anything, even after the process started
    /// with the default bound.
    #[tokio::test]
    async fn an_unbounded_policy_leaves_storage_alone() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;
        let flow = sample_http_flow("api.example.com", "/keep", "GET", 200, 1_700_000_021_000);
        state.upsert_flow(Box::new(flow));
        sleep(Duration::from_millis(80)).await;

        state.update_policy(ProxyPolicy {
            retention: relay_core_api::policy::RetentionPolicy::unbounded(),
            ..Default::default()
        });

        assert!(
            state.prune_now().await.is_none(),
            "an explicit unbounded policy must not prune"
        );

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        assert_eq!(store.count_flows().await.expect("count"), 1);
    }

    /// The process starts already bounded, so a fresh flow under the cap survives a prune.
    #[tokio::test]
    async fn default_retention_is_bounded_and_keeps_a_fresh_flow() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;
        let policy = state.retention_policy();
        assert_eq!(policy.max_flows, Some(5_000));
        assert_eq!(policy.max_age_secs, Some(7 * 24 * 60 * 60));

        let flow = sample_http_flow("api.example.com", "/keep", "GET", 200, 1_700_000_020_000);
        state.upsert_flow(Box::new(flow));
        sleep(Duration::from_millis(80)).await;

        assert!(
            state.prune_now().await.is_some(),
            "the default policy is bounded, so a prune pass runs"
        );

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        assert_eq!(
            store.count_flows().await.expect("count"),
            1,
            "a single fresh flow is under both the count and the age bound"
        );
    }

    /// Clearing history removes flows and summaries and leaves audit rows alone.
    #[tokio::test]
    async fn clear_captured_flows_drops_history_and_keeps_audit() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;
        state.update_policy(ProxyPolicy::default());
        let flow = sample_http_flow("api.example.com", "/gone", "GET", 200, 1_700_000_030_000);
        let id = flow.id.to_string();
        state.upsert_flow(Box::new(flow));

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        let audit_before = wait_for_audit_rows(&store).await;
        assert!(audit_before > 0);

        let (flows, summaries) = state
            .clear_captured_flows()
            .await
            .expect("clear should reach the store");
        assert!(
            flows >= 1,
            "the persisted flow should be deleted, got {flows}"
        );
        assert!(
            summaries >= 1,
            "its summary should be deleted, got {summaries}"
        );
        assert!(state.get_flow(id).await.is_none());
        assert!(
            state
                .search_flows(relay_core_api::modification::FlowQuery::default())
                .await
                .is_empty()
        );

        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("reopen store");
        assert_eq!(store.count_flows().await.expect("count"), 0);
        assert_eq!(
            store.count_audit_events().await.expect("audit"),
            audit_before,
            "clearing captures must not wipe the audit log"
        );

        let (again_flows, again_summaries) = state
            .clear_captured_flows()
            .await
            .expect("a second clear still succeeds");
        assert_eq!((again_flows, again_summaries), (0, 0));
    }

    /// A configured redaction policy must apply **before** persistence, not only on output.
    ///
    /// Redaction used to exist solely on the read path, so `persist_flow` wrote raw headers, URLs and
    /// bodies to disk: the file contained exactly the secrets the API was busy hiding. This reads the
    /// database directly — bypassing every output-path redaction — so it cannot pass by accident.
    #[tokio::test]
    async fn persisted_flow_is_redacted_when_the_policy_says_so() {
        let url = sqlite_url();
        let state = CoreState::new(Some(url.clone())).await;

        let policy = ProxyPolicy {
            redaction: RedactionPolicy {
                enabled: true,
                sensitive_header_names: vec!["authorization".to_string()],
                redact_bodies: true,
                ..Default::default()
            },
            ..Default::default()
        };
        state.update_policy(policy);

        let mut flow =
            sample_http_flow("api.example.com", "/secret", "GET", 200, 1_700_000_010_000);
        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
            http.request.headers.push((
                "authorization".to_string(),
                "Bearer super-secret-token".to_string(),
            ));
            http.request.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: "top-secret-payload".to_string(),
                size: 18,
                grpc: None,
            });
        }

        state.upsert_flow(Box::new(flow.clone()));

        // Read straight from the file, so no output-path redaction can mask a raw write.
        let store = relay_core_storage::store::Store::connect(&url)
            .await
            .expect("connect to the same database");
        let raw = loop {
            if let Some(value) = store
                .load_flow(&flow.id.to_string())
                .await
                .expect("load persisted flow")
            {
                break value;
            }
            sleep(Duration::from_millis(20)).await;
        };
        let serialized = raw.to_string();

        assert!(
            !serialized.contains("super-secret-token"),
            "a sensitive header value must never reach disk, found: {serialized}"
        );
        assert!(
            !serialized.contains("top-secret-payload"),
            "a redacted body must never reach disk, found: {serialized}"
        );
    }

    #[tokio::test]
    async fn search_flows_uses_store_with_offset_pagination() {
        let state = CoreState::new(Some(sqlite_url())).await;
        let flow_a = sample_http_flow("api.example.com", "/a", "GET", 200, 1_700_000_001_000);
        let flow_b = sample_http_flow("api.example.com", "/b", "POST", 500, 1_700_000_002_000);
        let flow_c = sample_http_flow("api.example.com", "/c", "GET", 201, 1_700_000_003_000);

        state.upsert_flow(Box::new(flow_a));
        state.upsert_flow(Box::new(flow_b));
        state.upsert_flow(Box::new(flow_c));

        let mut baseline = Vec::new();
        for _ in 0..20 {
            baseline = state
                .search_flows(FlowQuery {
                    host: Some("api.example.com".to_string()),
                    path_contains: None,
                    method: None,
                    status_min: None,
                    status_max: None,
                    has_error: None,
                    is_websocket: None,
                    limit: Some(3),
                    offset: Some(0),
                })
                .await;
            if baseline.len() == 3 {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(baseline.len(), 3);
        let page = state
            .search_flows(FlowQuery {
                host: Some("api.example.com".to_string()),
                path_contains: None,
                method: None,
                status_min: None,
                status_max: None,
                has_error: None,
                is_websocket: None,
                limit: Some(1),
                offset: Some(1),
            })
            .await;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, baseline[1].id);
    }

    #[tokio::test]
    async fn get_flow_falls_back_to_store_after_lru_eviction() {
        let state = CoreState::new(Some(sqlite_url())).await;
        let first_flow = sample_http_flow(
            "persist.example.com",
            "/first",
            "GET",
            200,
            1_700_000_010_000,
        );
        let first_id = first_flow.id.to_string();
        state.upsert_flow(Box::new(first_flow));
        for i in 0..240 {
            state.upsert_flow(Box::new(sample_http_flow(
                "persist.example.com",
                &format!("/{}", i),
                "GET",
                200,
                1_700_000_020_000 + i,
            )));
        }

        sleep(Duration::from_millis(200)).await;

        let loaded = state.get_flow(first_id).await;
        assert!(loaded.is_some());
    }

    #[tokio::test]
    async fn search_flows_redacts_summary_url_when_enabled() {
        let state = CoreState::new(None).await;
        state.update_policy_from(
            AuditActor::Runtime,
            "policy.redaction".to_string(),
            ProxyPolicy {
                redaction: RedactionPolicy {
                    enabled: true,
                    sensitive_query_keys: vec!["token".to_string()],
                    redact_bodies: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        state.upsert_flow(Box::new(sample_sensitive_http_flow(1_700_000_100_000)));

        let mut items = Vec::new();
        for _ in 0..20 {
            items = state
                .search_flows(FlowQuery {
                    host: Some("api.example.com".to_string()),
                    path_contains: Some("/private".to_string()),
                    ..Default::default()
                })
                .await;
            if !items.is_empty() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert!(!items.is_empty());
        let redacted = Url::parse(&items[0].url).expect("summary url should parse");
        let token = redacted
            .query_pairs()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.to_string());
        assert_eq!(token.as_deref(), Some("[REDACTED]"));
    }

    #[tokio::test]
    async fn get_flow_applies_header_query_and_body_redaction_when_enabled() {
        let state = CoreState::new(Some(sqlite_url())).await;
        state.update_policy_from(
            AuditActor::Runtime,
            "policy.redaction".to_string(),
            ProxyPolicy {
                redaction: RedactionPolicy {
                    enabled: true,
                    sensitive_header_names: vec![
                        "authorization".to_string(),
                        "set-cookie".to_string(),
                    ],
                    sensitive_query_keys: vec!["token".to_string()],
                    redact_bodies: true,
                },
                ..Default::default()
            },
        );

        let flow = sample_sensitive_http_flow(1_700_000_200_000);
        let flow_id = flow.id.to_string();
        state.upsert_flow(Box::new(flow));
        sleep(Duration::from_millis(80)).await;

        let loaded = state.get_flow(flow_id).await.expect("flow should exist");
        let Layer::Http(http) = loaded.layer else {
            panic!("expected http layer");
        };

        let auth = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("[REDACTED]"));

        let req_query_token = http
            .request
            .query
            .iter()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.as_str());
        assert_eq!(req_query_token, Some("[REDACTED]"));

        let req_body = http.request.body.as_ref().map(|b| b.content.as_str());
        assert_eq!(req_body, Some("[REDACTED]"));

        let response = http.response.expect("response should exist");
        let set_cookie = response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.as_str());
        assert_eq!(set_cookie, Some("[REDACTED]"));
        let res_body = response.body.as_ref().map(|b| b.content.as_str());
        assert_eq!(res_body, Some("[REDACTED]"));
    }

    #[test]
    fn redact_flow_update_masks_http_body_when_enabled() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");
        let state = runtime.block_on(CoreState::new(None));
        state.update_policy_from(
            AuditActor::Runtime,
            "policy.redaction".to_string(),
            ProxyPolicy {
                redaction: RedactionPolicy {
                    enabled: true,
                    redact_bodies: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let update = FlowUpdate::HttpBody {
            flow_id: "f-1".to_string(),
            direction: relay_core_api::flow::Direction::ClientToServer,
            body: BodyData {
                encoding: "utf-8".to_string(),
                content: "super-secret".to_string(),
                size: 12,
                grpc: None,
            },
        };
        let redacted = state.redact_flow_update_for_output(update);
        match redacted {
            FlowUpdate::HttpBody { body, .. } => assert_eq!(body.content, "[REDACTED]"),
            _ => panic!("expected http body update"),
        }
    }

    #[cfg(feature = "script")]
    #[tokio::test]
    async fn load_script_from_records_audit_event() {
        let state = CoreState::new(None).await;

        state
            .load_script_from(
                AuditActor::Tauri,
                "tauri.load_script".to_string(),
                "globalThis.onRequestHeaders = (_flow) => {};",
            )
            .await
            .expect("script should load");

        let events = state.recent_audit_events();
        let event = events.last().expect("audit event should exist");
        assert_eq!(event.actor, AuditActor::Tauri);
        assert_eq!(event.kind, AuditEventKind::ScriptReloaded);
        assert_eq!(event.outcome, AuditOutcome::Success);
        assert_eq!(event.target, "tauri.load_script");
        assert_eq!(
            state.current_script().as_deref(),
            Some("globalThis.onRequestHeaders = (_flow) => {};")
        );

        let failed = state
            .load_script_from(
                AuditActor::Tauri,
                "tauri.load_script".to_string(),
                "function (",
            )
            .await;
        assert!(failed.is_err(), "a broken script must not load");
        assert_eq!(
            state.current_script().as_deref(),
            Some("globalThis.onRequestHeaders = (_flow) => {};"),
            "a failed reload keeps the script that is still running"
        );
    }
}
