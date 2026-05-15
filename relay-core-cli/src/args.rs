use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about, long_about = None)]
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
    /// Traffic Monitoring (Online)
    Flows {
        /// Control API URL
        #[arg(long, default_value = "http://127.0.0.1:8081")]
        control_url: String,

        /// Output format (table, json, jsonl)
        #[arg(long, default_value = "table")]
        output: String,
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
    }
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
    }
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
    }
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
    }
}
