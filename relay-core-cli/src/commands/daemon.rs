//! The RelayCore daemon: the one process that owns the engine.
//!
//! Decision [`0007`](../../docs/decisions/0007-daemon-control-plane.md). This module is the host
//! side of the control plane — it holds the single `CoreState`, the single proxy lifecycle, the
//! data-directory lock and the manifest other processes discover it through. Everything else
//! (CLI commands, the MCP bridge, the Web UI) is a client of what this process serves.
//!
//! The daemon is normally started detached by `relay start`; `relay daemon` runs it in the
//! foreground for debugging.

use crate::logging;
use anyhow::{Context, Result};
use relay_core_http::control::{
    CONTROL_API_VERSION, DaemonLock, DaemonManifest, LockError, manifest_path, remove_manifest,
    write_manifest,
};
use relay_core_http::{HttpApiConfig, HttpApiServer};
use relay_core_runtime::paths;
use relay_core_runtime::services::{CoreProxyController, ProxyControlService, ProxyStartRequest};
use relay_core_runtime::{CoreState, RuntimeLifecyclePhase};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default ports come from the control-plane crate, so the config file, the daemon and the MCP
/// bridge cannot disagree about them.
pub use relay_core_http::control::{DEFAULT_API_PORT, DEFAULT_MCP_PORT};
pub use relay_core_runtime::services::DEFAULT_PROXY_PORT;

pub const DAEMON_LOG_FILE: &str = "daemon.log";

/// Where a daemon writes its log.
///
/// Not a detail the daemon can decide for itself: a spawned daemon inherits a redirected stderr
/// (the log file the spawner opened), while a daemon hosting a full-screen TUI must not write to
/// the terminal the TUI is painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonLog {
    /// stderr, which is the terminal in the foreground and the log file when spawned.
    Inherit,
    /// Append to `$RELAY_DATA_DIR/daemon.log`.
    ToFile,
}
pub const DAEMON_DB_FILE: &str = "daemon.db";

/// How long the daemon waits for the proxy to stop while exiting. Long enough to drain what is in
/// flight, short enough that `relay shutdown` feels immediate.
const EXIT_PROXY_STOP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    /// Requested control API port; falls back to an OS-assigned port when taken.
    pub api_port: u16,
    /// Address the control API binds. Loopback by default; a routable bind without a token is
    /// warned about at startup.
    pub api_bind: String,
    /// Bearer token clients must present. Generated when the daemon starts itself; a caller that
    /// supplies one is responsible for handing it to clients (it still lands in the manifest).
    pub api_token: Option<String>,
    /// CORS origins for the control API, comma separated.
    pub api_cors: Option<String>,
    /// Serve the embedded Web UI from the control API port.
    pub serve_webui: bool,
    pub proxy_port: u16,
    pub udp_tproxy_port: Option<u16>,
    pub transparent: bool,
    /// Serve the MCP endpoint on this port. `None` disables it.
    pub mcp_port: Option<u16>,
    /// Start the proxy as the daemon comes up.
    ///
    /// Off by default, and deliberately: a daemon that also starts a proxy races the client that
    /// is about to ask for one, so the same `relay start` reports `started` or `already_running`
    /// depending on timing. Clients ask for a proxy explicitly; the daemon only owns it.
    pub start_proxy: bool,
    pub ca_cert: Option<PathBuf>,
    pub ca_key: Option<PathBuf>,
    /// SQLite URL for flow/rule persistence; defaults to a file in the data directory.
    pub db_url: Option<String>,
    /// Keep everything in memory, so history dies with the daemon.
    pub in_memory: bool,
    /// Where the daemon logs.
    pub log: DaemonLog,
    /// Append every flow update to this file as JSONL. `None` records nothing.
    pub save_stream: Option<PathBuf>,
    /// Rules file loaded into the engine at startup.
    pub rules: Option<PathBuf>,
    /// Script file loaded at startup (feature `script`).
    pub script: Option<PathBuf>,
    /// Reload the script file when it changes.
    pub script_watch: bool,
    /// Comma-separated environment variables `relay.env()` may read.
    pub script_env_allow: Option<String>,
    /// Comma-separated allowlist for `relay.fetch`; unset keeps it disabled.
    pub script_fetch_allow: Option<String>,
    /// Upstream proxy URL.
    pub upstream: Option<String>,
    /// Username for upstream Basic auth (password from `RELAYCORE_UPSTREAM_PASSWORD`).
    pub upstream_auth_user: Option<String>,
    /// Comma-separated hosts to bypass the upstream.
    pub upstream_bypass: Option<String>,
    /// Fall back to a direct connection when the upstream is unreachable.
    pub upstream_fail_open: bool,
    /// Stop the proxy after this many seconds without captured traffic. `0` (the default) means
    /// never: the proxy is only stopped by an explicit command.
    ///
    /// Off by default on purpose. Any heuristic that closes the proxy "to save resources" shows up
    /// as the proxy mysteriously dying when an editor reloads or an agent restarts, and the user
    /// ends up debugging a proxy that is not there.
    pub idle_timeout_secs: u64,
}

