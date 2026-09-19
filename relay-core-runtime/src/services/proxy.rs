//! Proxy lifecycle control: the narrow capability a host needs to start, stop and observe the
//! running proxy without owning the engine.
//!
//! See decision [`0007`](../../../docs/decisions/0007-daemon-control-plane.md). The property this
//! module exists to guarantee is that "start" means *listening*: the runtime spawns the proxy task
//! and reports the bind outcome on the lifecycle channel asynchronously, so a host that only calls
//! `spawn_proxy` cannot tell a running proxy from a task that died on `EADDRINUSE`. A caller that
//! then answers tool calls with an empty flow list is the failure mode being removed here.

use crate::audit::{AuditActor, AuditEvent, AuditEventKind, AuditOutcome};
use crate::paths::CaPaths;
use crate::{CoreState, ProxyConfig, ProxySpawnResult, RuntimeLifecycle, RuntimeLifecyclePhase};
use async_trait::async_trait;
use relay_core_api::flow::FlowUpdate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Port used when a start request does not name one.
pub const DEFAULT_PROXY_PORT: u16 = 8080;

/// How long a start may take before it is reported as a failure rather than a hopeful success.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a stop may take. The proxy drains in-flight connections before the task ends.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// Stable, machine-readable failure codes.
///
/// Adapters (HTTP status mapping, MCP structured errors) branch on these instead of parsing
/// messages, so the text of an error can change without breaking a consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyControlErrorCode {
    InvalidConfig,
    StartFailed,
    StartTimeout,
    StopFailed,
    StopTimeout,
}

impl ProxyControlErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfig => "invalid_config",
            Self::StartFailed => "start_failed",
            Self::StartTimeout => "start_timeout",
            Self::StopFailed => "stop_failed",
            Self::StopTimeout => "stop_timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyControlError {
    #[error("invalid proxy configuration: {0}")]
    InvalidConfig(String),
    #[error("proxy failed to start: {0}")]
    StartFailed(String),
    #[error("timed out waiting for the proxy to start listening")]
    StartTimeout,
    #[error("proxy failed to stop: {0}")]
    StopFailed(String),
    #[error("timed out waiting for the proxy to stop")]
    StopTimeout,
}

impl ProxyControlError {
    pub fn code(&self) -> ProxyControlErrorCode {
        match self {
            Self::InvalidConfig(_) => ProxyControlErrorCode::InvalidConfig,
            Self::StartFailed(_) => ProxyControlErrorCode::StartFailed,
            Self::StartTimeout => ProxyControlErrorCode::StartTimeout,
            Self::StopFailed(_) => ProxyControlErrorCode::StopFailed,
            Self::StopTimeout => ProxyControlErrorCode::StopTimeout,
        }
    }
}

/// What a caller asks for when starting the proxy.
///
/// Fields mirror the subset of `relay run` flags the daemon accepts over the control plane; CA
/// paths are optional so a request can rely on the data-directory defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyStartRequest {
    #[serde(default = "default_proxy_port")]
    pub port: u16,
    #[serde(default)]
    pub transparent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_tproxy_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_key: Option<PathBuf>,
}

fn default_proxy_port() -> u16 {
    DEFAULT_PROXY_PORT
}

impl Default for ProxyStartRequest {
    fn default() -> Self {
        Self {
            port: DEFAULT_PROXY_PORT,
            transparent: false,
            udp_tproxy_port: None,
            ca_cert: None,
            ca_key: None,
        }
    }
}

/// Result of a start request.
///
/// `AlreadyRunning` is a success, not an error: start is idempotent so a client that reconnects
/// (or two clients racing) never tears down a proxy the other one is using.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProxyStartOutcome {
    Started { port: u16 },
    AlreadyRunning { port: u16 },
}

impl ProxyStartOutcome {
    pub fn port(&self) -> u16 {
        match self {
            Self::Started { port } | Self::AlreadyRunning { port } => *port,
        }
    }
}

