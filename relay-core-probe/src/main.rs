//! relay-core-probe entry point: a stdio bridge to the RelayCore daemon.
//!
//! This binary is a **transport**, not an engine. It discovers the daemon that owns the proxy,
//! starting one if necessary, and forwards MCP messages between stdin/stdout and the daemon's MCP
//! endpoint (decision [`0007`](../docs/decisions/0007-daemon-control-plane.md)).
//!
//! Consequences that are the point of the design:
//!
//! * Connecting a client starts no proxy and owns no flow history, so several clients (a CLI, an
//!   editor, an agent framework) share one engine instead of racing for port 8080.
//! * Tool semantics live only in the server: a tool cannot mean one thing over stdio and another
//!   over HTTP, because stdio carries the same JSON-RPC.
//! * A bridge that is killed takes nothing with it — the proxy and its history outlive it.

use relay_core_http::control::config as relay_config;
use relay_core_http::control::{
    BootstrapError, DaemonManifest, DaemonStatus, SpawnRequest, connect, find_host_binary,
    spawn_detached, wait_until_ready,
};
use relay_core_probe::bridge::{BridgeOptions, run_stdio_bridge};
use relay_core_runtime::paths;
use std::path::Path;
use std::time::Duration;

/// How long the bridge waits for a daemon it started itself.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(15);

fn parse_arg(prefix: &str) -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix(prefix).map(|v| v.to_string()))
}

fn parse_arg_env(arg_prefix: &str, env_key: &str) -> Option<String> {
    parse_arg(arg_prefix).or_else(|| std::env::var(env_key).ok())
}

/// A boolean switch: present as a flag, or set to `1` in any of `env_keys`.
fn flag(arg: &str, env_keys: &[&str]) -> bool {
    std::env::args().any(|a| a == arg)
        || env_keys
            .iter()
            .any(|key| std::env::var(key).is_ok_and(|value| value == "1"))
}

#[tokio::main]
async fn main() {
    // Logs go to stderr: stdout is the MCP channel.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    // `replay_flow` issues real HTTPS requests, and rustls needs a process-level provider before
    // any client config is built — otherwise the first replay panics instead of failing.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let options = match resolve_bridge_options().await {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    eprintln!(
        "relay-core MCP bridge -> {} (the daemon owns the proxy; see `relay status`)",
        options.mcp_url
    );

    if let Err(error) = run_stdio_bridge(options).await {
        eprintln!("relay-core MCP bridge error: {error}");
        std::process::exit(1);
    }
}

/// Find the daemon's MCP endpoint, starting a daemon when there is none.
async fn resolve_bridge_options() -> Result<BridgeOptions, String> {
    // An explicit endpoint skips discovery entirely, for a daemon reached through a different data
    // directory or a tunnel.
    if let Some(url) = parse_arg_env("--mcp-url=", "RELAY_MCP_URL") {
        return Ok(BridgeOptions {
            mcp_url: url,
            token: parse_arg_env("--token=", "RELAY_MCP_TOKEN"),
        });
    }

    let data_dir = paths::resolve_data_dir();

    match connect(&data_dir).await {
        DaemonStatus::Running { manifest, .. } => bridge_options_for(&manifest, &data_dir),
        DaemonStatus::NotRunning => {
            if !may_autostart(&data_dir)? {
                return Err(format!(
                    "no RelayCore daemon is running in {} and auto-start is disabled.\n\
                     Start one with `relay start`, or enable [client] autostart_daemon in {} / unset \
                     RELAY_MCP_NO_AUTOSTART.",
                    data_dir.display(),
                    relay_config::config_path(&data_dir).display()
                ));
            }
            let manifest = start_daemon(&data_dir).await?;
            bridge_options_for(&manifest, &data_dir)
        }
        DaemonStatus::Unresponsive(manifest) => Err(format!(
            "a RelayCore daemon (pid {}) is not answering at {}. See {} — stop it with `relay shutdown`, or kill pid {}.",
            manifest.pid,
            manifest.control_base_url(),
            data_dir.join("daemon.log").display(),
            manifest.pid
        )),
        DaemonStatus::Incompatible { found, expected } => Err(format!(
            "the running RelayCore daemon speaks control protocol {found}, but this build speaks {expected}. \
             Stop it with `relay shutdown`, or upgrade @relay-core/mcp."
        )),
    }
}

fn bridge_options_for(manifest: &DaemonManifest, data_dir: &Path) -> Result<BridgeOptions, String> {
    let Some(port) = manifest.mcp_port else {
        return Err(format!(
            "the RelayCore daemon in {} has its MCP endpoint disabled (started with --no-mcp).\n\
             Restart it with `relay shutdown && relay start`, or point this bridge at an endpoint \
             with --mcp-url.",
            data_dir.display()
        ));
    };

    Ok(BridgeOptions {
        mcp_url: format!("http://127.0.0.1:{port}/mcp"),
        token: manifest.token.clone(),
    })
}

/// Whether this bridge may start a daemon.
///
/// Three ways to say no, in the order the project resolves everything: the flag, the environment,
/// and the config file. A config file that cannot be parsed is an error rather than a default,
/// because "auto-start is off" and "your config has a typo" are different problems.
fn may_autostart(data_dir: &Path) -> Result<bool, String> {
    if flag(
        "--no-autostart",
        &["RELAY_MCP_NO_AUTOSTART", "RELAY_NO_AUTOSTART"],
    ) {
        return Ok(false);
    }

    let config = relay_config::load(data_dir).map_err(|error| {
        format!(
            "{error}\nFix or remove the file: auto-start settings cannot be read while it is broken."
        )
    })?;
    Ok(config.client.autostart_daemon)
}

/// Start `relay-core-cli daemon` and wait until it publishes a control API.
async fn start_daemon(data_dir: &Path) -> Result<DaemonManifest, String> {
    let program =
        find_host_binary().ok_or_else(|| BootstrapError::HostBinaryNotFound.to_string())?;
    let log_file = data_dir.join("daemon.log");

    eprintln!(
        "relay-core MCP bridge: no daemon is running in {}; starting one ({})",
        data_dir.display(),
        program.display()
    );

    let request = SpawnRequest {
        program,
        // The daemon serves the proxy and the MCP endpoint; it does not start a proxy on its own.
        // Starting the proxy stays an explicit command (`relay start`, or the proxy_start tool).
        args: vec!["daemon".to_string()],
        log_file: log_file.clone(),
    };
    let pid = spawn_detached(&request).map_err(|error| error.to_string())?;

    match wait_until_ready(data_dir, pid, &log_file, DAEMON_START_TIMEOUT).await {
        Ok(_) => match connect(data_dir).await {
            DaemonStatus::Running { manifest, .. } => Ok(*manifest),
            _ => Err("the RelayCore daemon started but did not publish its manifest".to_string()),
        },
        Err(error) => Err(error.to_string()),
    }
}