/// Whether this process serves the embedded page.
///
/// `--no-web` wins, then an explicit `--web`, then the config file. The page is already in the
/// binary and the control API already requires the manifest token, so the default is to serve it.
pub fn resolve_serve_webui(web: bool, no_web: bool, configured: bool) -> bool {
    if no_web { false } else { web || configured }
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            api_port: DEFAULT_API_PORT,
            api_bind: "127.0.0.1".to_string(),
            api_token: None,
            api_cors: None,
            serve_webui: true,
            proxy_port: DEFAULT_PROXY_PORT,
            udp_tproxy_port: None,
            transparent: false,
            mcp_port: Some(DEFAULT_MCP_PORT),
            start_proxy: false,
            ca_cert: None,
            ca_key: None,
            db_url: None,
            in_memory: false,
            log: DaemonLog::Inherit,
            save_stream: None,
            rules: None,
            script: None,
            script_watch: false,
            script_env_allow: None,
            script_fetch_allow: None,
            upstream: None,
            upstream_auth_user: None,
            upstream_bypass: None,
            upstream_fail_open: false,
            idle_timeout_secs: 0,
        }
    }
}

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DAEMON_LOG_FILE)
}

/// A daemon that is up and serving.
///
/// The split from [`run`] exists for the foreground case: a host that also renders a UI needs the
/// control URL before the process exits, and needs a way to stop the daemon when the UI quits.
pub struct RunningDaemon {
    api_url: String,
    token: String,
    mcp_url: Option<String>,
    webui_url: Option<String>,
    shutdown: Arc<tokio::sync::Notify>,
    serving: tokio::task::JoinHandle<()>,
    /// Kept alive so the script file keeps being watched.
    _script_watcher: Option<ScriptWatcher>,
    /// Kept alive so buffered logs flush.
    _log_guard: Option<logging::WorkerGuard>,
    data_dir: PathBuf,
    /// Whether this process actually owns the engine. False when another daemon had the lock: the
    /// loser must not pretend to serve, and a foreground caller must not render a UI for it.
    owns_data_dir: bool,
}

impl RunningDaemon {
    /// The handle a process gets when another daemon already owns the data directory.
    fn already_owned(data_dir: PathBuf) -> Self {
        Self {
            api_url: String::new(),
            token: String::new(),
            mcp_url: None,
            webui_url: None,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            serving: tokio::spawn(async {}),
            _script_watcher: None,
            _log_guard: None,
            data_dir,
            owns_data_dir: false,
        }
    }

    /// Whether this process is the one serving the control API.
    pub fn is_owner(&self) -> bool {
        self.owns_data_dir
    }

    /// Control API URL, e.g. `http://127.0.0.1:8082`.
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// MCP endpoint URL, when the daemon serves one.
    pub fn mcp_url(&self) -> Option<&str> {
        self.mcp_url.as_deref()
    }