/// Result of a stop request. `AlreadyStopped` keeps stop idempotent for scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProxyStopOutcome {
    Stopped,
    AlreadyStopped,
}

/// Who is asking for a lifecycle change.
///
/// The [`AuditActor`] is the transport that carried the request; the label is the client's own
/// description of itself (`cli:relay stop`, `mcp:proxy_start`). The label is **never** used for
/// authorization — a client can lie about it — it exists so the audit trail can answer "who stopped
/// this proxy" after the caller has exited.
///
/// Both transitions take one, so neither can be made anonymously by omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    pub actor: AuditActor,
    pub label: Option<String>,
}

impl Requester {
    pub fn new(actor: AuditActor) -> Self {
        Self { actor, label: None }
    }

    /// Describe this client, e.g. `cli:relay stop`.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The label, or the actor's name when the client said nothing.
    fn described(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.actor.as_str().to_string())
    }
}

/// Narrow capability: control the lifecycle of the one proxy this engine owns.
#[async_trait]
pub trait ProxyControlService: Send + Sync {
    async fn proxy_start(
        &self,
        requester: Requester,
        request: ProxyStartRequest,
    ) -> Result<ProxyStartOutcome, ProxyControlError>;

    async fn proxy_stop(&self, requester: Requester)
    -> Result<ProxyStopOutcome, ProxyControlError>;

    fn proxy_lifecycle(&self) -> RuntimeLifecycle;
}

/// Owns the proxy lifecycle on behalf of a host that keeps the engine alive.
///
/// The host holds one of these for its whole lifetime: it is the object that makes the proxy's
/// state authoritative in a single place rather than inferred per client.
pub struct CoreProxyController {
    state: Arc<CoreState>,
    /// The runtime requires a sink for every flow update. History lives in the store and live
    /// events on the broadcast channel, so this sink is only for a host that wants the raw stream
    /// written to a file; without one, a task drains it. Leaving the channel unread instead would
    /// make the runtime count every flow as dropped.
    sink: mpsc::Sender<FlowUpdate>,
    /// Where flow updates are appended as JSONL, when the host asked for a stream file.
    save_stream: Option<std::path::PathBuf>,
}

impl CoreProxyController {
    /// A controller with no recording: flow history lives in the store and on the broadcast
    /// channel, so the runtime's sink is drained and discarded.
    pub fn new(state: Arc<CoreState>) -> Self {
        Self::build(state, None)
    }

    /// A controller that also appends every flow update to `path` as JSONL.
    ///
    /// Fallible on purpose: a recording the operator asked for must either exist or be reported.
    /// Returning a controller that silently records nothing is the failure mode this design keeps
    /// removing elsewhere.
    pub fn with_save_stream(
        state: Arc<CoreState>,
        path: impl Into<std::path::PathBuf>,
    ) -> Result<Self, String> {
        let path = path.into();
        let file = open_stream_file(&path)?;
        Ok(Self::build(state, Some((path, file))))
    }

    fn build(
        state: Arc<CoreState>,
        recording: Option<(std::path::PathBuf, std::fs::File)>,
    ) -> Self {
        let (sink, mut rx) = mpsc::channel::<FlowUpdate>(1024);
        let save_stream = recording.as_ref().map(|(path, _)| path.clone());
        let mut writer = recording.map(|(_, file)| std::io::BufWriter::new(file));

        tokio::spawn(async move {
            use std::io::Write;
            while let Some(update) = rx.recv().await {
                if writer.is_none() {
                    continue;
                }

                let line = match serde_json::to_string(&update) {
                    Ok(line) => line,
                    Err(error) => {
                        tracing::warn!("could not serialize a flow update: {error}");
                        continue;
                    }
                };

                let written = writer
                    .as_mut()
                    .map(|writer| writeln!(writer, "{line}"))
                    .unwrap_or(Ok(()));

                if written.is_err() {
                    // A hole in the recording is reported once, loudly, instead of leaving a file
                    // that looks complete.
                    tracing::error!(
                        "flow stream file is no longer writable; stopping the recording"
                    );
                    writer = None;
                }
            }
            if let Some(writer) = writer.as_mut() {
                let _ = writer.flush();
            }
        });

        Self {
            state,
            sink,
            save_stream,
        }
    }

