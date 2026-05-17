use relay_core_probe::{ProbeServer, ProbeConfig, ProbeTransport};
use relay_core_runtime::CoreState;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let state = Arc::new(CoreState::new(None).await);
    let config = ProbeConfig { transport: ProbeTransport::Stdio };
    if let Err(e) = ProbeServer::new(config, state).run().await {
        eprintln!("Probe server error: {}", e);
        std::process::exit(1);
    }
}