    /// Bearer token the control API requires. Handed to clients this process starts itself (the
    /// foreground TUI); other clients read it from the manifest.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Browser URL for the Web UI, token included in the fragment, when one is served.
    pub fn webui_url(&self) -> Option<&str> {
        self.webui_url.as_deref()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Serve until something asks for shutdown.
    pub async fn wait(self) -> Result<()> {
        let _ = self.serving.await;
        Ok(())
    }

    /// Ask the daemon to stop, and wait for it to finish.
    pub async fn shutdown(self) -> Result<()> {
        self.shutdown.notify_one();
        let _ = self.serving.await;
        Ok(())
    }
}

/// Run the daemon until it is asked to exit.
pub async fn run(options: DaemonOptions) -> Result<()> {
    start(options).await?.wait().await
}

/// Bring the daemon up and start serving.
pub async fn start(options: DaemonOptions) -> Result<RunningDaemon> {
    let data_dir = paths::resolve_data_dir();

    // The lock comes first: everything after it assumes this process is the only owner, and a
    // second daemon that got further would bind ports and publish a manifest before discovering
    // it lost.
    let lock = match DaemonLock::acquire(&data_dir) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning { pid }) => {
            // Not an error: `relay start` may have raced another `relay start`, and the loser
            // exiting quietly is what makes the race converge on one daemon.
            eprintln!(
                "RelayCore daemon: another daemon (pid {pid}) already owns {}; exiting.",
                data_dir.display()
            );
            return Ok(RunningDaemon::already_owned(data_dir));
        }
        Err(error) => return Err(anyhow::anyhow!(error.to_string())),
    };

    let log_guard = match options.log {
        DaemonLog::ToFile => Some(
            logging::init_daemon_log(&log_path(&data_dir))
                .with_context(|| format!("opening {}", log_path(&data_dir).display()))?,
        ),
        DaemonLog::Inherit => {
            logging::init_plain();
            None
        }
    };

    let state = Arc::new(CoreState::new(db_url(&options, &data_dir)).await);

    let controller = Arc::new(match options.save_stream.clone() {
        Some(path) => CoreProxyController::with_save_stream(state.clone(), path)
            .map_err(anyhow::Error::msg)?,
        None => CoreProxyController::new(state.clone()),
    });

    let shutdown = Arc::new(tokio::sync::Notify::new());
    let token = options.api_token.clone().unwrap_or_else(generate_token);
    let started_at_ms = now_ms();

    apply_startup_policy(&state, &options)?;
    load_rules_file(&state, &options).await?;
    let script_watcher = load_script_file(&state, &options).await?;

    let (server, api_addr) = bind_control_api(
        &state,
        controller.clone(),
        shutdown.clone(),
        &options,
        token.clone(),
    )
    .await?;

    exclude_own_endpoints(&state, api_addr.port(), options.mcp_port);

    let mcp_port = serve_mcp_endpoint(&state, controller.clone(), options.mcp_port).await;

    let manifest = DaemonManifest {
        pid: std::process::id(),
        api_version: CONTROL_API_VERSION.to_string(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        api_port: api_addr.port(),
        proxy_port: None,
        mcp_port,
        serve_webui: options.serve_webui,
        token: Some(token.clone()),
        started_at_ms,
        data_dir: data_dir.clone(),
    };
    write_manifest(&data_dir, &manifest).context("publishing the daemon manifest")?;

    let manifest_updater =
        spawn_manifest_updater(data_dir.clone(), controller.clone(), manifest.clone());
    spawn_signal_handler(shutdown.clone());

    tracing::info!(
        target: "relay_core_daemon",
        pid = std::process::id(),
        control = %format!("http://{api_addr}"),
        mcp = %mcp_port.map(|p| format!("http://127.0.0.1:{p}/mcp")).unwrap_or_else(|| "disabled".to_string()),
        webui = %if manifest.serve_webui {
            manifest.control_base_url()
        } else {
            "disabled".to_string()
        },
        data_dir = %data_dir.display(),
        stream = %controller.save_stream().map(|p| p.display().to_string()).unwrap_or_else(|| "off".to_string()),
        "daemon ready"
    );

    if options.start_proxy {
        let request = ProxyStartRequest {
            port: options.proxy_port,
            transparent: options.transparent,
            udp_tproxy_port: options.udp_tproxy_port,
            ca_cert: options.ca_cert.clone(),
            ca_key: options.ca_key.clone(),
        };
        // A proxy that cannot start must not take the control plane down with it: the client
        // that asked for it reads the failure from the lifecycle and reports it.
        if let Err(error) = controller
            .proxy_start(cli_requester("daemon --start-proxy"), request)
            .await
        {
            tracing::warn!(target: "relay_core_daemon", error = %error, "proxy did not start");
        }
    }

    if options.idle_timeout_secs > 0 {
        spawn_idle_stop(
            controller.clone(),
            Duration::from_secs(options.idle_timeout_secs),
        );
    }

    let api_url = format!("http://{api_addr}");
    let mcp_url = mcp_port.map(|port| format!("http://127.0.0.1:{port}/mcp"));
    let webui_url = manifest.webui_url();
    let serving_data_dir = data_dir.clone();

    let serving = tokio::spawn(async move {
        if let Err(error) = server.run().await {
            tracing::error!(target: "relay_core_daemon", error = %error, "control API stopped");
        }

        // The manifest means "this daemon is reachable", and it stopped being true the moment
        // serving ended. Removing it after the slower work below would leave a client that saw the
        // API go quiet still finding a manifest that looks like a running daemon.
        //
        // The updater is stopped and *awaited* first: it rewrites the manifest on every lifecycle
        // change, so a stop that merely removes the file can be undone by it a moment later,
        // stranding a manifest for a daemon that is gone.
        manifest_updater.abort();
        let _ = manifest_updater.await;
        remove_manifest(&serving_data_dir);

        let _ = tokio::time::timeout(
            EXIT_PROXY_STOP_TIMEOUT,
            controller.proxy_stop(runtime_requester("daemon shutdown")),
        )
        .await;
        tracing::info!(target: "relay_core_daemon", "daemon stopped");
        drop(lock);
    });

    Ok(RunningDaemon {
        api_url,
        token,
        mcp_url,
        webui_url,
        shutdown,
        serving,
        _script_watcher: script_watcher,
        _log_guard: log_guard,
        data_dir,
        owns_data_dir: true,
    })
}

