use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::tls::CertificateAuthority;
use relay_core_lib::{engine::TcpCaptureSource, start_proxy};
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Example: Loading a custom CA from PEM files (or creating it if missing)
///
/// Usage:
///   cargo run --example custom_ca
///
/// This will:
/// 1. Look for `custom_ca.crt` and `custom_ca.key` in the current directory.
/// 2. If found, load them (even if `custom_ca.json` metadata is missing).
/// 3. If not found, generate a new CA and save it to disk.
/// 4. Start a proxy on 127.0.0.1:8080 using this CA.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Install crypto provider (required for rustls 0.23+)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;
    println!("Proxy listening on {}", addr);

    let source = TcpCaptureSource::new(listener);

    // No-op flow handler (just prints nothing)
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    tokio::spawn(async move { while let Some(_) = rx.recv().await {} });

    // No-op interceptor (no rules)
    let interceptor = Arc::new(NoOpInterceptor);

    // Load custom CA from files
    let ca_cert_path = Path::new("custom_ca.crt");
    let ca_key_path = Path::new("custom_ca.key");

    println!("Loading CA from {:?}", ca_cert_path);

    // This will try to load from files, or create new ones if missing.
    // If ca.json is missing, it will parse PEM files directly (fallback mode).
    let ca = Arc::new(CertificateAuthority::load_or_create(
        ca_cert_path,
        ca_key_path,
    )?);

    println!("CA Loaded Successfully!");
    println!("CA Subject: RelayCraft CA (Self-signed)");
    println!("CA Certificate PEM:\n{}", ca.get_ca_cert_pem());

    let (_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    println!("Starting proxy...");
    start_proxy(source, tx, interceptor, ca, policy_rx, None, None, None).await?;

    Ok(())
}
