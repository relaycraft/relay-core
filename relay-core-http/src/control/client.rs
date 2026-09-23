//! Client half of the control plane: how the CLI and the MCP bridge drive the daemon.
//!
//! The client speaks the same HTTP API the daemon serves, so every capability a client has is a
//! capability the API documents. Failures carry the daemon's stable error `code`, because a client
//! that can only report prose forces its own caller to guess.

use crate::control::manifest::{
    CONTROL_API_VERSION, DaemonManifest, Discovery, discover, process_alive, remove_manifest,
};
use relay_core_runtime::RuntimeLifecycle;
use relay_core_runtime::services::{ProxyStartOutcome, ProxyStartRequest, ProxyStopOutcome};
use serde::de::DeserializeOwned;
use std::path::Path;
use std::time::Duration;

/// Request timeout for control calls. Long enough for a proxy start (which binds and builds a CA),
/// short enough that a dead-but-live-pid daemon is detected rather than hung on.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Health checks happen during discovery, where waiting is the wrong answer.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ApiVersionInfo {
    pub engine_version: String,
    pub api_version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlClientError {
    /// Nothing is listening: no daemon, or it died. `detail` is the client error, so a proxy
    /// redirect or a refused connection is not reported as an empty listener.
    #[error("no RelayCore daemon is answering at {url} ({detail})")]
    Unreachable { url: String, detail: String },

    /// The daemon answered and refused, with a machine-readable reason.
    #[error("{code}: {message}")]
    Rejected {
        status: u16,
        code: String,
        message: String,
    },

    /// The daemon answered with something this client does not understand.
    #[error("unexpected response from the daemon: {0}")]
    Protocol(String),
}