    /// Where this controller records flow updates, when it records them.
    pub fn save_stream(&self) -> Option<&std::path::Path> {
        self.save_stream.as_deref()
    }

    pub fn state(&self) -> &Arc<CoreState> {
        &self.state
    }
}

#[async_trait]
impl ProxyControlService for CoreProxyController {
    async fn proxy_start(
        &self,
        requester: Requester,
        request: ProxyStartRequest,
    ) -> Result<ProxyStartOutcome, ProxyControlError> {
        let mut lifecycle_rx = self.state.subscribe_lifecycle();

        let ca_paths = CaPaths::resolve(request.ca_cert.clone(), request.ca_key.clone())
            .map_err(ProxyControlError::InvalidConfig)?;
        let mut config = ProxyConfig::new(request.port, ca_paths.cert, ca_paths.key);
        config.transparent = request.transparent;
        config.udp_tproxy_port = request.udp_tproxy_port;

        let outcome = match self.state.spawn_proxy(config, self.sink.clone(), None) {
            Ok(ProxySpawnResult::AlreadyRunning) => {
                let lifecycle = self.state.lifecycle();
                Ok(ProxyStartOutcome::AlreadyRunning {
                    port: lifecycle.port.unwrap_or(request.port),
                })
            }
            Ok(ProxySpawnResult::Started(_handle)) => {
                // "Started" only means the task was spawned. Waiting for the lifecycle is what
                // turns a bind failure into an error here instead of an empty flow list later.
                let settled = wait_for_phase(
                    &mut lifecycle_rx,
                    &[
                        RuntimeLifecyclePhase::Running,
                        RuntimeLifecyclePhase::Failed,
                    ],
                    START_TIMEOUT,
                )
                .await;

                match settled {
                    Some(lifecycle) if lifecycle.phase == RuntimeLifecyclePhase::Running => {
                        Ok(ProxyStartOutcome::Started {
                            port: lifecycle.port.unwrap_or(request.port),
                        })
                    }
                    Some(lifecycle) => Err(ProxyControlError::StartFailed(
                        lifecycle
                            .last_error
                            .unwrap_or_else(|| "proxy reported failure without a message".into()),
                    )),
                    None => Err(ProxyControlError::StartTimeout),
                }
            }
            Err(error) => Err(ProxyControlError::StartFailed(error)),
        };

        self.audit(
            &requester,
            match &outcome {
                Ok(_) => AuditOutcome::Success,
                Err(_) => AuditOutcome::Failed,
            },
            match &outcome {
                Ok(ProxyStartOutcome::Started { port }) => {
                    json!({ "change": "started", "port": port })
                }
                Ok(ProxyStartOutcome::AlreadyRunning { port }) => {
                    json!({ "change": "already_running", "port": port })
                }
                Err(error) => json!({
                    "change": "start_failed",
                    "requested_port": request.port,
                    "error": error.to_string(),
                }),
            },
        );

        outcome
    }

