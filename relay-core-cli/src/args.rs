use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Intercept and debug HTTP traffic with ease",
    after_help = "\
Examples:
  relay run                      Start proxy in background (default)
  relay run --ui                 Start proxy with interactive TUI
  relay flows --output table     View captured flows in table format
  relay analyze --file flows.jsonl           Analyze captured flow data
  relay analyze --file export.har --format har  Analyze HAR export
  relay scripts init                        Scaffold a new script project
  relay scripts build                       Bundle script with esbuild
  relay ca init                  Generate a CA certificate for HTTPS interception
  relay ca install               Install CA to system trust store (macOS)
  relay rules validate rules.json   Validate a rules file

Environment:
  RELAY_LOG     Log filter (default: info, e.g. debug, trace)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the proxy server
    Run {
        /// Address to listen on (e.g., 127.0.0.1:8080)
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        listen: String,

        /// Control API port
        #[arg(short, long, default_value = "8081")]
        control_port: u16,

        /// Enable UDP TPROXY on specified port (Linux only)
        #[arg(long)]
        udp_tproxy_port: Option<u16>,

        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        ca_cert: PathBuf,

        /// Path to CA key
        #[arg(long, default_value = "ca_key.pem")]
        ca_key: PathBuf,

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

        /// Enable transparent proxy mode (macOS PF / Linux TPROXY)
        #[arg(long)]
        transparent: bool,

        /// Output format (table, json, jsonl)
        #[arg(long, default_value = "table")]
        output: String,

        /// Save flow stream to file (JSONL format)
        #[arg(long)]
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
    /// Traffic Monitoring (Online): live WebSocket stream, or search via REST API
    Flows {
        /// Control API URL (WebSocket stream when not searching)
        #[arg(long, default_value = "http://127.0.0.1:8081")]
        control_url: String,

        /// REST API base URL for search mode (requires `relay run --api-port`)
        #[arg(long, default_value = "http://127.0.0.1:8082")]
        api_url: String,

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

        #[arg(long)]
        websocket: bool,

        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Interception Control (Online)
    Intercept {
        #[command(subcommand)]
        action: InterceptAction,

        /// Control API URL
        #[arg(long, default_value = "http://127.0.0.1:8081")]
        control_url: String,
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
pub enum InterceptAction {
    Pause,
    Resume,
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
    /// Initialize CA (generate if not exists)
    Init {
        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        cert: PathBuf,

        /// Path to CA key
        #[arg(long, default_value = "ca_key.pem")]
        key: PathBuf,

        /// Force regenerate even if exists
        #[arg(long)]
        force: bool,
    },
    /// Export CA certificate
    Export {
        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        cert: PathBuf,

        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Install CA certificate to system trust store
    Install {
        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        cert: PathBuf,
    },
    /// Uninstall CA certificate from system trust store
    Uninstall {
        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        cert: PathBuf,
    },
    /// Check CA certificate status
    Status {
        /// Path to CA certificate
        #[arg(long, default_value = "ca_cert.pem")]
        cert: PathBuf,
    },
}
