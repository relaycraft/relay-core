//! Lifecycle commands: `start`, `stop`, `restart`, `status`, `shutdown`.
//!
//! Decision [`0007`](../../docs/decisions/0007-daemon-control-plane.md). These commands are the
//! user-facing half of the control plane: they discover the daemon, start one when there is none,
//! and drive the proxy through the control API rather than by starting an engine of their own.

use crate::commands::daemon;
use anyhow::{Context, Result, bail};
use relay_core_api::CLI_COMMAND;
use relay_core_http::control::{
    BootstrapError, ControlClient, DaemonStatus, SpawnRequest, connect, find_host_binary,
    spawn_detached, wait_until_ready,
};
use relay_core_runtime::paths;
use relay_core_runtime::services::{ProxyStartOutcome, ProxyStartRequest, ProxyStopOutcome};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long `start` waits for a freshly spawned daemon to publish its manifest and answer.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `shutdown` waits for the daemon to stop answering.
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Audit label shown by `status`, e.g. `cli:relay-core stop`.
fn cli_actor(action: &str) -> String {
    format!("cli:{CLI_COMMAND} {action}")
}

#[derive(Debug, Clone)]
pub struct StartOptions {
    /// Proxy listen address (`host:port`). Only the port reaches the engine.
    pub listen: String,
    pub api_port: Option<u16>,
    pub mcp_port: Option<u16>,
    pub no_mcp: bool,
    /// Forwarded to a daemon this command spawns. The page stays on unless this or the config says so.
    pub no_web: bool,
    /// Ensure the daemon exists but leave the proxy alone.
    pub no_proxy: bool,
    pub transparent: bool,
    pub udp_tproxy_port: Option<u16>,
    pub ca_cert: Option<PathBuf>,
    pub ca_key: Option<PathBuf>,
    pub in_memory: bool,
    /// Record every flow update to this file (JSONL) in the daemon this command starts.
    pub save_stream: Option<PathBuf>,
    /// Forwarded to a daemon this command spawns; 0 means never stop the proxy automatically.
    pub idle_timeout_secs: u64,
    pub json: bool,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            listen: format!("127.0.0.1:{}", daemon::DEFAULT_PROXY_PORT),
            api_port: None,
            mcp_port: None,
            no_mcp: false,
            no_web: false,
            no_proxy: false,
            transparent: false,
            udp_tproxy_port: None,
            ca_cert: None,
            ca_key: None,
            in_memory: false,
            save_stream: None,
            idle_timeout_secs: 0,
            json: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StatusOptions {
    pub json: bool,
}

/// `relay start` — idempotently ensure a daemon with a running proxy.
pub async fn start(options: StartOptions) -> Result<()> {
    let data_dir = paths::resolve_data_dir();
    let client = ensure_daemon(&data_dir, &options).await?;

    if options.no_proxy {
        print_started(&client, None, options.json).await?;
        return Ok(());
    }

    let request = proxy_request(&options)?;
    match client
        .clone()
        .identifying_as(cli_actor("start"))
        .proxy_start(&request)
        .await
    {
        Ok(outcome) => print_started(&client, Some(outcome), options.json).await,
        Err(error) => bail!("{error}"),
    }
}

/// `relay stop` — stop the proxy, leave the daemon (and its history) running.
pub async fn stop(json: bool) -> Result<()> {
    let data_dir = paths::resolve_data_dir();

    let client = match connect(&data_dir).await {
        DaemonStatus::Running { client, .. } => client,
        DaemonStatus::NotRunning => {
            // Idempotent: stopping something already stopped is the state the caller asked for.
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "proxy": "not_running", "daemon": "not_running" })
                );
            } else {
                println!(
                    "RelayCore is not running (no daemon in {}).",
                    data_dir.display()
                );
            }
            return Ok(());
        }
        other => bail!("{}", describe_unusable(&data_dir, other)),
    };

    match stop_proxy(&client, &cli_actor("stop")).await {
        Ok(ProxyStopOutcome::Stopped) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "proxy": "stopped", "daemon": "running" })
                );
            } else {
                println!(
                    "Proxy stopped. The daemon is still running — history and rules are kept."
                );
                println!("Run `{CLI_COMMAND} shutdown` to stop the daemon as well.");
            }
        }
        Ok(ProxyStopOutcome::AlreadyStopped) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "proxy": "already_stopped", "daemon": "running" })
                );
            } else {
                println!("Proxy is not running; the daemon is.");
            }
        }
        Err(error) => bail!("{error}"),
    }
    Ok(())
}