    async fn proxy_stop(
        &self,
        requester: Requester,
    ) -> Result<ProxyStopOutcome, ProxyControlError> {
        let mut lifecycle_rx = self.state.subscribe_lifecycle();

        let outcome = match self.state.stop_proxy() {
            Ok(crate::ProxyStopResult::NotRunning) => Ok(ProxyStopOutcome::AlreadyStopped),
            Ok(crate::ProxyStopResult::Stopping) => {
                // A caller that returns while the phase is still `Stopping` makes the next start
                // fail with "already stopping", which is the flapping a lifecycle command must not
                // introduce.
                let settled = wait_for_phase(
                    &mut lifecycle_rx,
                    &[
                        RuntimeLifecyclePhase::Stopped,
                        RuntimeLifecyclePhase::Failed,
                    ],
                    STOP_TIMEOUT,
                )
                .await;

                match settled {
                    Some(_) => Ok(ProxyStopOutcome::Stopped),
                    None => Err(ProxyControlError::StopTimeout),
                }
            }
            Err(error) => Err(ProxyControlError::StopFailed(error)),
        };

        self.audit(
            &requester,
            match &outcome {
                Ok(_) => AuditOutcome::Success,
                Err(_) => AuditOutcome::Failed,
            },
            match &outcome {
                Ok(ProxyStopOutcome::Stopped) => json!({ "change": "stopped" }),
                Ok(ProxyStopOutcome::AlreadyStopped) => json!({ "change": "already_stopped" }),
                Err(error) => json!({ "change": "stop_failed", "error": error.to_string() }),
            },
        );

        outcome
    }

    fn proxy_lifecycle(&self) -> RuntimeLifecycle {
        self.state.lifecycle()
    }
}

impl CoreProxyController {
    /// Record a lifecycle change, so the audit trail answers "who started this proxy" after the
    /// asking process is long gone.
    fn audit(&self, requester: &Requester, outcome: AuditOutcome, mut details: serde_json::Value) {
        let target = details
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .map(|port| format!("proxy:{port}"))
            .unwrap_or_else(|| "proxy".to_string());

        if let Some(object) = details.as_object_mut() {
            object.insert(
                "requested_by".to_string(),
                serde_json::Value::String(requester.described()),
            );
        }

        self.state.record_audit_event(AuditEvent::new(
            requester.actor.clone(),
            AuditEventKind::ProxyLifecycleChanged,
            target,
            outcome,
            details,
        ));
    }
}

fn open_stream_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("opening the flow stream file {}: {e}", path.display()))
}

