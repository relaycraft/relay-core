use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::tls::CertificateAuthority;
use relay_core_lib::{engine::TcpCaptureSource, start_proxy};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(addr).await?;
    let source = TcpCaptureSource::new(listener);
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);

    // Drain channel
    tokio::spawn(async move { while let Some(_) = rx.recv().await {} });

    let interceptor = Arc::new(NoOpInterceptor);
    let ca = Arc::new(CertificateAuthority::new()?);
    let (_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    start_proxy(source, tx, interceptor, ca, policy_rx, None, None, None).await?;
    Ok(())
}