/// A lifecycle request made by this process on behalf of the user.
fn cli_requester(label: &str) -> relay_core_runtime::services::Requester {
    relay_core_runtime::services::Requester::new(relay_core_runtime::audit::AuditActor::Cli)
        .label(label)
}

/// A lifecycle change the daemon makes on its own initiative.
fn runtime_requester(label: &str) -> relay_core_runtime::services::Requester {
    relay_core_runtime::services::Requester::new(relay_core_runtime::audit::AuditActor::Runtime)
        .label(label)
}

/// A live script-file watcher, kept by the daemon handle so watching survives as long as the
/// daemon does.
#[cfg(feature = "script")]
type ScriptWatcher = notify::RecommendedWatcher;

#[cfg(not(feature = "script"))]
type ScriptWatcher = ();

/// Apply the policy a host asked for before any traffic flows: the upstream proxy, and the rule
/// that the engine's own control endpoints are not "user traffic".
fn apply_startup_policy(state: &Arc<CoreState>, options: &DaemonOptions) -> Result<()> {
    let Some(upstream_url) = options.upstream.as_deref() else {
        return Ok(());
    };

    let auth = options.upstream_auth_user.as_ref().map(|user| {
        let password = std::env::var("RELAYCORE_UPSTREAM_PASSWORD").unwrap_or_default();
        if password.is_empty() {
            tracing::warn!(
                target: "relay_core_daemon",
                "RELAYCORE_UPSTREAM_PASSWORD is empty while --upstream-auth-user is set; \
                 upstream authentication will fail"
            );
        }
        relay_core_api::policy::UpstreamAuth::new(user.clone(), password)
    });

    let bypass_hosts: Vec<String> = options
        .upstream_bypass
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .collect();

    state.patch_policy_from(
        relay_core_runtime::audit::AuditActor::Runtime,
        "daemon --upstream".to_string(),
        relay_core_api::policy::ProxyPolicyPatch {
            redaction: None,
            upstream: Some(relay_core_api::policy::UpstreamProxyConfig {
                proxy_url: upstream_url.to_string(),
                auth,
                bypass_hosts,
                fail_open: options.upstream_fail_open,
            }),
            retention: None,
        },
    );
    tracing::info!(target: "relay_core_daemon", upstream = upstream_url, "upstream proxy configured");
    Ok(())
}

