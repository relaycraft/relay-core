use crate::commands::config::ConfigAction;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Intercept and debug HTTP traffic with ease",
    after_help = "\
Examples:
  relay start                    Start the daemon and the proxy (idempotent)
  relay status                   Show the daemon, the proxy and the MCP endpoint
  relay stop                     Stop the proxy, keep the daemon and its history
  relay shutdown                 Stop the proxy and the daemon
  relay run                      Run the daemon and the proxy in the foreground
  relay run --ui                 Foreground daemon with the interactive TUI attached
  relay run --web                Foreground daemon serving the Web UI on the API port
  relay config init              Write a commented config file and edit your defaults
  relay flows                    List captured flows (add --follow to watch live traffic)
  relay analyze --file flows.jsonl           Analyze captured flow data
  relay analyze --file export.har --format har  Analyze HAR export
  relay scripts init                        Scaffold a new script project
  relay scripts build                       Bundle script with esbuild
  relay ca generate              Generate CA certificate/key pair
  relay ca install               Install CA to system trust store (macOS)
  relay rules validate rules.json   Validate a rules file

Environment:
  RELAY_LOG       Log filter (default: info, e.g. debug, trace)
  RELAY_DATA_DIR       Data directory (default: ~/.relay-core)
  RELAY_CORE_TUI_THEME TUI preset when --theme is omitted (relay, slate, high-contrast)
  RELAY_CA_CERT        CA cert path (overrides data dir default)
  RELAY_CA_KEY         CA key path (overrides data dir default)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
