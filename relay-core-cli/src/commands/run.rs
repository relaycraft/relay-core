use crate::server;
use crate::ui::app::{ApiMode, TuiApp};
use crate::ui::theme;
use crate::utils::load_rules;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(feature = "script")]
use notify::{RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use ratatui::{Terminal, backend::CrosstermBackend};
use relay_core_api::flow::{Flow, FlowUpdate, Layer, WebSocketMessage};
use relay_core_http::{HttpApiConfig, HttpApiServer};
use relay_core_lib::intercept::types::{
    BoxError, HttpBody, Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use relay_core_runtime::{CaPaths, CoreState, ProxyConfig, ProxySpawnResult, audit::AuditActor};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

struct CliInterceptor {
    enabled: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Interceptor for CliInterceptor {
    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(RequestAction::Continue(body));
        }
        Ok(RequestAction::Continue(body))
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        Ok(WebSocketMessageAction::Continue(message))
    }
}

struct CliSink {
    output: String,
    writer: Option<Mutex<BufWriter<std::fs::File>>>,
    flow_tx: tokio::sync::broadcast::Sender<FlowUpdate>,
    ui_enabled: bool,
}

impl CliSink {
    fn new(
        output: String,
        save_stream: Option<PathBuf>,
        flow_tx: tokio::sync::broadcast::Sender<FlowUpdate>,
        ui_enabled: bool,
    ) -> Self {
        let writer = if let Some(path) = save_stream {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(file) => Some(Mutex::new(BufWriter::new(file))),
                Err(e) => {
                    error!("Failed to open save_stream file {:?}: {}", path, e);
                    std::process::exit(1);
                }
            }
        } else {
            None
        };
        Self {
            output,
            writer,
            flow_tx,
            ui_enabled,
        }
    }

    async fn process_updates(&self, mut rx: mpsc::Receiver<FlowUpdate>) {
        while let Some(update) = rx.recv().await {
            // Send to subscribers (TUI + IPC)
            // Ignore SendError (no subscribers)
            let _ = self.flow_tx.send(update.clone());

            if let FlowUpdate::Full(flow) = &update {
                // Only print to stdout if not in TUI mode
                if !self.ui_enabled {
                    match self.output.as_str() {
                        "jsonl" => {
                            if let Ok(json) = serde_json::to_string(flow) {
                                println!("{}", json);
                            }
                        }
                        "json" => {
                            if let Ok(json) = serde_json::to_string_pretty(flow) {
                                println!("{}", json);
                            }
                        }
                        _ => {
                            let url = match &flow.layer {
                                Layer::Http(h) => h.request.url.to_string(),
                                Layer::WebSocket(w) => w.handshake_request.url.to_string(),
                                _ => "unknown".to_string(),
                            };
                            let method = match &flow.layer {
                                Layer::Http(h) => h.request.method.clone(),
                                Layer::WebSocket(w) => w.handshake_request.method.clone(),
                                _ => "".to_string(),
                            };
                            info!("[Flow] {} {} {}", flow.id, method, url);
                        }
                    }
                } // End of !ui_enabled check

                if let Some(mutex) = &self.writer
                    && let Ok(mut w) = mutex.lock()
                    && let Ok(json) = serde_json::to_string(flow)
                {
                    let _ = writeln!(w, "{}", json);
                }
            } else if let FlowUpdate::WebSocketMessage { flow_id, message } = &update {
                // For now, only log WS messages in table/default mode and if UI is disabled
                if !self.ui_enabled && self.output == "table" {
                    info!("[WS] [{}] {} bytes", flow_id, message.content.size);
                }
            }
        }
    }
}