/// Keep the daemon's own endpoints out of the flow list.
///
/// With the proxy configured system-wide, the control API and the Web UI would otherwise show up as
/// user traffic — and an agent reading flows would see its own requests.
fn exclude_own_endpoints(state: &Arc<CoreState>, api_port: u16, mcp_port: Option<u16>) {
    // The proxy port is added when the proxy actually binds, and removed when it stops. Writing
    // the configured default here hid a real service on 8080 after the proxy moved to another port.
    let mut exclude = vec![
        format!("127.0.0.1:{api_port}"),
        format!("localhost:{api_port}"),
    ];
    if let Some(port) = mcp_port {
        exclude.push(format!("127.0.0.1:{port}"));
        exclude.push(format!("localhost:{port}"));
    }

    let mut policy = state.policy_snapshot();
    policy.capture_exclude = exclude;
    // The engine default keeps bodies off the live flow. This host is what CLI and MCP read, so it
    // asks for a bounded prefix while the body keeps streaming. Desktop still sets Full on its own.
    policy.body_observation = relay_core_api::body_plan::BodyObservation::Prefixed;
    state.update_policy_from(
        relay_core_runtime::audit::AuditActor::Cli,
        "daemon.capture_exclude".to_string(),
        policy,
    );
}

/// Load the rules file a host named, failing the daemon start when it cannot be used.
async fn load_rules_file(state: &Arc<CoreState>, options: &DaemonOptions) -> Result<()> {
    let Some(path) = &options.rules else {
        return Ok(());
    };

    let rules = crate::utils::load_rules(&path.clone())
        .with_context(|| format!("loading rules from {}", path.display()))?;
    let count = rules.len();
    state.set_legacy_rules(rules).await;
    tracing::info!(target: "relay_core_daemon", rules = count, file = %path.display(), "rules loaded");
    Ok(())
}

/// Load the script file, and watch it when asked.
async fn load_script_file(
    state: &Arc<CoreState>,
    options: &DaemonOptions,
) -> Result<Option<ScriptWatcher>> {
    #[cfg(not(feature = "script"))]
    {
        let _ = (state, options);
        Ok(None)
    }

    #[cfg(feature = "script")]
    {
        use notify::{RecursiveMode, Watcher};
        use relay_core_runtime::audit::AuditActor;

        if let Some(allowed) = &options.script_env_allow {
            let allow: std::collections::HashSet<String> = allowed
                .split(',')
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect();
            state.set_script_env_allow(allow).await;
        }

        // `relay.fetch` is off unless a host asks for it; without this the allowlist existed but no
        // host could enable the feature at all.
        if let Some(allowed) = &options.script_fetch_allow {
            let hosts: std::collections::HashSet<String> = allowed
                .split(',')
                .map(|host| host.trim().to_string())
                .filter(|host| !host.is_empty() && host != "*")
                .collect();
            let enabled = !allowed.trim().is_empty();
            state.set_script_fetch_allow(enabled, hosts).await;
        }

        let Some(script_path) = &options.script else {
            return Ok(None);
        };

        let content = std::fs::read_to_string(script_path)
            .with_context(|| format!("reading script {}", script_path.display()))?;
        state
            .load_script_from(
                AuditActor::Cli,
                "daemon.script.initial_load".to_string(),
                &content,
            )
            .await
            .map_err(|error| {
                anyhow::anyhow!("loading script {}: {error}", script_path.display())
            })?;

        if !options.script_watch {
            return Ok(None);
        }

        // Watch the parent directory: editors typically write a temporary file and rename it, which
        // a file-level watch never sees.
        let watch_path = script_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(script_path)
            .to_path_buf();
        let target = script_path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        let mut watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                match event {
                    Ok(event) => {
                        if event
                            .paths
                            .iter()
                            .any(|path| path.file_name().is_some_and(|name| name == target))
                        {
                            let _ = tx.blocking_send(());
                        }
                    }
                    Err(error) => {
                        tracing::error!(target: "relay_core_daemon", %error, "watch error")
                    }
                }
            })
            .context("creating the script watcher")?;
        watcher
            .watch(&watch_path, RecursiveMode::NonRecursive)
            .with_context(|| format!("watching {}", watch_path.display()))?;

        let reload_state = state.clone();
        let reload_path = script_path.clone();
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // A short pause so the write that triggered the event has finished.
                tokio::time::sleep(Duration::from_millis(100)).await;
                match std::fs::read_to_string(&reload_path) {
                    Ok(content) => {
                        match reload_state
                            .load_script_from(
                                AuditActor::Cli,
                                "daemon.script.reload".to_string(),
                                &content,
                            )
                            .await
                        {
                            Ok(()) => tracing::info!(
                                target: "relay_core_daemon",
                                file = %reload_path.display(),
                                "script reloaded"
                            ),
                            Err(error) => tracing::error!(
                                target: "relay_core_daemon",
                                %error,
                                "script reload failed; the previous script is still in effect"
                            ),
                        }
                    }
                    Err(error) => tracing::error!(
                        target: "relay_core_daemon",
                        %error,
                        "reading the script for reload failed"
                    ),
                }
            }
        });

        Ok(Some(watcher))
    }
}

