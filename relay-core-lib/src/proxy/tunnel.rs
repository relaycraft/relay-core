use std::sync::Arc;
use std::time::Duration;
use std::net::SocketAddr;
use tokio::sync::{mpsc::Sender, watch};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto;
use hyper::service::service_fn;
use tokio_rustls::TlsAcceptor;
use url::Url;
use tracing::{info, error};
use crate::tls::CertificateAuthority;
use crate::interceptor::Interceptor;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use crate::proxy::http::handle_http_request;
use crate::proxy::http_utils::HttpsClient;

use crate::capture::loop_detection::LoopDetector;

#[allow(clippy::too_many_arguments)]
pub async fn handle_tunnel(
    upgraded: Upgraded,
    host: String,
    client_addr: SocketAddr,
    ca: Arc<CertificateAuthority>,
    on_flow: Sender<FlowUpdate>,
    client: Arc<HttpsClient>,
    interceptor: Arc<dyn Interceptor>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
) -> crate::error::Result<()>
{
    info!("Starting MITM tunnel for {}", host);

    // 1. Get cached server config for `host` (generates on miss)
    // Strip port if present for certificate generation (SNI / CN)
    let hostname = host.split(':').next().unwrap_or(&host);
    let server_config = ca.gen_server_config(hostname).await?;
    
    // 2. Configure Server TLS
    // ALPN protocols (h2, http/1.1) are already configured in gen_server_config
    let tls_acceptor = TlsAcceptor::from(server_config);

    // 3. Accept TLS connection from client
    let io = TokioIo::new(upgraded);
    let tls_stream = match tokio::time::timeout(Duration::from_secs(10), tls_acceptor.accept(io)).await {
        Ok(res) => res?,
        Err(_) => return Err(crate::error::RelayError::Proxy("TLS handshake timeout".into())),
    };
    let tls_io = TokioIo::new(tls_stream);

    // 4. Serve the decrypted stream as HTTP
    // Note: We use the passed client_addr
    
    let policy_rx = policy_rx.clone();
    let loop_detector = loop_detector.clone();
    
    let mut builder = auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder.http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    builder.http2()
        .timer(hyper_util::rt::TokioTimer::new())
        .initial_stream_window_size(2 * 1024 * 1024) // 2MB
        .initial_connection_window_size(4 * 1024 * 1024) // 4MB
        .max_concurrent_streams(200)
        .max_header_list_size(65536) // 64KB
        .keep_alive_interval(std::time::Duration::from_secs(20))
        .keep_alive_timeout(std::time::Duration::from_secs(10));

    if let Err(err) = builder
        .serve_connection(tls_io, service_fn(move |req| {
            // Inside the tunnel, requests are relative (e.g., GET /).
            // We need to construct absolute URLs for the client using `host` and scheme `https`.
            let mut req = req;
            let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
            let uri_string = format!("https://{}{}", host, path);
            if let Ok(new_uri) = Url::parse(&uri_string)
                && let Ok(uri) = new_uri.as_str().parse() {
                    *req.uri_mut() = uri;
                }

            handle_http_request(
                req, 
                client_addr, 
                on_flow.clone(), 
                client.clone(), 
                interceptor.clone(), 
                true, 
                policy_rx.clone(),
                target_addr,
                loop_detector.clone()
            )
        }))
        .await
    {
        error!("MITM Tunnel Error: {:?}", err);
    }

    Ok(())
}