/// `relay restart` — stop the proxy if it runs, then start it with the given options.
pub async fn restart(options: StartOptions) -> Result<()> {
    let data_dir = paths::resolve_data_dir();

    if let DaemonStatus::Running { client, .. } = connect(&data_dir).await {
        match stop_proxy(&client, &cli_actor("restart")).await {
            Ok(_) => {}
            Err(error) => bail!("{error}"),
        }
    }

    start(options).await
}

/// `relay status` — report what is running.
pub async fn status(options: StatusOptions) -> Result<()> {
    let data_dir = paths::resolve_data_dir();

    match connect(&data_dir).await {
        DaemonStatus::Running { manifest, client } => {
            let lifecycle = client.proxy_lifecycle().await.ok();
            // Supplementary, so a failure here must not break `status` — but it must not look like
            // "no lifecycle change was ever recorded" either.
            let (last_change, audit_error) = match client.lifecycle_audit(1).await {
                Ok(events) => (events.into_iter().next_back(), None),
                Err(error) => (None, Some(error.to_string())),
            };
            if options.json {
                let payload = serde_json::json!({
                    "daemon": {
                        "status": "running",
                        "pid": manifest.pid,
                        "engine_version": manifest.engine_version,
                        "control_url": manifest.control_base_url(),
                        "api_version": manifest.api_version,
                        "started_at_ms": manifest.started_at_ms,
                        "data_dir": manifest.data_dir,
                        "log": daemon::describe_log(&data_dir),
                    },
                    "proxy": lifecycle.as_ref().map(|l| serde_json::json!({
                        "phase": l.phase.as_str(),
                        "port": l.port,
                        "started_at_ms": l.started_at_ms,
                        "last_error": l.last_error,
                    })),
                    "mcp_url": manifest.mcp_port.map(|p| format!("http://127.0.0.1:{p}/mcp")),
                    "webui_url": manifest.webui_url(),
                    "last_change_error": audit_error,
                    "last_change": last_change.as_ref().map(|event| serde_json::json!({
                        "change": event.details.get("change"),
                        "actor": event.actor.as_str(),
                        "requested_by": event.details.get("requested_by"),
                        "outcome": event.outcome.as_str(),
                        "timestamp_ms": event.timestamp_ms,
                        "error": event.details.get("error"),
                    })),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("RelayCore daemon");
                println!("  status:   running (pid {})", manifest.pid);
                println!("  control:  {}", manifest.control_base_url());
                println!("  version:  {}", manifest.engine_version);
                match &lifecycle {
                    Some(lifecycle) => {
                        let where_ = lifecycle
                            .port
                            .map(|port| format!(" on 127.0.0.1:{port}"))
                            .unwrap_or_default();
                        println!(
                            "  proxy:    {}{}{}",
                            lifecycle.phase.as_str(),
                            where_,
                            uptime_suffix(lifecycle.started_at_ms),
                        );
                        if let Some(error) = lifecycle.last_error.as_deref() {
                            println!("  error:    {error}");
                        }
                    }
                    None => println!("  proxy:    unknown (control API did not answer)"),
                }
                if let Some(error) = &audit_error {
                    println!("  last:     unknown (could not read the audit trail: {error})");
                } else if let Some(event) = &last_change {
                    let change = event
                        .details
                        .get("change")
                        .and_then(|value| value.as_str())
                        .unwrap_or("changed");
                    let who = event
                        .details
                        .get("requested_by")
                        .and_then(|value| value.as_str())
                        .unwrap_or_else(|| event.actor.as_str());
                    println!(
                        "  last:     {} by {}{}",
                        change,
                        who,
                        ago_suffix(event.timestamp_ms)
                    );
                }
                println!(
                    "  mcp:      {}",
                    manifest
                        .mcp_port
                        .map(|port| format!("http://127.0.0.1:{port}/mcp"))
                        .unwrap_or_else(|| "disabled".to_string())
                );
                if let Some(webui) = manifest.webui_url() {
                    // Printed with the token embedded, because that is the only way a browser can
                    // authenticate: everything else about this line is a puzzle otherwise.
                    println!("  web ui:   {webui}");
                }
                println!("  data dir: {}", data_dir.display());
                println!("  log:      {}", daemon::describe_log(&data_dir));
            }
            Ok(())
        }
        DaemonStatus::NotRunning => bail!(
            "RelayCore daemon is not running ({} has no manifest). Start one with `{CLI_COMMAND} start`.",
            data_dir.display()
        ),
        other => bail!("{}", describe_unusable(&data_dir, other)),
    }
}

/// `relay shutdown` — stop the daemon (and the proxy it owns).
pub async fn shutdown(json: bool) -> Result<()> {
    let data_dir = paths::resolve_data_dir();

    let client = match connect(&data_dir).await {
        DaemonStatus::Running { client, .. } => client,
        DaemonStatus::NotRunning => {
            if json {
                println!("{}", serde_json::json!({ "daemon": "not_running" }));
            } else {
                println!("RelayCore daemon is not running.");
            }
            return Ok(());
        }
        other => bail!("{}", describe_unusable(&data_dir, other)),
    };

    // Stop the proxy first so in-flight connections are drained while the control plane is still
    // there to report progress; the daemon also stops it on the way out.
    let _ = stop_proxy(&client, &cli_actor("shutdown")).await;

    client.shutdown().await?;

    // All three conditions, because any one alone is a half-truth: a daemon that stopped answering
    // but still holds its registry, or one that gave up the registry while its proxy drains and its
    // data-directory lock is still held — in which case an immediate `relay start` would be refused
    // by a daemon that is already gone. A caller that reads "stopped" must not have to poll.
    let deadline = Instant::now() + DAEMON_STOP_TIMEOUT;
    loop {
        let answering = client.health().await;
        let registered = relay_core_http::control::manifest_path(&data_dir).exists();
        let owned = relay_core_http::control::daemon_lock_path(&data_dir).exists();

        if !answering && !registered && !owned {
            if json {
                println!("{}", serde_json::json!({ "daemon": "stopped" }));
            } else {
                println!("RelayCore daemon stopped.");
            }
            return Ok(());
        }

        if Instant::now() >= deadline {
            let mut still = Vec::new();
            if answering {
                still.push("answering");
            }
            if registered {
                still.push("registered in the data directory");
            }
            if owned {
                still.push("holding the data-directory lock");
            }
            bail!(
                "the daemon accepted the shutdown request but is still {} after {}s; see {}",
                still.join(" and "),
                DAEMON_STOP_TIMEOUT.as_secs(),
                daemon::describe_log(&data_dir)
            );
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Stop the proxy, telling the daemon who asked.
async fn stop_proxy(
    client: &ControlClient,
    label: &str,
) -> Result<ProxyStopOutcome, relay_core_http::control::ControlClientError> {
    client.clone().identifying_as(label).proxy_stop().await
}

/// Find the daemon, starting one when there is none.
async fn ensure_daemon(data_dir: &Path, options: &StartOptions) -> Result<ControlClient> {
    match connect(data_dir).await {
        DaemonStatus::Running { client, .. } => Ok(client),
        DaemonStatus::NotRunning => {
            let pid = spawn_daemon(data_dir, options)?;
            match wait_until_ready(
                data_dir,
                pid,
                &daemon::log_path(data_dir),
                DAEMON_START_TIMEOUT,
            )
            .await
            {
                Ok(client) => Ok(client),
                Err(error) => Err(anyhow::Error::new(error)),
            }
        }
        other => bail!("{}", describe_unusable(data_dir, other)),
    }
}

/// Start `relay-core-cli daemon` detached, writing to the daemon log.
fn spawn_daemon(data_dir: &Path, options: &StartOptions) -> Result<u32> {
    // The binary that is running right now is the host: same binary, `daemon` subcommand.
    let program = std::env::current_exe()
        .ok()
        .or_else(find_host_binary)
        .ok_or_else(|| anyhow::Error::new(BootstrapError::HostBinaryNotFound))?;

    let request = SpawnRequest {
        program,
        args: daemon_args(options),
        log_file: daemon::log_path(data_dir),
    };

    spawn_detached(&request).map_err(anyhow::Error::new)
}

/// Arguments for a daemon that outlives this command.
///
/// Deliberately no `--start-proxy`: the daemon owns the engine, and the caller's own `proxy_start`
/// is what starts a proxy, so the outcome of `relay start` does not depend on how fast the daemon
/// boots.
fn daemon_args(options: &StartOptions) -> Vec<String> {
    let mut args = vec!["daemon".to_string()];
    args.push("--api-port".to_string());
    args.push(
        options
            .api_port
            .unwrap_or(daemon::DEFAULT_API_PORT)
            .to_string(),
    );

    match options.mcp_port {
        Some(port) if !options.no_mcp => {
            args.push("--mcp-port".to_string());
            args.push(port.to_string());
        }
        _ => {}
    }
    if options.no_mcp {
        args.push("--no-mcp".to_string());
    }
    if options.no_web {
        args.push("--no-web".to_string());
    }
    if options.transparent {
        args.push("--transparent".to_string());
    }
    if let Some(port) = options.udp_tproxy_port {
        args.push("--udp-tproxy-port".to_string());
        args.push(port.to_string());
    }
    if let Some(path) = &options.ca_cert {
        args.push("--ca-cert".to_string());
        args.push(path.display().to_string());
    }
    if let Some(path) = &options.ca_key {
        args.push("--ca-key".to_string());
        args.push(path.display().to_string());
    }
    if options.in_memory {
        args.push("--in-memory".to_string());
    }
    if let Some(path) = &options.save_stream {
        args.push("--save-stream".to_string());
        args.push(path.display().to_string());
    }
    if options.idle_timeout_secs > 0 {
        args.push("--idle-timeout".to_string());
        args.push(options.idle_timeout_secs.to_string());
    }

    args
}

fn proxy_addr(listen: &str) -> Result<SocketAddr> {
    listen
        .parse()
        .with_context(|| format!("invalid listen address: {listen} (expected host:port)"))
}

fn proxy_request(options: &StartOptions) -> Result<ProxyStartRequest> {
    let addr = proxy_addr(&options.listen)?;
    if !addr.ip().is_loopback() {
        // The engine binds loopback regardless (`run_proxy` hardcodes 127.0.0.1), so accepting a
        // routable address here would silently ignore it.
        bail!(
            "the proxy binds loopback only; {listen} is not a loopback address",
            listen = options.listen
        );
    }

    Ok(ProxyStartRequest {
        port: addr.port(),
        transparent: options.transparent,
        udp_tproxy_port: options.udp_tproxy_port,
        ca_cert: options.ca_cert.clone(),
        ca_key: options.ca_key.clone(),
    })
}

async fn print_started(
    client: &ControlClient,
    outcome: Option<ProxyStartOutcome>,
    json: bool,
) -> Result<()> {
    let lifecycle = client.proxy_lifecycle().await.ok();
    let manifest_url = client.base_url().to_string();

    if json {
        let payload = serde_json::json!({
            "daemon": "running",
            "control_url": manifest_url,
            "proxy": outcome.as_ref().map(|outcome| match outcome {
                ProxyStartOutcome::Started { port } => serde_json::json!({ "outcome": "started", "port": port }),
                ProxyStartOutcome::AlreadyRunning { port } => serde_json::json!({ "outcome": "already_running", "port": port }),
            }),
            "phase": lifecycle.as_ref().map(|l| l.phase.as_str()),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("RelayCore daemon is running.");
    println!("  control: {manifest_url}");
    match outcome {
        Some(ProxyStartOutcome::Started { port }) => {
            println!("  proxy:   started on 127.0.0.1:{port}");
            println!();
            println!("Configure your client to use http://127.0.0.1:{port} as its HTTP proxy.");
            println!(
                "Stop the proxy with `{CLI_COMMAND} stop`; stop the daemon with `{CLI_COMMAND} shutdown`."
            );
        }
        Some(ProxyStartOutcome::AlreadyRunning { port }) => {
            println!("  proxy:   already running on 127.0.0.1:{port}");
        }
        None => {
            println!("  proxy:   left alone (--no-proxy)");
        }
    }
    Ok(())
}

fn describe_unusable(data_dir: &Path, status: DaemonStatus) -> String {
    match status {
        DaemonStatus::Unresponsive { manifest, reason } => format!(
            "a RelayCore daemon (pid {}) failed the control handshake at {} ({reason}). See {} — stop it with `{CLI_COMMAND} shutdown`, or kill pid {}.",
            manifest.pid,
            manifest.control_base_url(),
            daemon::describe_log(data_dir),
            manifest.pid
        ),
        DaemonStatus::Incompatible { found, expected } => format!(
            "a RelayCore daemon speaks control protocol {found}, but this build speaks {expected}. \
             Stop the running daemon (`{CLI_COMMAND} shutdown`) or upgrade this client."
        ),
        DaemonStatus::NotRunning | DaemonStatus::Running { .. } => {
            "the daemon state changed while this command was running; retry".to_string()
        }
    }
}

/// " (2m ago)" for an audit timestamp.
fn ago_suffix(timestamp_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seconds = now.saturating_sub(timestamp_ms) / 1000;
    if seconds < 60 {
        format!(" ({seconds}s ago)")
    } else if seconds < 3600 {
        format!(" ({}m ago)", seconds / 60)
    } else {
        format!(" ({}h ago)", seconds / 3600)
    }
}

fn uptime_suffix(started_at_ms: Option<u64>) -> String {
    let Some(started_at_ms) = started_at_ms else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seconds = now.saturating_sub(started_at_ms) / 1000;
    if seconds < 60 {
        format!(" (up {seconds}s)")
    } else if seconds < 3600 {
        format!(" (up {}m)", seconds / 60)
    } else {
        format!(" (up {}h{}m)", seconds / 3600, (seconds % 3600) / 60)
    }
}