fn db_url(options: &DaemonOptions, data_dir: &Path) -> Option<String> {
    if options.in_memory {
        return None;
    }
    Some(options.db_url.clone().unwrap_or_else(|| {
        let path = data_dir.join(DAEMON_DB_FILE);
        format!("sqlite://{}?mode=rwc", path.display())
    }))
}

/// A bearer token for the control API.
///
/// The API is loopback-only, so this is defence in depth rather than the primary boundary: it
/// stops another local account (or a browser page reaching localhost) from driving the proxy.
fn generate_token() -> String {
    use uuid::Uuid;
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn bind_control_api(
    state: &Arc<CoreState>,
    controller: Arc<CoreProxyController>,
    shutdown: Arc<tokio::sync::Notify>,
    options: &DaemonOptions,
    token: String,
) -> Result<(HttpApiServer, SocketAddr)> {
    let bind_ip: std::net::IpAddr = options
        .api_bind
        .parse()
        .with_context(|| format!("invalid --api-bind {:?}", options.api_bind))?;

    let build = |port: u16| {
        let mut config = HttpApiConfig::new(port);
        config.addr = SocketAddr::new(bind_ip, port);
        config.bearer_token = Some(token.clone());
        config.serve_webui = options.serve_webui;

        if let Some(cors) = &options.api_cors {
            let origins: Vec<_> = cors
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .filter_map(|origin| origin.parse().ok())
                .collect();
            if !origins.is_empty() {
                config = config.with_allowed_origins(origins);
            }
        }

        HttpApiServer::new(config, state.clone())
            .with_proxy_control(controller.clone())
            .with_shutdown_signal(shutdown.clone())
    };

    match build(options.api_port).bind().await {
        Ok(bound) => Ok(bound),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Falling back rather than failing keeps a stale Web UI or a second `relay run` from
            // making the daemon unusable. The real port is published in the manifest, which is
            // what clients read.
            tracing::warn!(
                target: "relay_core_daemon",
                port = options.api_port,
                "control API port is taken; binding an ephemeral port instead"
            );
            build(0).bind().await.with_context(|| {
                format!(
                    "binding the control API after {} was taken",
                    options.api_port
                )
            })
        }
        Err(error) => Err(error).context("binding the control API"),
    }
}