impl ControlClientError {
    /// Stable code suitable for an MCP structured error or a script branch.
    pub fn code(&self) -> &str {
        match self {
            Self::Unreachable { .. } => "daemon_unreachable",
            Self::Rejected { code, .. } => code,
            Self::Protocol(_) => "daemon_protocol_error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControlClient {
    base_url: String,
    token: Option<String>,
    /// What to call this client in the daemon's audit trail, e.g. `cli:relay start`.
    label: Option<String>,
    http: reqwest::Client,
    health_http: reqwest::Client,
}

impl ControlClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        let base_url = base_url.into();
        let build = |timeout: Duration| {
            reqwest::Client::builder()
                .timeout(timeout)
                // The control API is on loopback. `HTTP_PROXY` and the Windows system proxy are
                // how a machine is pointed at RelayCore; sending this call back through that
                // proxy makes a live daemon look unreachable (`relay-core status`, MCP startup).
                .no_proxy()
                .build()
                .unwrap_or_default()
        };
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            label: None,
            http: build(DEFAULT_TIMEOUT),
            health_http: build(HEALTH_TIMEOUT),
        }
    }

    /// Describe this client to the daemon, for the audit trail.
    ///
    /// Self-reported, and never used for authorization: it answers "who did this" for a human
    /// reading the audit log, not "may this caller do it".
    pub fn identifying_as(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn from_manifest(manifest: &DaemonManifest) -> Self {
        Self::new(manifest.control_base_url(), manifest.token.clone())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The bearer token this client presents, when the daemon requires one.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ControlClientError> {
        self.send(self.http.get(self.url(path)), path).await
    }

    async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ControlClientError> {
        let mut request = self.http.post(self.url(path));
        if let Some(body) = body {
            request = request.json(body);
        }
        self.send(request, path).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = match self.token.as_deref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        match self.label.as_deref() {
            Some(label) => request.header("x-relay-client", label),
            None => request,
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<T, ControlClientError> {
        let response = self.authorize(request).send().await.map_err(|error| {
            ControlClientError::Unreachable {
                url: self.url(path),
                detail: error.to_string(),
            }
        })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            let (code, message) = parse_error_body(&body);
            return Err(ControlClientError::Rejected {
                status: status.as_u16(),
                code,
                message,
            });
        }

        serde_json::from_str(&body).map_err(|e| ControlClientError::Protocol(e.to_string()))
    }

    /// Is a daemon answering at all? Used during discovery, so it never waits long.
    pub async fn health(&self) -> bool {
        let request = self
            .health_http
            .get(self.url("/api/v1/version"))
            .timeout(HEALTH_TIMEOUT);
        match self.authorize(request).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }

    pub async fn version(&self) -> Result<ApiVersionInfo, ControlClientError> {
        self.get_json("/api/v1/version").await
    }

    pub async fn status(
        &self,
    ) -> Result<relay_core_runtime::CoreStatusSnapshot, ControlClientError> {
        self.get_json("/api/v1/status").await
    }

    pub async fn metrics(&self) -> Result<relay_core_runtime::CoreMetrics, ControlClientError> {
        self.get_json("/api/v1/metrics").await
    }

    /// Full lifecycle state of the proxy the daemon owns.
    pub async fn proxy_lifecycle(&self) -> Result<RuntimeLifecycle, ControlClientError> {
        self.get_json("/api/v1/proxy/status").await
    }

    pub async fn proxy_start(
        &self,
        request: &ProxyStartRequest,
    ) -> Result<ProxyStartOutcome, ControlClientError> {
        self.post_json("/api/v1/proxy/start", Some(request)).await
    }

    pub async fn proxy_stop(&self) -> Result<ProxyStopOutcome, ControlClientError> {
        let empty: Option<&()> = None;
        self.post_json("/api/v1/proxy/stop", empty).await
    }

    /// The most recent lifecycle changes, newest last.
    ///
    /// This is how a client answers "why is this proxy running, and who stopped it last" without
    /// keeping a registry of connections that may already be gone.
    pub async fn lifecycle_audit(
        &self,
        limit: usize,
    ) -> Result<Vec<relay_core_runtime::audit::AuditEvent>, ControlClientError> {
        let snapshot: relay_core_runtime::CoreAuditSnapshot = self
            .get_json(&format!(
                "/api/v1/audit?kind=proxy_lifecycle_changed&limit={limit}"
            ))
            .await?;
        Ok(snapshot.events)
    }

    /// Ask the daemon to exit. Returns once the request is accepted, not once the process is gone.
    pub async fn shutdown(&self) -> Result<(), ControlClientError> {
        let empty: Option<&()> = None;
        let _: serde_json::Value = self.post_json("/api/v1/daemon/shutdown", empty).await?;
        Ok(())
    }
}

/// Pull `error.code` / `error.message` out of a control-plane failure body.
///
/// Falls back to the HTTP body when the daemon (or a proxy in between) answered with something
/// else, so a rejection is never reported as an empty message.
fn parse_error_body(body: &str) -> (String, String) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let trimmed = body.trim();

    if let Some(value) = parsed {
        let code = value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(|code| code.as_str())
            .or_else(|| value.get("error").and_then(|error| error.as_str()))
            .unwrap_or("request_failed")
            .to_string();
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(|message| message.as_str())
            .filter(|message| !message.is_empty())
            .unwrap_or(trimmed)
            .to_string();
        return (code, message);
    }

    (
        "request_failed".to_string(),
        if trimmed.is_empty() {
            "the daemon refused the request without a message".to_string()
        } else {
            trimmed.to_string()
        },
    )
}

/// What is running in a data directory.
#[derive(Debug)]
pub enum DaemonStatus {
    /// No manifest: nothing to attach to.
    NotRunning,
    /// A manifest exists but speaks a different control protocol.
    Incompatible { found: String, expected: String },
    /// A live pid owns the directory but `GET /api/v1/version` was not accepted (still starting,
    /// hung, or another process holding the published port). `reason` is that request's failure,
    /// so a 401 is not reported as an empty listener.
    Unresponsive {
        manifest: DaemonManifest,
        reason: String,
    },
    Running {
        manifest: Box<DaemonManifest>,
        client: ControlClient,
    },
}

impl DaemonStatus {
    pub fn manifest(&self) -> Option<&DaemonManifest> {
        match self {
            Self::Unresponsive { manifest, .. } => Some(manifest),
            Self::Running { manifest, .. } => Some(manifest),
            _ => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// Find the daemon owning `data_dir`, confirming it over the wire.
///
/// Discovery is a two-step check on purpose: a live pid is not proof of a working daemon (the
/// process may be starting, hung, or the manifest may be reused after a pid wrap), and a client
/// that skips the health check would attach to nothing and report empty traffic.
pub async fn connect(data_dir: &Path) -> DaemonStatus {
    match discover(data_dir) {
        Discovery::Absent | Discovery::Stale => DaemonStatus::NotRunning,
        Discovery::Incompatible { found, expected } => {
            DaemonStatus::Incompatible { found, expected }
        }
        Discovery::Running(manifest) => {
            let client = ControlClient::from_manifest(&manifest);
            match client.version().await {
                Ok(info) if info.api_version == CONTROL_API_VERSION => DaemonStatus::Running {
                    manifest: Box::new(manifest),
                    client,
                },
                // The manifest was current but the running daemon reports otherwise: trust the
                // daemon over the file it left behind.
                Ok(info) => {
                    remove_manifest(data_dir);
                    DaemonStatus::Incompatible {
                        found: info.api_version,
                        expected: CONTROL_API_VERSION.to_string(),
                    }
                }
                Err(error) => {
                    // The process can die between the liveness check and this request. Leaving
                    // the manifest in place would make the next client refuse to start a daemon.
                    if !process_alive(manifest.pid) {
                        remove_manifest(data_dir);
                        DaemonStatus::NotRunning
                    } else {
                        DaemonStatus::Unresponsive {
                            manifest,
                            reason: error.to_string(),
                        }
                    }
                }
            }
        }
    }
}
