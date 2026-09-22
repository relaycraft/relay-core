//! `relay run` — the daemon in the foreground.
//!
//! Decision [`0007`](../../docs/decisions/0007-daemon-control-plane.md). `run` used to be a second
//! implementation of the engine: its own `CoreState`, its own legacy control API on another port,
//! its own proxy. Nothing could see it (no manifest), and running it next to a daemon produced two
//! engines and two flow histories.
//!
//! It is now the same daemon as `relay start`, in the foreground: it publishes a manifest, serves
//! the control API and the MCP endpoint, and `--ui` renders a TUI that is a *client* of it — so the
//! terminal sees exactly what every other client sees.

use crate::commands::daemon::{self, DaemonLog, DaemonOptions};
use crate::sse_client;
use crate::ui::app::TuiApp;
use crate::ui::theme;
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use relay_core_api::CLI_COMMAND;
use relay_core_api::flow::FlowUpdate;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info};

/// How many flow updates the UI may fall behind before they are counted as skipped.
const UI_CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Proxy listen address.
    pub listen: String,
    pub ui: bool,
    pub web: bool,
    pub theme: Option<String>,
    pub rules: Option<PathBuf>,
    pub script: Option<PathBuf>,
    pub script_watch: bool,
    pub script_env_allow: Option<String>,
    pub script_fetch_allow: Option<String>,
    pub transparent: bool,
    pub udp_tproxy_port: Option<u16>,
    pub ca_cert: Option<PathBuf>,
    pub ca_key: Option<PathBuf>,
    pub api_port: Option<u16>,
    pub api_bind: String,
    pub api_token: Option<String>,
    pub api_cors: Option<String>,
    pub mcp_port: Option<u16>,
    pub save_stream: Option<PathBuf>,
    pub upstream: Option<String>,
    pub upstream_auth_user: Option<String>,
    pub upstream_bypass: Option<String>,
    pub upstream_fail_open: bool,
}

pub async fn execute(options: RunOptions) -> Result<()> {
    if options.web && options.ui {
        bail!(
            "--web and --ui are mutually exclusive; use --web for the browser dashboard or --ui for the terminal UI"
        );
    }

    let addr: std::net::SocketAddr = options
        .listen
        .parse()
        .with_context(|| format!("invalid --listen {:?} (expected host:port)", options.listen))?;
    if !addr.ip().is_loopback() {
        bail!(
            "the proxy binds loopback only; {} is not a loopback address",
            options.listen
        );
    }

    let daemon_options = DaemonOptions {
        api_port: options.api_port.unwrap_or(daemon::DEFAULT_API_PORT),
        api_bind: options.api_bind.clone(),
        api_token: options.api_token.clone(),
        api_cors: options.api_cors.clone(),
        serve_webui: options.web,
        proxy_port: addr.port(),
        udp_tproxy_port: options.udp_tproxy_port,
        transparent: options.transparent,
        mcp_port: options.mcp_port,
        start_proxy: true,
        ca_cert: options.ca_cert.clone(),
        ca_key: options.ca_key.clone(),
        db_url: None,
        in_memory: false,
        // A full-screen UI owns the terminal, so its logs go to the data directory.
        log: if options.ui {
            DaemonLog::ToFile
        } else {
            DaemonLog::Inherit
        },
        save_stream: options.save_stream.clone(),
        rules: options.rules.clone(),
        script: options.script.clone(),
        script_watch: options.script_watch,
        script_env_allow: options.script_env_allow.clone(),
        script_fetch_allow: options.script_fetch_allow.clone(),
        upstream: options.upstream.clone(),
        upstream_auth_user: options.upstream_auth_user.clone(),
        upstream_bypass: options.upstream_bypass.clone(),
        upstream_fail_open: options.upstream_fail_open,
        idle_timeout_secs: 0,
    };

    let daemon = daemon::start(daemon_options).await?;
    if !daemon.is_owner() {
        bail!(
            "another RelayCore daemon already owns this data directory; \
             run `{CLI_COMMAND} status` to inspect it, or set RELAY_DATA_DIR to use a separate one"
        );
    }

    if options.ui {
        let theme_id = theme::resolve_theme(options.theme.clone()).map_err(anyhow::Error::msg)?;
        theme::init(theme_id);
        info!("TUI theme: {} — {}", theme_id.id(), theme_id.description());

        let app = TuiApp::new(addr.port());
        let rx = subscribe_to_flows(&daemon).await?;
        debug!(
            "Proxy {} | control {} | press ? for help, q to quit",
            addr,
            daemon.api_url()
        );

        if let Err(error) = run_tui_broadcast(app, rx).await {
            let _ = disable_raw_mode();
            eprintln!("TUI error: {error}");
        }

        // Foreground semantics: quitting the UI ends the session it started.
        daemon.shutdown().await?;
        return Ok(());
    }

    log_startup_endpoints(&daemon, addr);
    daemon.wait().await
}