/// Serve the MCP endpoint from this process, sharing the daemon's engine.
///
/// Returns the port actually served, or `None` when disabled or unavailable — a busy MCP port must
/// not stop the daemon, because the control plane is what tells the user about it.
async fn serve_mcp_endpoint(
    state: &Arc<CoreState>,
    controller: Arc<CoreProxyController>,
    requested: Option<u16>,
) -> Option<u16> {
    #[cfg(not(feature = "mcp"))]
    {
        let _ = (state, controller, requested);
        None
    }

    #[cfg(feature = "mcp")]
    {
        use relay_core_probe::{ProbeConfig, ProbeServer, ProbeTransport};

        let requested = requested?;
        let port = free_loopback_port(requested).await?;
        let bind = IpAddr::from([127, 0, 0, 1]);
        let probe_state = state.clone();

        tokio::spawn(async move {
            let config = ProbeConfig {
                transport: ProbeTransport::Sse { port, bind },
            };
            // The controller is what makes `proxy_status` / `proxy_start` / `proxy_stop` work for
            // an agent: without it, MCP clients could read traffic but never start capturing it.
            let server = ProbeServer::new(config, probe_state).with_proxy_control(controller);
            if let Err(error) = server.run().await {
                tracing::error!(target: "relay_core_daemon", error = %error, "MCP endpoint stopped");
            }
        });

        Some(port)
    }
}

