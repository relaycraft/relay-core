use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc::Sender, watch};
use tracing::{error, info};

use crate::capture::CaptureSource;
use crate::capture::loop_detection::LoopDetector;
use crate::interceptor::{ENGINE_INDEX, Interceptor};
use crate::proxy::http::handle_request;
use crate::proxy::http_utils::HttpsClient;
use crate::tls::CertificateAuthority;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;

static CONN_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Start the HTTP Proxy Server
pub async fn start_proxy<S>(
    mut source: S,
    on_flow: Sender<FlowUpdate>,
    interceptor: Arc<dyn Interceptor>,
    ca: Arc<CertificateAuthority>,
    policy: watch::Receiver<ProxyPolicy>,
    client: Option<Arc<HttpsClient>>,
    shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>,
) -> crate::error::Result<()>
where
    S: CaptureSource + Send + 'static,
    S::IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    info!("RelayCore Proxy starting...");
    info!("CA Loaded. Root cert:\n{}", ca.get_ca_cert_pem());
    info!("Proxy Policy: {:?}", policy.borrow());

    // Initialize HTTP Client (shared)
    let client = if let Some(c) = client {
        c
    } else {
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        let client: HttpsClient = Client::builder(hyper_util::rt::TokioExecutor::new())
            .timer(hyper_util::rt::TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(32)
            .http2_initial_stream_window_size(2 * 1024 * 1024) // 2MB
            .http2_initial_connection_window_size(4 * 1024 * 1024) // 4MB
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build(https);
        Arc::new(client)
    };

    // Initialize Loop Detector
    let listen_addrs = source.listen_addrs().into_iter().collect();
    let loop_detector = Arc::new(LoopDetector::new(listen_addrs));
    {
        let loop_detector_bg = loop_detector.clone();
        tokio::spawn(async move {
            // Prime local interface cache at startup.
            loop_detector_bg.refresh_local_addrs().await;
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                loop_detector_bg.refresh_local_addrs().await;
            }
        });
    }

    let mut shutdown_rx = shutdown_rx;

    loop {
        // Accept connection from abstract source or handle shutdown
        let connection_result = tokio::select! {
            res = source.accept() => res,
            _ = async {
                if let Some(rx) = shutdown_rx.as_mut() {
                    rx.await.ok();
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                info!("RelayCore Proxy received shutdown signal");
                break;
            }
        };

        let connection = match connection_result {
            Ok(conn) => conn,
            Err(e) => {
                error!("Error accepting connection: {}", e);
                continue;
            }
        };

        let stream = connection.stream;
        let client_addr = connection.client_addr;
        let target_addr = connection.target_addr;

        let io = TokioIo::new(stream);
        let on_flow = on_flow.clone();
        let ca = ca.clone();
        let client = client.clone();
        let interceptor = interceptor.clone();
        let policy = policy.clone();
        let loop_detector = loop_detector.clone();

        let engine_index = CONN_COUNTER.fetch_add(1, Ordering::Relaxed);

        tokio::task::spawn(ENGINE_INDEX.scope(engine_index, async move {
            if let Err(err) = http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10))
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        handle_request(
                            req,
                            client_addr,
                            on_flow.clone(),
                            ca.clone(),
                            client.clone(),
                            interceptor.clone(),
                            target_addr,
                            policy.clone(),
                            loop_detector.clone(),
                        )
                    }),
                )
                .with_upgrades()
                .await
            {
                error!("Error serving connection: {:?}", err);
            }
        }));
    }

    Ok(())
}