// Exactly one of these is constructed, once, at process start — so the size difference between
// variants costs nothing, and clap's derive reads better than a boxed payload would.
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Start the RelayCore daemon and the proxy (idempotent)
    Start {
        /// Proxy listen address. The engine binds loopback only.
        #[arg(short, long, env = "RELAY_PROXY_LISTEN")]
        listen: Option<String>,

        /// Control API port for a daemon this command starts
        #[arg(long, env = "RELAY_API_PORT")]
        api_port: Option<u16>,

        /// MCP endpoint port for a daemon this command starts
        #[arg(long, env = "RELAY_MCP_PORT")]
        mcp_port: Option<u16>,

        /// Do not serve the MCP endpoint
        #[arg(long)]
        no_mcp: bool,

        /// Ensure the daemon runs but do not start the proxy
        #[arg(long)]
        no_proxy: bool,

        /// Enable transparent proxy mode (macOS PF / Linux TPROXY)
        #[arg(long)]
        transparent: bool,

        /// Enable UDP TPROXY on specified port (Linux only)
        #[arg(long)]
        udp_tproxy_port: Option<u16>,

        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Keep flows and rules in memory instead of the data directory database
        #[arg(long)]
        in_memory: bool,

        /// Append every flow update to this file as JSONL
        #[arg(long, value_name = "PATH")]
        save_stream: Option<PathBuf>,

        /// Stop the proxy after this many seconds without captured traffic (0 = never, the default)
        #[arg(long, env = "RELAY_PROXY_IDLE_TIMEOUT")]
        idle_timeout: Option<u64>,

        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Stop the proxy (the daemon keeps running, keeping history and rules)
    Stop {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Restart the proxy
    Restart {
        /// Proxy listen address
        #[arg(short, long, env = "RELAY_PROXY_LISTEN")]
        listen: Option<String>,

        /// Enable transparent proxy mode (macOS PF / Linux TPROXY)
        #[arg(long)]
        transparent: bool,

        /// Enable UDP TPROXY on specified port (Linux only)
        #[arg(long)]
        udp_tproxy_port: Option<u16>,

        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Show the daemon, the proxy and the MCP endpoint
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Stop the proxy and the daemon
    Shutdown {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Read, show and seed the configuration file
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Internal: run the daemon in the foreground
    #[command(hide = true)]
    Daemon {
        /// Control API port (falls back to an ephemeral port when taken)
        #[arg(long, env = "RELAY_API_PORT")]
        api_port: Option<u16>,

        /// Proxy listen port
        #[arg(long, env = "RELAY_PROXY_PORT")]
        proxy_port: Option<u16>,

        /// MCP endpoint port
        #[arg(long, env = "RELAY_MCP_PORT")]
        mcp_port: Option<u16>,

        /// Do not serve the MCP endpoint
        #[arg(long)]
        no_mcp: bool,

        /// Also start the proxy as the daemon comes up (clients normally ask for it explicitly)
        #[arg(long)]
        start_proxy: bool,

        /// Enable transparent proxy mode (macOS PF / Linux TPROXY)
        #[arg(long)]
        transparent: bool,

        /// Enable UDP TPROXY on specified port (Linux only)
        #[arg(long)]
        udp_tproxy_port: Option<u16>,

        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Keep flows and rules in memory only
        #[arg(long)]
        in_memory: bool,

        /// SQLite URL for persistence (default: a file in the data directory)
        #[arg(long)]
        db_url: Option<String>,

        /// Append every flow update to this file as JSONL
        #[arg(long, value_name = "PATH")]
        save_stream: Option<PathBuf>,

        /// Stop the proxy after this many seconds without captured traffic (0 = never, the default)
        #[arg(long, env = "RELAY_PROXY_IDLE_TIMEOUT")]
        idle_timeout: Option<u64>,
    },
    /// Run the daemon and the proxy in the foreground
    Run {
        /// Address to listen on (e.g., 127.0.0.1:8080)
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        listen: String,

        /// Enable UDP TPROXY on specified port (Linux only)
        #[arg(long)]
        udp_tproxy_port: Option<u16>,

        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Path to rules file (JSON/YAML)
        #[arg(long)]
        rules: Option<PathBuf>,

        /// Path to script file (JavaScript)
        #[cfg(feature = "script")]
        #[arg(long)]
        script: Option<PathBuf>,

        /// Enable script file watching
        #[cfg(feature = "script")]
        #[arg(long)]
        script_watch: bool,

        /// Enable TUI mode
        #[arg(long)]
        ui: bool,

        /// Enable Web UI mode (implies --api-port 8082 if not explicitly set)
        #[arg(long)]
        web: bool,

        /// Also serve the MCP endpoint from this process on this port (e.g. 18083).
        /// Agents that prefer a daemon they can share should use `relay start` instead.
        #[arg(long)]
        mcp_port: Option<u16>,

        /// TUI color preset (relay, slate, high-contrast). Overrides config; env: RELAY_CORE_TUI_THEME
        #[arg(long, env = "RELAY_CORE_TUI_THEME", value_name = "THEME")]
        theme: Option<String>,

        /// Enable transparent proxy mode (macOS PF / Linux TPROXY)
        #[arg(long)]
        transparent: bool,

        /// Record every flow update to this file as JSONL
        #[arg(long, value_name = "PATH")]
        save_stream: Option<PathBuf>,

        /// Enable REST/SSE HTTP API on this port (e.g. 8082).
        /// Exposes GET /api/v1/flows, /api/v1/rules, /api/v1/events, etc.
        #[arg(long)]
        api_port: Option<u16>,

        /// HTTP API bind address (default 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        api_bind: String,

        /// Bearer token for HTTP API authentication
        #[arg(long)]
        api_token: Option<String>,

        /// CORS allowed origins (comma-separated), e.g. "https://app.example.com,http://localhost:3000"
        #[arg(long)]
        api_cors: Option<String>,

        /// Comma-separated list of env vars accessible via relay.env(name) in scripts
        #[cfg(feature = "script")]
        #[arg(long)]
        script_env_allow: Option<String>,

        /// Comma-separated host allowlist for relay.fetch in scripts. Setting it enables
        /// relay.fetch; leaving it unset keeps it disabled. Use "*" to allow any host.
        #[cfg(feature = "script")]
        #[arg(long)]
        script_fetch_allow: Option<String>,

        /// Upstream proxy URL, e.g. "http://corp-proxy:8080" or "https://secure-proxy:8443"
        #[arg(long)]
        upstream: Option<String>,

        /// Username for upstream proxy Basic auth (password from RELAYCORE_UPSTREAM_PASSWORD env var)
        #[arg(long)]
        upstream_auth_user: Option<String>,

        /// Comma-separated hosts to bypass upstream (CIDR with "cidr:" prefix, IP literals, or globs)
        #[arg(long)]
        upstream_bypass: Option<String>,

        /// Allow fallback to direct connection when upstream is unreachable
        #[arg(long, default_value_t = false)]
        upstream_fail_open: bool,
    },
    /// Manage Certificate Authority
    Ca {
        #[command(subcommand)]
        action: CaAction,
    },
    /// Manage Intercept Rules
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Manage Scripts
    #[cfg(feature = "script")]
    Scripts {
        #[command(subcommand)]
        action: ScriptsAction,
    },
    /// List captured flows, or follow live traffic with --follow
    Flows {
        /// Control API base URL; discovered from the daemon manifest when omitted
        #[arg(long)]
        api_url: Option<String>,

        /// Stream new flows as they arrive instead of listing what has been captured.
        /// Filters do not apply to a live stream, so they cannot be combined with this.
        #[arg(long)]
        follow: bool,

        /// Output format (table, json, jsonl)
        #[arg(long, default_value = "table")]
        output: String,

        /// Filter expression (same as TUI `/` bar): host:api method:POST status:>=400 err ws
        #[arg(long)]
        filter: Option<String>,

        #[arg(long)]
        host: Option<String>,

        #[arg(long)]
        path: Option<String>,

        #[arg(long)]
        method: Option<String>,

        #[arg(long)]
        status_min: Option<u16>,

        #[arg(long)]
        status_max: Option<u16>,

        #[arg(long)]
        has_error: bool,

        /// Only list WebSocket flows
        #[arg(long)]
        websocket: bool,

        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Get Core Metrics
    Metrics {
        /// Proxy URL (where the metrics endpoint is exposed)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        proxy_url: String,

        /// Output format (table, json)
        #[arg(long, default_value = "table")]
        output: String,
    },
    /// Manage Transparent Proxy (macOS PF)
    #[cfg(any(feature = "transparent-linux", feature = "transparent-macos"))]
    Proxy {
        #[command(subcommand)]
        action: TransparentAction,
    },
    /// Analyze offline flow data from a JSONL or HAR file
    Analyze {
        /// Path to flow dump file (JSONL from --save-stream, or HAR export)
        #[arg(short, long)]
        file: PathBuf,

        /// Input format: jsonl (default) or har
        #[arg(long, default_value = "jsonl")]
        format: String,

        /// Output format: table (default) or json
        #[arg(long, default_value = "table")]
        output: String,

        /// Number of top slow requests to show
        #[arg(long, default_value = "10")]
        top_n: usize,
    },
}

#[cfg(any(feature = "transparent-linux", feature = "transparent-macos"))]
#[derive(Subcommand)]
pub enum TransparentAction {
    /// Generate PF configuration
    Generate {
        /// Proxy port
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Network interface to redirect (default: en0)
        #[arg(long, default_value = "en0")]
        interface: String,
    },
    /// Load PF rules and enable transparent proxy (requires sudo)
    Load {
        /// Proxy port
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Network interface to redirect (default: en0)
        #[arg(long, default_value = "en0")]
        interface: String,
    },
    /// Unload PF rules and disable transparent proxy (requires sudo)
    Unload,
    /// Check transparent proxy status
    Status,
}

#[derive(Subcommand)]
pub enum RulesAction {
    /// Validate a rules file
    Validate {
        /// Path to rules file
        file: PathBuf,
    },
    /// Print rules in standardized format
    Print {
        /// Path to rules file
        file: PathBuf,

        /// Output format (json, yaml)
        #[arg(long, default_value = "yaml")]
        format: String,
    },
    /// Test rules against a sample flow
    Test {
        /// Path to rules file
        file: PathBuf,

        /// Path to sample flow JSON
        #[arg(long)]
        flow: PathBuf,
    },
    /// List currently active rules from a running proxy via HTTP API
    List {
        /// API base URL (default: http://127.0.0.1:18082)
        #[arg(long, default_value = "http://127.0.0.1:18082")]
        api_url: String,
    },
}

#[cfg(feature = "script")]
#[derive(Subcommand)]
pub enum ScriptsAction {
    /// Validate a script file
    Validate {
        /// Path to script file
        file: PathBuf,
    },
    /// Run script once against a sample flow
    RunOnce {
        /// Path to script file
        file: PathBuf,

        /// Path to sample flow JSON
        #[arg(long)]
        flow: PathBuf,
    },
    /// Scaffold a new script project with esbuild bundling
    Init {
        /// Target directory (created if missing)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Bundle script with esbuild for production use
    Build {
        /// Entry script file (default: src/index.ts)
        #[arg(default_value = "src/index.ts")]
        entry: PathBuf,

        /// Output file (default: dist/bundle.js)
        #[arg(short, long, default_value = "dist/bundle.js")]
        out: PathBuf,
    },
    /// Watch and auto-bundle script on changes
    Dev {
        /// Entry script file (default: src/index.ts)
        #[arg(default_value = "src/index.ts")]
        entry: PathBuf,

        /// Output file (default: dist/bundle.js)
        #[arg(short, long, default_value = "dist/bundle.js")]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum CaAction {
    /// Generate CA (if missing). Use --force to overwrite.
    Generate {
        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Force regenerate even if exists
        #[arg(long)]
        force: bool,
    },
    /// Export CA certificate
    Export {
        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,

        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Export as DER format (suitable for Windows .cer import)
        #[arg(long, default_value_t = false)]
        der: bool,
    },
    /// Install CA certificate to system trust store
    Install {
        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,
    },
    /// Uninstall CA certificate from system trust store
    Uninstall {
        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,
    },
    /// Check CA certificate status
    Status {
        /// Path to CA certificate
        #[arg(long)]
        ca_cert: Option<PathBuf>,

        /// Path to CA key
        #[arg(long)]
        ca_key: Option<PathBuf>,
    },
}