async fn run_tui_broadcast(
    mut app: TuiApp,
    mut rx: tokio::sync::broadcast::Receiver<FlowUpdate>,
) -> Result<()> {
    use std::sync::atomic::AtomicBool;

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

fn startup_row(label: &str, value: impl std::fmt::Display) -> String {
    format!("  {:7} {value}", label)
}

/// Endpoint summary printed once; Control API bind is logged at debug only.
fn log_startup_endpoints(
    addr: std::net::SocketAddr,
    control_port: u16,
    api_bind: &str,
    api_port: Option<u16>,
) {
    info!("──────────────────────────────────────────────");
    info!("{}", startup_row("Proxy", addr));
    info!(
        "{}",
        startup_row("Control", format!("http://127.0.0.1:{control_port}/"))
    );
    if let Some(port) = api_port {
        info!(
            "{}",
            startup_row("REST", format!("http://{api_bind}:{port}/api/v1/"))
        );
    }
    info!(
        "{}",
        startup_row("TUI", "run with --ui for interactive mode")
    );
    info!("──────────────────────────────────────────────");
}

#[allow(clippy::too_many_arguments)]
pub async fn execute(
    listen: String,
    control_port: u16,
    udp_tproxy_port: Option<u16>,
    ca_cert: Option<PathBuf>,
    ca_key: Option<PathBuf>,
    rules: Option<PathBuf>,
    #[cfg(feature = "script")] script: Option<PathBuf>,
    #[cfg(feature = "script")] script_watch: bool,
    #[cfg(feature = "script")] script_env_allow: Option<String>,
    ui: bool,
    theme: Option<String>,
    transparent: bool,
    output: String,
    save_stream: Option<PathBuf>,
    api_port: Option<u16>,
    api_bind: String,
    api_token: Option<String>,
    api_cors: Option<String>,
    upstream: Option<String>,
    upstream_auth_user: Option<String>,
    upstream_bypass: Option<String>,
    upstream_fail_open: bool,
) -> Result<()> {
    let ca_paths = CaPaths::resolve(ca_cert, ca_key).map_err(anyhow::Error::msg)?;
    if !ca_paths.cert.exists() || !ca_paths.key.exists() {
        return Err(anyhow::anyhow!(
            "CA files not found:\n  cert: {}\n  key: {}\nRun `relay-core-cli ca generate` first.",
            ca_paths.cert.display(),
            ca_paths.key.display()
        ));
    }

    let state = Arc::new(CoreState::new(None).await);
    let interception_enabled = Arc::new(AtomicBool::new(true));

    // Configure upstream proxy if --upstream is set
    if let Some(upstream_url) = upstream.as_deref() {
        let auth = upstream_auth_user.map(|user| {
            let password = std::env::var("RELAYCORE_UPSTREAM_PASSWORD").unwrap_or_default();
            if password.is_empty() {
                error!(
                    "RELAYCORE_UPSTREAM_PASSWORD env var is required when --upstream-auth-user is set"
                );
            }
            relay_core_api::policy::UpstreamAuth::new(user, password)
        });
        let bypass_hosts: Vec<String> = upstream_bypass
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let upstream_cfg = relay_core_api::policy::UpstreamProxyConfig {
            proxy_url: upstream_url.to_string(),
            auth,
            bypass_hosts,
            fail_open: upstream_fail_open,
        };
        state.patch_policy_from(
            AuditActor::Runtime,
            "cli --upstream".to_string(),
            relay_core_api::policy::ProxyPolicyPatch {
                redaction: None,
                upstream: Some(upstream_cfg),
            },
        );
        info!("Upstream proxy configured: {}", upstream_url);
    }

    // Create broadcast channel for flow updates (TUI + WebSocket)
    let (flow_tx, _) = tokio::sync::broadcast::channel(1024);

    // Start legacy Control API Server (WebSocket flow stream + intercept toggle)
    let server_tx = flow_tx.clone();
    let server_interception = interception_enabled.clone();
    tokio::spawn(async move {
        if let Err(e) = server::start_server(control_port, server_tx, server_interception).await {
            tracing::error!("Control API server error: {e:#}");
        }
    });

    // Start REST/SSE HTTP API server (if --api-port specified)
    if let Some(port) = api_port {
        let bind_addr = std::net::SocketAddr::new(
            api_bind
                .parse()
                .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1])),
            port,
        );
        let api_state = state.clone();
        let api_token = api_token.clone();
        let api_cors = api_cors.clone();
        tokio::spawn(async move {
            let mut cfg = HttpApiConfig::new(port);
            cfg.addr = bind_addr;
            if let Some(token) = api_token {
                cfg = cfg.with_bearer_token(token);
            }
            if let Some(cors) = api_cors {
                let origins: Vec<_> = cors
                    .split(',')
                    .filter_map(|s| {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    })
                    .collect();
                if !origins.is_empty() {
                    cfg = cfg
                        .with_allowed_origins(origins.into_iter().filter_map(|s| s.parse().ok()));
                }
            }
            let srv = HttpApiServer::new(cfg, api_state);
            if let Err(e) = srv.run().await {
                error!("HTTP API server error: {}", e);
            }
        });
        if !ui {
            info!("HTTP API listening on {}", bind_addr);
        }
    }

    // Configure TUI
    let tui_rx = if ui { Some(flow_tx.subscribe()) } else { None };

    // Parse address
    let addr: std::net::SocketAddr = listen.parse().expect("Invalid listen address");
    let port = addr.port();

    // Load rules (JSON/YAML)
    if let Some(rules_path) = &rules {
        match load_rules(rules_path) {
            Ok(rules) => {
                state.set_legacy_rules(rules).await;
                if !ui {
                    info!("Loaded rules from {:?}", rules_path);
                }
            }
            Err(e) => {
                error!("Failed to parse rules file: {}", e);
                return Err(anyhow::anyhow!("Failed to parse rules file: {}", e));
            }
        }
    }

    #[cfg(feature = "script")]
    let _watcher = {
        // S5: Set env var whitelist for relay.env() before loading script
        #[cfg(feature = "script")]
        if let Some(ref allowed) = script_env_allow {
            let env_allow: std::collections::HashSet<String> = allowed
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            state.set_script_env_allow(env_allow).await;
        }

        let mut watcher: Option<RecommendedWatcher> = None;
        if let Some(script_path) = &script {
            // Initial load
            match std::fs::read_to_string(script_path) {
                Ok(content) => {
                    if let Err(e) = state
                        .load_script_from(
                            AuditActor::Cli,
                            "cli.script.initial_load".to_string(),
                            &content,
                        )
                        .await
                    {
                        error!("Failed to load script: {}", e);
                    } else if !ui {
                        info!("Loaded script from {:?}", script_path);
                    }
                }
                Err(e) => error!("Failed to read script file: {}", e),
            }

            if script_watch {
                // Setup watcher
                let script_path_clone = script_path.clone();
                let state = state.clone();

                let (tx, mut rx) = tokio::sync::mpsc::channel(1);

                // Watch parent directory to handle atomic writes (rename/replace)
                let watch_path = script_path.parent().unwrap_or(script_path).to_path_buf();
                let target_filename = script_path.file_name().unwrap_or_default().to_os_string();

                let watcher_res =
                    notify::recommended_watcher(move |res: NotifyResult<notify::Event>| {
                        match res {
                            Ok(event) => {
                                // Check if event affects our target file
                                let interested = event.paths.iter().any(|p| {
                                    p.file_name().map(|n| n == target_filename).unwrap_or(false)
                                });

                                if interested {
                                    // Send event for any modification/creation
                                    let _ = tx.blocking_send(());
                                }
                            }
                            Err(e) => error!("Watch error: {:?}", e),
                        }
                    });

                match watcher_res {
                    Ok(mut w) => {
                        if let Err(e) = w.watch(&watch_path, RecursiveMode::NonRecursive) {
                            error!("Failed to watch script directory: {}", e);
                        } else {
                            watcher = Some(w);
                            if !ui {
                                info!("Watching script file for changes...");
                            }

                            // Spawn reloader task
                            tokio::spawn(async move {
                                while rx.recv().await.is_some() {
                                    if !ui {
                                        info!("Script file changed, reloading...");
                                    }
                                    // Add a small delay to ensure file write is complete
                                    tokio::time::sleep(tokio::time::Duration::from_millis(100))
                                        .await;

                                    match std::fs::read_to_string(&script_path_clone) {
                                        Ok(content) => {
                                            if let Err(e) = state
                                                .load_script_from(
                                                    AuditActor::Cli,
                                                    "cli.script.reload".to_string(),
                                                    &content,
                                                )
                                                .await
                                            {
                                                error!("Failed to reload script: {}", e);
                                            } else if !ui {
                                                info!("Script reloaded successfully");
                                            }
                                        }
                                        Err(e) => error!(
                                            "Failed to read script file during reload: {}",
                                            e
                                        ),
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => error!("Failed to create watcher: {}", e),
                }
            }
        }
        watcher
    };

    let config = ProxyConfig::new(port, ca_paths.cert.clone(), ca_paths.key.clone())
        .with_transparent(transparent)
        .with_udp_tproxy_port(udp_tproxy_port);

    // Create flow channel for proxy -> sink
    let (proxy_tx, proxy_rx) = mpsc::channel(1000);

    // Create sink and spawn processor
    // flow_tx is the broadcast channel created earlier
    let sink = Arc::new(CliSink::new(output, save_stream, flow_tx.clone(), ui));
    let sink_clone = sink.clone();
    tokio::spawn(async move {
        sink_clone.process_updates(proxy_rx).await;
    });

    let extra_interceptor = Some(Arc::new(CliInterceptor {
        enabled: interception_enabled.clone(),
    }) as Arc<dyn Interceptor>);

    if ui {
        let theme_id = theme::resolve_theme(theme).map_err(anyhow::Error::msg)?;
        theme::init(theme_id);
        info!("TUI theme: {} — {}", theme_id.id(), theme_id.description());

        // Spawn proxy in background if TUI is enabled
        let state = state.clone();
        let config = config.clone();
        let proxy_tx = proxy_tx.clone();
        let extra = extra_interceptor.clone();

        match state.spawn_proxy(config, proxy_tx, extra) {
            Ok(ProxySpawnResult::Started(_)) => {}
            Ok(ProxySpawnResult::AlreadyRunning) => {
                error!("Failed to start proxy: already running")
            }
            Err(e) => error!("Failed to start proxy: {}", e),
        }

        // Determine API mode: SSE + rules panel if --api-port is set
        let api_mode = if api_port.is_some() {
            ApiMode::Connected
        } else {
            ApiMode::Offline
        };
        let app = TuiApp::new(port, api_mode);
        debug!("Proxy listening on {} | Press ? for help, q to quit", addr);

        if let Some(rx) = tui_rx
            && let Err(e) = run_tui_broadcast(app, rx).await
        {
            let _ = disable_raw_mode();
            eprintln!("TUI error: {}", e);
        }
    } else {
        log_startup_endpoints(addr, control_port, &api_bind, api_port);
        debug!(
            "{}",
            startup_row(
                "CA",
                format!(
                    "cert={} key={}",
                    ca_paths.cert.display(),
                    ca_paths.key.display()
                )
            )
        );
        info!("Starting proxy on {}...", addr);

        if let Err(e) = state.start_proxy(config, proxy_tx, extra_interceptor).await {
            error!("Failed to start proxy: {}", e);
        }
    }

    Ok(())
}
