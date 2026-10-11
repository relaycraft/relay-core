//! Which daemon a CLI command talks to, and how it authenticates.
//!
//! Every command that reads live state has to answer the same two questions: where is the control
//! API, and what credential does it want. Answering them per command is how this CLI grew two
//! control planes that disagreed. `rules list` defaulted to `127.0.0.1:18082` — a port no daemon
//! binds, the control API defaults to 8082 — and sent no credential, so with a live daemon on the
//! default port it still failed, in two independent ways.
//!
//! So resolution lives here, and commands ask for a [`ControlClient`] instead of assembling URLs.
//! (A command may still target the *proxy* listener when that is the point — `metrics --proxy-url`
//! reads the proxy's own unauthenticated endpoint — but reaching live engine state goes through the
//! daemon.)

use anyhow::{Result, bail};
use relay_core_api::CLI_COMMAND;
use relay_core_http::control::{ControlClient, DaemonStatus, connect};
use relay_core_runtime::paths;

/// Where the daemon is, and how to authenticate to it.
///
/// Discovered rather than defaulted: a daemon that fell back to an ephemeral port would make a
/// hard-coded `127.0.0.1:8082` connect to something else, or to nothing.
pub struct ResolvedDaemon {
    pub base_url: String,
    pub token: Option<String>,
}

impl ResolvedDaemon {
    /// A control client for this daemon, labelled for the daemon's audit trail.
    pub fn client(&self, label: &str) -> ControlClient {
        ControlClient::new(self.base_url.clone(), self.token.clone()).identifying_as(label)
    }
}

/// Find the daemon to talk to: the one named by `--api-url`, or the one running right now.
///
/// An explicit URL still needs a credential, and `RELAY_API_TOKEN` is where one comes from — that is
/// the same variable the MCP bridge uses. Omitting it against a token-protected daemon is reported
/// by the daemon as a rejection rather than guessed at here.
pub async fn resolve_daemon(explicit: Option<&str>) -> Result<ResolvedDaemon> {
    if let Some(url) = explicit {
        return Ok(ResolvedDaemon {
            base_url: url.trim_end_matches('/').to_string(),
            token: std::env::var("RELAY_API_TOKEN").ok(),
        });
    }

    let data_dir = paths::resolve_data_dir();
    match connect(&data_dir).await {
        DaemonStatus::Running { manifest, .. } => Ok(ResolvedDaemon {
            base_url: manifest.control_base_url(),
            token: manifest.token.clone(),
        }),
        DaemonStatus::NotRunning => bail!(
            "no RelayCore daemon is running in {}. Start one with `{CLI_COMMAND} start`, or pass --api-url.",
            data_dir.display()
        ),
        DaemonStatus::Unresponsive { manifest, reason } => bail!(
            "the RelayCore daemon (pid {}) failed the control handshake at {} ({reason})",
            manifest.pid,
            manifest.control_base_url()
        ),
        DaemonStatus::Incompatible { found, expected } => bail!(
            "the running RelayCore daemon speaks control protocol {found}, this build speaks {expected}"
        ),
    }
}