/// Pick a port for a sub-server, preferring `requested` and falling back to an ephemeral one.
#[cfg(feature = "mcp")]
async fn free_loopback_port(requested: u16) -> Option<u16> {
    if tokio::net::TcpListener::bind(("127.0.0.1", requested))
        .await
        .is_ok()
    {
        return Some(requested);
    }
    tracing::warn!(
        target: "relay_core_daemon",
        port = requested,
        "MCP port is taken; binding an ephemeral port instead"
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Keep the manifest's `proxy_port` in step with the proxy.
///
/// Clients read the manifest to learn where the proxy is; a manifest that keeps a port after the
/// proxy stopped is how a client ends up configuring a browser to use a dead port.
fn spawn_manifest_updater(
    data_dir: PathBuf,
    controller: Arc<CoreProxyController>,
    mut manifest: DaemonManifest,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = controller.proxy_lifecycle_watch();
        loop {
            let lifecycle = rx.borrow_and_update().clone();
            let port = match lifecycle.phase {
                RuntimeLifecyclePhase::Running => lifecycle.port,
                _ => None,
            };
            if manifest.proxy_port != port {
                manifest.proxy_port = port;
                if let Err(error) = write_manifest(&data_dir, &manifest) {
                    tracing::warn!(target: "relay_core_daemon", error = %error, "manifest update failed");
                }
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
}

/// Exit on SIGINT/SIGTERM by requesting the graceful shutdown the API also uses.
fn spawn_signal_handler(shutdown: Arc<tokio::sync::Notify>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async {
                    match terminate.as_mut() {
                        Some(signal) => { signal.recv().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        tracing::info!(target: "relay_core_daemon", "shutdown signal received");
        shutdown.notify_one();
    });
}

/// Where the daemon writes its log, for user-facing messages.
pub fn describe_log(data_dir: &Path) -> String {
    log_path(data_dir).display().to_string()
}

/// The manifest path, re-exported so the CLI can point users at it.
pub fn describe_manifest(data_dir: &Path) -> String {
    manifest_path(data_dir).display().to_string()
}

/// Add the proxy lifecycle watch accessor the manifest updater needs.
trait LifecycleWatch {
    fn proxy_lifecycle_watch(
        &self,
    ) -> tokio::sync::watch::Receiver<relay_core_runtime::RuntimeLifecycle>;
}

impl LifecycleWatch for CoreProxyController {
    fn proxy_lifecycle_watch(
        &self,
    ) -> tokio::sync::watch::Receiver<relay_core_runtime::RuntimeLifecycle> {
        self.state().subscribe_lifecycle()
    }
}

/// How often the idle check looks at the traffic counter.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Should an idle proxy be stopped?
///
/// Split out from the timer so the rule is testable without waiting: `None` means the user never
/// asked for this, and the answer is always no.
fn should_stop_idle(configured: Option<Duration>, idle_for: Duration, proxy_active: bool) -> bool {
    match configured {
        Some(timeout) => proxy_active && idle_for >= timeout,
        None => false,
    }
}

/// Stop the proxy after the configured idle period.
///
/// Only runs when the operator asked for it (`--idle-timeout`); the default is no automatic stop at
/// all. "Idle" is measured from the captured-flow counter, which is what the user means by "nothing
/// is happening" — a proxy that is up but capturing nothing is not useful, and a proxy seeing
/// traffic is never touched.
fn spawn_idle_stop(controller: Arc<CoreProxyController>, timeout: Duration) {
    tracing::info!(
        target: "relay_core_daemon",
        timeout_secs = timeout.as_secs(),
        "idle proxy shutdown is enabled"
    );

    tokio::spawn(async move {
        let mut last_flows = controller.state().get_metrics().await.flows_total;
        let mut idle_since = std::time::Instant::now();

        loop {
            tokio::time::sleep(IDLE_CHECK_INTERVAL).await;

            let metrics = controller.state().get_metrics().await;
            if metrics.flows_total != last_flows {
                last_flows = metrics.flows_total;
                idle_since = std::time::Instant::now();
                continue;
            }

            let active = controller.proxy_lifecycle().is_active();
            if !should_stop_idle(Some(timeout), idle_since.elapsed(), active) {
                if !active {
                    idle_since = std::time::Instant::now();
                }
                continue;
            }

            tracing::info!(
                target: "relay_core_daemon",
                idle_secs = idle_since.elapsed().as_secs(),
                "stopping the idle proxy; rules and history are kept"
            );
            if let Err(error) = controller
                .proxy_stop(runtime_requester("idle timeout"))
                .await
            {
                tracing::warn!(target: "relay_core_daemon", error = %error, "idle stop failed");
            }
            idle_since = std::time::Instant::now();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfigured_idle_timeout_never_stops_the_proxy() {
        assert!(!should_stop_idle(None, Duration::from_secs(86_400), true));
    }

    #[test]
    fn a_configured_idle_timeout_stops_only_an_idle_active_proxy() {
        let timeout = Some(Duration::from_secs(900));

        assert!(
            should_stop_idle(timeout, Duration::from_secs(901), true),
            "an active proxy idle past the timeout is stopped"
        );
        assert!(
            !should_stop_idle(timeout, Duration::from_secs(899), true),
            "traffic within the window keeps the proxy"
        );
        assert!(
            !should_stop_idle(timeout, Duration::from_secs(9_999), false),
            "a stopped proxy is not stopped again"
        );
    }

    #[tokio::test]
    async fn the_daemon_keeps_a_body_prefix_for_agents() {
        let state = Arc::new(relay_core_runtime::CoreState::new(None).await);
        assert_eq!(
            state.policy_snapshot().body_observation,
            relay_core_api::body_plan::BodyObservation::Off,
            "the engine default stays off until this host declares otherwise"
        );

        exclude_own_endpoints(&state, 18082, Some(18083));

        let policy = state.policy_snapshot();
        assert_eq!(
            policy.body_observation,
            relay_core_api::body_plan::BodyObservation::Prefixed
        );
        assert!(
            policy
                .capture_exclude
                .iter()
                .any(|host| host == "127.0.0.1:18082")
        );
        assert!(
            policy
                .capture_exclude
                .iter()
                .all(|host| host != "127.0.0.1:8080" && host != "localhost:8080"),
            "the proxy port is excluded only while the proxy is listening on it"
        );
    }

    #[test]
    fn the_daemon_does_not_start_a_proxy_by_default() {
        // The daemon owns the engine; clients ask for a proxy. Making this default true again
        // would reintroduce the race where `relay start` reports "started" or "already_running"
        // depending on how fast the daemon booted.
        assert!(!DaemonOptions::default().start_proxy);
        assert_eq!(DaemonOptions::default().idle_timeout_secs, 0);
        assert!(
            DaemonOptions::default().serve_webui,
            "the page is part of the daemon; starting a proxy is a separate command"
        );
    }

    #[test]
    fn the_page_follows_no_web_then_web_then_config() {
        assert!(resolve_serve_webui(false, false, true));
        assert!(
            resolve_serve_webui(true, false, false),
            "--web serves the page when the config file turned it off"
        );
        assert!(
            !resolve_serve_webui(true, true, true),
            "--no-web wins over --web and the config file"
        );
        assert!(!resolve_serve_webui(false, false, false));
    }
}
