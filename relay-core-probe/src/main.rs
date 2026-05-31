use relay_core_probe::{ProbeConfig, ProbeServer, ProbeTransport};
use relay_core_runtime::{CaPaths, CoreState, ProxyConfig};
use std::net::IpAddr;
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

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let state = Arc::new(CoreState::new(None).await);

    let port = parse_arg_env("--port=", "RELAY_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let ca_paths = match CaPaths::resolve(
        parse_arg_env("--ca-cert=", "RELAY_CA_CERT").map(PathBuf::from),
        parse_arg_env("--ca-key=", "RELAY_CA_KEY").map(PathBuf::from),
    ) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("Invalid CA path configuration: {e}");
            std::process::exit(1);
        }
    };
    if !ca_paths.cert.exists() || !ca_paths.key.exists() {
        eprintln!(
            "CA files not found:\n  cert: {}\n  key: {}\nRun `relay-core-cli ca generate` first.",
            ca_paths.cert.display(),
            ca_paths.key.display()
        );
        std::process::exit(1);
    }

    let config = ProxyConfig::new(port, ca_paths.cert.clone(), ca_paths.key.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(1000);
    match state.spawn_proxy(config, tx, None) {
        Ok(_) => {}
        Err(e) => eprintln!("Proxy start warning: {}", e),
    }

    // Print startup guidance
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("RelayCore MCP proxy started");
    eprintln!("  Proxy:  127.0.0.1:{}", port);
    eprintln!("  CA:     {}", ca_paths.cert.display());
    eprintln!();
    if !cfg!(target_os = "macos") || ca_paths.cert.exists() {
        eprintln!("Configure your browser/system to use this proxy.");
    }
    if cfg!(target_os = "macos") {
        eprintln!("HTTPS interception requires CA trust:");
        eprintln!("  npx @relay-core/cli ca install");
    } else {
        eprintln!("HTTPS interception: install ca_cert.pem to your system trust store.");
    }
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // ——— Transport selection ———
    let transport = parse_transport();

    let probe_config = ProbeConfig { transport };
    if let Err(e) = ProbeServer::new(probe_config, state).run().await {
        eprintln!("Probe server error: {}", e);
        std::process::exit(1);
    }
}

fn parse_transport() -> ProbeTransport {
    let transport_arg = parse_arg_env("--transport=", "RELAY_PROBE_TRANSPORT");

    match transport_arg.as_deref() {
        Some("sse") => {
            let sse_port = parse_arg_env("--probe-port=", "RELAY_PROBE_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000);
            let sse_bind = parse_arg_env("--probe-bind=", "RELAY_PROBE_BIND")
                .and_then(|b| b.parse::<IpAddr>().ok())
                .unwrap_or(IpAddr::from([127, 0, 0, 1]));
            ProbeTransport::Sse {
                port: sse_port,
                bind: sse_bind,
            }
        }
        _ => ProbeTransport::Stdio,
    }
}