/// Subscribe to the daemon's live flow stream.
///
/// The UI is a client like any other: it reads `/api/v1/events` over the control API instead of
/// reaching into the engine. The SSE reader feeds an mpsc that a forwarder turns into the broadcast
/// channel the UI already consumes, which keeps the "count what the UI missed" behaviour.
async fn subscribe_to_flows(
    daemon: &daemon::RunningDaemon,
) -> Result<broadcast::Receiver<FlowUpdate>> {
    let (tx, mut rx) = mpsc::channel::<FlowUpdate>(UI_CHANNEL_CAPACITY);
    let (flow_tx, flow_rx) = broadcast::channel::<FlowUpdate>(UI_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            // A send error only means every subscriber is gone, i.e. the UI quit.
            let _ = flow_tx.send(update);
        }
    });

    let client = sse_client::ApiClient::new(
        daemon.api_url().to_string(),
        Some(daemon.token().to_string()),
    );
    tokio::spawn(async move {
        if let Err(error) = client.stream_events(tx).await {
            error!("flow stream ended: {error}");
        }
    });

    Ok(flow_rx)
}

/// What the foreground host is serving, so a user does not have to run `relay status` to find out.
fn log_startup_endpoints(daemon: &daemon::RunningDaemon, addr: std::net::SocketAddr) {
    info!("──────────────────────────────────────────────");
    info!("{}", row("Proxy", addr.to_string()));
    info!("{}", row("Control", format!("{}/", daemon.api_url())));
    if let Some(mcp) = daemon.mcp_url() {
        info!("{}", row("MCP", mcp));
    }
    if let Some(webui) = daemon.webui_url() {
        info!("{}", row("Web UI", webui));
    }
    info!("{}", row("Data", daemon.data_dir().display().to_string()));
    info!(
        "{}",
        row(
            "Stop",
            format!("`{CLI_COMMAND} stop` keeps the daemon; `{CLI_COMMAND} shutdown` ends it")
        )
    );
    info!("──────────────────────────────────────────────");
}

fn row(label: &str, value: impl std::fmt::Display) -> String {
    format!("{label:<8}{value}")
}

async fn run_tui_broadcast(mut app: TuiApp, mut rx: broadcast::Receiver<FlowUpdate>) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_tui = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown_tui.store(true, Ordering::Relaxed);
    });

    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        }
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let _guard = TerminalGuard;

    let initial_size = terminal.size()?;
    if initial_size.width < 60 || initial_size.height < 12 {
        eprintln!(
            "Terminal too small: need >= 60x12, got {}x{}.\n\
             Please resize the window and try again.",
            initial_size.width, initial_size.height
        );
        return Ok(());
    }

    let tick_rate = std::time::Duration::from_millis(250);
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|f| app.ui(f))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_default();

        if crossterm::event::poll(timeout)? {
            match event::read()? {
                Event::Key(event) => app.on_key(event),
                Event::Resize(w, h) => {
                    if w < 60 || h < 12 {
                        app.toast = Some("Terminal too small — resize to ≥ 60×12".into());
                    } else {
                        terminal.draw(|f| app.ui(f))?;
                    }
                }
                _ => {}
            }
        }

        if shutdown.load(Ordering::Relaxed) {
            app.should_quit = true;
        }

        if app.should_quit {
            break;
        }

        while let Ok(update) = rx.try_recv() {
            if let FlowUpdate::Full(flow) = update {
                app.on_flow(*flow);
            }
        }
        if let Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) = rx.try_recv() {
            tracing::warn!("TUI lagged behind by {n} flow updates, resyncing");
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = std::time::Instant::now();
        }
    }

    Ok(())
}
