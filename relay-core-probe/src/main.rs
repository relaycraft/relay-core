use relay_core_probe::{ProbeServer, ProbeConfig, ProbeTransport};
use relay_core_runtime::{CoreState, ProxyConfig};
use relay_core_lib::tls::CertificateAuthority;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = Arc::new(CoreState::new(None).await);

    // ——— Parse args ———
    let port = std::env::args()
        .nth(1)
        .and_then(|a| a.strip_prefix("--port=").map(|p| p.to_string()))
        .or_else(|| std::env::var("RELAY_PORT").ok())
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let ca_dir = dirs_next().unwrap_or_else(|| std::env::current_dir().unwrap());
    let ca_cert_path = ca_dir.join("ca_cert.pem");
    let ca_key_path = ca_dir.join("ca_key.pem");

    // Auto-init CA if not exists
    if !ca_cert_path.exists() || !ca_key_path.exists() {
        if let Err(e) = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path) {
            eprintln!("Failed to init CA: {}", e);
        }
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
    eprintln!("");
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
    let probe_config = ProbeConfig { transport: ProbeTransport::Stdio };
    if let Err(e) = ProbeServer::new(probe_config, state).run().await {
        eprintln!("Probe server error: {}", e);
        std::process::exit(1);
    }
}

fn dirs_next() -> Option<std::path::PathBuf> {
    dirs::data_local_dir().or_else(|| dirs::home_dir())
}
