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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use tracing::error;

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
    /// O4: Total invalid method rejections (strict_http_semantics)
    pub proxy_invalid_method_total: usize,
    /// O4: Total invalid status code rejections (strict_http_semantics)
    pub proxy_invalid_status_total: usize,
    /// O4: Total idempotent request retries
    pub proxy_retry_total: usize,
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
relay_core_proxy_invalid_method_total {}\n\
relay_core_proxy_invalid_status_total {}\n\
relay_core_proxy_retry_total {}\n\
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
            self.proxy_invalid_method_total,
            self.proxy_invalid_status_total,
            self.proxy_retry_total,
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
    pub policy_tx: watch::Sender<ProxyPolicy>,
    pub flows_dropped: Arc<AtomicUsize>,
    audit_events_total: Arc<AtomicUsize>,
    audit_events_failed: Arc<AtomicUsize>,
    flow_events_lagged_total: Arc<AtomicUsize>,
    audit_events_lagged_total: Arc<AtomicUsize>,
    /// 内部广播 channel：Tauri、Probe 等多个消费者均可订阅
    flow_broadcast_tx: broadcast::Sender<FlowUpdate>,
    audit_broadcast_tx: broadcast::Sender<AuditEvent>,
    audit_history: Arc<Mutex<VecDeque<AuditEvent>>>,
    lifecycle: LifecycleManager,
}

impl CoreState {
    pub async fn new(db_url: Option<String>) -> Self {
        const AUDIT_HISTORY_LIMIT: usize = 200;

        let store = if let Some(url) = db_url {
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
        let flow_actor = FlowStoreActor::new(flow_rx, store.clone());
        tokio::spawn(flow_actor.run());

        let (intercept_tx, intercept_rx) = mpsc::channel(1000);
        let intercept_actor = InterceptBrokerActor::new(intercept_rx);
        tokio::spawn(intercept_actor.run());

        let (rule_tx, rule_rx) = mpsc::channel(100);
        let rule_actor = RuleStoreActor::new(rule_rx, store.clone());
        tokio::spawn(rule_actor.run());

        let (policy_tx, _) = watch::channel(ProxyPolicy::default());
        let (flow_broadcast_tx, _) = broadcast::channel(1000);
        let (audit_broadcast_tx, _) = broadcast::channel(256);

        #[cfg(feature = "script")]
        let script_interceptor = ScriptInterceptor::new()
            .await
            .expect("Failed to initialize ScriptInterceptor");
        Self {
            flow_store: flow_tx,
            intercept_broker: intercept_tx,
            rule_store: rule_tx,
            store,
            #[cfg(feature = "script")]
            script_interceptor: Arc::new(script_interceptor),
            policy_tx,
            flows_dropped: Arc::new(AtomicUsize::new(0)),
            audit_events_total: Arc::new(AtomicUsize::new(0)),
            audit_events_failed: Arc::new(AtomicUsize::new(0)),
            flow_events_lagged_total: Arc::new(AtomicUsize::new(0)),
            audit_events_lagged_total: Arc::new(AtomicUsize::new(0)),
            flow_broadcast_tx,
            audit_broadcast_tx,
            audit_history: Arc::new(Mutex::new(VecDeque::with_capacity(AUDIT_HISTORY_LIMIT))),
            lifecycle: LifecycleManager::new(),
        }
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
            proxy_invalid_method_total: relay_core_lib::metrics::get_proxy_invalid_method(),
            proxy_invalid_status_total: relay_core_lib::metrics::get_proxy_invalid_status(),
            proxy_retry_total: relay_core_lib::metrics::get_proxy_retry(),
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

    pub async fn query_audit_snapshot(&self, query: CoreAuditQuery) -> CoreAuditSnapshot {
        let limit = query.limit.clamp(1, 500);
        if let Some(store) = &self.store {
            let rows = store
                .query_audit_events(
                    query.since_ms,
                    query.until_ms,
                    query.actor.as_ref().map(AuditActor::as_str),
                    query.kind.as_ref().map(AuditEventKind::as_str),
                    query.outcome.as_ref().map(AuditOutcome::as_str),
                    limit,
                )
                .await
                .unwrap_or_default();

            let mut events = Vec::with_capacity(rows.len());
            for row in rows {
                if let Ok(event) = serde_json::from_value::<AuditEvent>(row) {
                    events.push(event);
                }
            }
            return CoreAuditSnapshot { events };
        }

        let mut events = self.recent_audit_events();
        events.reverse();
        let filtered = events
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
            .take(limit)
            .collect();
        CoreAuditSnapshot { events: filtered }
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

        self.policy_tx.send_replace(policy);
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

        result
    }

    /// S5: Set the env var whitelist for relay.env() in user scripts.
    #[cfg(feature = "script")]
    pub async fn set_script_env_allow(&self, env_allow: std::collections::HashSet<String>) {
        self.script_interceptor.set_env_allow(env_allow).await;
    }

    fn record_audit_event(&self, event: AuditEvent) {
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
        )));

        interceptors.push(Arc::new(interceptors::metrics::MetricsInterceptor::new(
            self.clone(),
        )));

        if let Some(interceptor) = extra_interceptor {
            interceptors.push(interceptor);
        }

        let interceptor = Arc::new(CompositeInterceptor::new(interceptors));
        let (proxy_tx, mut proxy_rx) = mpsc::channel::<FlowUpdate>(1000);

        tokio::spawn(async move {
            while let Some(update) = proxy_rx.recv().await {
                match update.clone() {
                    FlowUpdate::Full(flow) => {
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
        let shutdown_rx = Some(shutdown_rx);

        if config.transparent {
            let policy = ProxyPolicy {
                transparent_enabled: true,
                ..Default::default()
            };
            self.update_policy_from(AuditActor::Runtime, "proxy.transparent".to_string(), policy);
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
            let policy = ProxyPolicy::default();
            self.update_policy_from(AuditActor::Runtime, "proxy.standard".to_string(), policy);
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
    }
}

fn redact_flow(mut flow: Flow, redaction: &RedactionPolicy) -> Flow {
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

fn redact_flow_summary(mut summary: FlowSummary, redaction: &RedactionPolicy) -> FlowSummary {
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

#[derive(Clone)]
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
    if mods.request_body.is_some() {
        fields.push("request_body");
    }
    if mods.status_code.is_some() {
        fields.push("status_code");
    }
    if mods.response_headers.is_some() {
        fields.push("response_headers");
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

    fn sample_http_flow(host: &str, path: &str, method: &str, status: u16, ts: i64) -> Flow {
        let start_time =
            chrono::DateTime::<Utc>::from_timestamp_millis(ts).expect("timestamp should be valid");
        let request_url =
            Url::parse(&format!("http://{}{}", host, path)).expect("url should parse");
        Flow {
            id: Uuid::new_v4(),
            start_time,
            end_time: Some(start_time),
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12000,
                server_ip: "127.0.0.1".to_string(),
                server_port: 8080,
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
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12000,
                server_ip: "127.0.0.1".to_string(),
                server_port: 8080,
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
                    }),
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
    }
}