/// Wait until the lifecycle reaches one of `targets`, or the timeout expires.
///
/// The current value is checked before waiting because the transition can land between subscribing
/// and the first `changed()` — a race that would otherwise wait out the whole timeout.
async fn wait_for_phase(
    rx: &mut watch::Receiver<RuntimeLifecycle>,
    targets: &[RuntimeLifecyclePhase],
    timeout: Duration,
) -> Option<RuntimeLifecycle> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let current = rx.borrow_and_update().clone();
        if targets.contains(&current.phase) {
            return Some(current);
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }

        match tokio::time::timeout(remaining, rx.changed()).await {
            Ok(Ok(())) => continue,
            // Closed channel or elapsed timeout: report the phase we last saw rather than hanging.
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn install_crypto_provider() {
        // Starting a real proxy builds a real CA, which needs a process-level TLS provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// Ask the OS for a port nothing is listening on, then release it.
    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        port
    }

    fn request_on(dir: &std::path::Path, port: u16) -> ProxyStartRequest {
        ProxyStartRequest {
            port,
            ca_cert: Some(dir.join("ca_cert.pem")),
            ca_key: Some(dir.join("ca_key.pem")),
            ..Default::default()
        }
    }

    async fn controller() -> (Arc<CoreProxyController>, tempfile::TempDir) {
        install_crypto_provider();
        let dir = tempfile::tempdir().expect("temp dir");
        let state = Arc::new(CoreState::new(None).await);
        (Arc::new(CoreProxyController::new(state)), dir)
    }

    /// A lifecycle change must leave a trace that outlives the caller: "who started this proxy"
    /// is otherwise unanswerable once a short-lived client has exited.
    #[tokio::test]
    async fn lifecycle_changes_are_audited_with_their_requester() {
        let (controller, dir) = controller().await;
        let port = free_port();
        let request = request_on(dir.path(), port);

        controller
            .proxy_start(
                Requester::new(AuditActor::Probe).label("mcp:proxy_start"),
                request,
            )
            .await
            .expect("start");
        controller
            .proxy_stop(Requester::new(AuditActor::Cli).label("cli:relay stop"))
            .await
            .expect("stop");

        let events = controller.state().recent_audit_events();
        let lifecycle: Vec<_> = events
            .iter()
            .filter(|event| event.kind == AuditEventKind::ProxyLifecycleChanged)
            .collect();

        assert_eq!(lifecycle.len(), 2, "start and stop are both recorded");

        let start = lifecycle[0];
        assert_eq!(start.actor, AuditActor::Probe);
        assert_eq!(start.details["change"], "started");
        assert_eq!(start.details["requested_by"], "mcp:proxy_start");
        assert_eq!(start.target, format!("proxy:{port}"));

        let stop = lifecycle[1];
        assert_eq!(stop.actor, AuditActor::Cli);
        assert_eq!(stop.details["change"], "stopped");
        assert_eq!(stop.details["requested_by"], "cli:relay stop");
    }

    /// A start that fails is a lifecycle change too, and the failure reason belongs in the trail.
    #[tokio::test]
    async fn a_failed_start_is_audited() {
        let (controller, dir) = controller().await;
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("hold port");
        let port = occupied.local_addr().expect("addr").port();

        let _ = controller
            .proxy_start(
                Requester::new(AuditActor::Probe).label("mcp:proxy_start"),
                request_on(dir.path(), port),
            )
            .await
            .expect_err("a taken port must fail");

        let events = controller.state().recent_audit_events();
        let failed = events
            .iter()
            .find(|event| event.kind == AuditEventKind::ProxyLifecycleChanged)
            .expect("the attempt must be recorded");

        assert_eq!(failed.outcome, AuditOutcome::Failed);
        assert_eq!(failed.details["change"], "start_failed");
        assert_eq!(failed.details["requested_by"], "mcp:proxy_start");
        assert!(
            failed.details["error"]
                .as_str()
                .is_some_and(|error| error.contains(&port.to_string())),
            "the reason must be in the trail: {:?}",
            failed.details
        );
    }

    #[tokio::test]
    async fn starting_reports_a_listening_proxy() {
        let (controller, dir) = controller().await;
        let port = free_port();

        let outcome = controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), port),
            )
            .await
            .expect("start should succeed on a free port");

        assert_eq!(outcome, ProxyStartOutcome::Started { port });
        let lifecycle = controller.proxy_lifecycle();
        assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Running);
        assert_eq!(lifecycle.port, Some(port));

        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("cleanup stop");
    }

    #[tokio::test]
    async fn starting_on_a_taken_port_fails_instead_of_reporting_success() {
        let (controller, dir) = controller().await;
        let occupied = TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let port = occupied.local_addr().expect("addr").port();

        let error = controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), port),
            )
            .await
            .expect_err("a taken port must be reported, not silently accepted");

        assert_eq!(error.code(), ProxyControlErrorCode::StartFailed);
        assert!(
            error.to_string().contains(&port.to_string()),
            "the error should name the port: {error}"
        );
    }

    #[tokio::test]
    async fn starting_twice_keeps_one_proxy_and_reports_it() {
        let (controller, dir) = controller().await;
        let port = free_port();
        controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), port),
            )
            .await
            .expect("first start");

        // A different port on purpose: the second call must not start a second proxy.
        let second = controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), free_port()),
            )
            .await
            .expect("second start must be idempotent");

        assert_eq!(second, ProxyStartOutcome::AlreadyRunning { port });

        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("cleanup stop");
    }

    #[tokio::test]
    async fn stopping_leaves_the_engine_ready_to_start_again() {
        let (controller, dir) = controller().await;
        controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), free_port()),
            )
            .await
            .expect("start");

        let outcome = controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("stop");
        assert_eq!(outcome, ProxyStopOutcome::Stopped);
        assert_eq!(
            controller.proxy_lifecycle().phase,
            RuntimeLifecyclePhase::Stopped,
            "stop must not return while the proxy is still shutting down"
        );

        // The reason stop waits: `relay stop && relay start` must work back to back.
        controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), free_port()),
            )
            .await
            .expect("restart after stop");

        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("cleanup stop");
    }

    #[tokio::test]
    async fn stopping_when_nothing_runs_is_not_an_error() {
        let (controller, _dir) = controller().await;
        let outcome = controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("stop must be idempotent");
        assert_eq!(outcome, ProxyStopOutcome::AlreadyStopped);
    }

    #[tokio::test]
    async fn a_start_request_creates_the_ca_it_needs() {
        let (controller, dir) = controller().await;

        controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), free_port()),
            )
            .await
            .expect("start with a fresh data directory");

        assert!(dir.path().join("ca_cert.pem").exists());
        assert!(dir.path().join("ca_key.pem").exists());

        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("cleanup stop");
    }

    /// Two clients starting the proxy at the same moment is the normal case (a CLI and an agent,
    /// or two windows). Exactly one start wins; the rest must be told the proxy is already running
    /// rather than racing for the port and leaving the lifecycle `failed` with a proxy listening.
    #[tokio::test]
    async fn concurrent_starts_produce_one_proxy() {
        let (controller, dir) = controller().await;
        let port = free_port();

        let attempts = 4;
        let mut tasks = Vec::new();
        for _ in 0..attempts {
            let controller = controller.clone();
            let request = request_on(dir.path(), port);
            tasks.push(tokio::spawn(async move {
                controller
                    .proxy_start(Requester::new(AuditActor::Cli), request)
                    .await
            }));
        }

        let mut started = 0;
        let mut already_running = 0;
        for outcome in tasks {
            match outcome.await.expect("task") {
                Ok(ProxyStartOutcome::Started { .. }) => started += 1,
                Ok(ProxyStartOutcome::AlreadyRunning { .. }) => already_running += 1,
                Err(error) => panic!("a concurrent start must not fail: {error}"),
            }
        }

        assert_eq!(started, 1, "exactly one call starts the proxy");
        assert_eq!(already_running, attempts - 1);

        let lifecycle = controller.proxy_lifecycle();
        assert_eq!(
            lifecycle.phase,
            RuntimeLifecyclePhase::Running,
            "the loser must not drive the lifecycle to failed: {lifecycle:?}"
        );

        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("cleanup stop");
    }

    #[tokio::test]
    async fn a_controller_can_record_the_flow_stream() {
        install_crypto_provider();
        let dir = tempfile::tempdir().expect("temp dir");
        let stream = dir.path().join("flows.jsonl");
        let state = Arc::new(CoreState::new(None).await);
        let controller = CoreProxyController::with_save_stream(state.clone(), &stream)
            .expect("a writable path must produce a recording controller");

        assert_eq!(controller.save_stream(), Some(stream.as_path()));

        // Only a proxy produces flow updates, so this test pins the wiring rather than the traffic:
        // the sink must be a live channel, and the file must exist for the daemon to append to.
        assert!(stream.exists(), "the recording file is created up front");

        let port = free_port();
        controller
            .proxy_start(
                Requester::new(AuditActor::Cli),
                request_on(dir.path(), port),
            )
            .await
            .expect("start");
        controller
            .proxy_stop(Requester::new(AuditActor::Cli))
            .await
            .expect("stop");
    }

    #[test]
    fn recording_an_unwritable_path_is_reported() {
        install_crypto_provider();
        let error = CoreProxyController::with_save_stream(
            Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(CoreState::new(None)),
            ),
            "/proc/definitely/not/writable/flows.jsonl",
        )
        .err()
        .expect("an unwritable path must be an error, not a silent no-op");

        assert!(
            error.contains("flow stream file") || error.contains("creating"),
            "the error must name the file: {error}"
        );
    }

    #[test]
    fn a_start_request_defaults_to_the_standard_port() {
        assert_eq!(ProxyStartRequest::default().port, DEFAULT_PROXY_PORT);
        let json = serde_json::to_string(&ProxyStartRequest {
            transparent: true,
            ..Default::default()
        })
        .expect("serialize");
        assert_eq!(json, r#"{"port":8080,"transparent":true}"#);
    }
}
