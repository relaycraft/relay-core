use relay_core_lib::tls::CertificateAuthority;
use relay_core_probe::{ProbeConfig, ProbeServer, ProbeTransport};
use relay_core_runtime::{CoreState, ProxyConfig};
use std::path::PathBuf;
use std::sync::Arc;

fn parse_arg(prefix: &str) -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix(prefix).map(|v| v.to_string()))
}

fn parse_arg_env(arg_prefix: &str, env_key: &str) -> Option<String> {
    parse_arg(arg_prefix).or_else(|| std::env::var(env_key).ok())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = Arc::new(CoreState::new(None).await);

    let port = parse_arg_env("--port=", "RELAY_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let ca_cert_path = parse_arg_env("--ca-cert=", "RELAY_CA_CERT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_next_or_cwd().join("ca_cert.pem"));
    let ca_key_path = parse_arg_env("--ca-key=", "RELAY_CA_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_next_or_cwd().join("ca_key.pem"));

    // Auto-init CA if not exists
    if (!ca_cert_path.exists() || !ca_key_path.exists())
        && let Err(e) = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path)
    {
        eprintln!("Failed to init CA: {}", e);
    }

    let config = ProxyConfig::new(port, ca_cert_path.clone(), ca_key_path.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(1000);
    match state.spawn_proxy(config, tx, None) {
        Ok(_) => {}
        Err(e) => eprintln!("Proxy start warning: {}", e),
    }

    // Print startup guidance
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("RelayCore MCP proxy started");
    eprintln!("  Proxy:  127.0.0.1:{}", port);
    eprintln!("  CA:     {}", ca_cert_path.display());
    eprintln!();
    if !cfg!(target_os = "macos") || ca_cert_path.exists() {
        eprintln!("Configure your browser/system to use this proxy.");
    }
    if cfg!(target_os = "macos") {
        eprintln!("HTTPS interception requires CA trust:");
        eprintln!("  npx @relay-core/cli ca install");
    } else {
        eprintln!("HTTPS interception: install ca_cert.pem to your system trust store.");
    }
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // ——— Serve MCP ———
    let probe_config = ProbeConfig {
        transport: ProbeTransport::Stdio,
    };
    if let Err(e) = ProbeServer::new(probe_config, state).run().await {
        eprintln!("Probe server error: {}", e);
        std::process::exit(1);
    }
}

fn dirs_next_or_cwd() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
