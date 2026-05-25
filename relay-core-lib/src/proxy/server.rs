use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::Sender, watch};
use tracing::{error, info};
use uuid::Uuid;

use crate::capture::CaptureSource;
use crate::capture::loop_detection::LoopDetector;
use crate::interceptor::{ConnectAction, ConnectionInfo, ENGINE_INDEX, Interceptor};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::http::handle_request;
use crate::proxy::http_utils::HttpsClient;
use crate::rule::engine::executor::{ConnectOverride, RuleEngine};
use crate::tls::CertificateAuthority;
use chrono::Utc;
use relay_core_api::flow::{Flow, FlowUpdate, Layer, NetworkInfo, TcpLayer, TransportProtocol};
use relay_core_api::policy::ProxyPolicy;
use relay_core_api::rule::RuleStage;
use std::collections::HashMap;
use std::net::SocketAddr;

static CONN_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Start the HTTP Proxy Server
#[allow(clippy::too_many_arguments)]
pub async fn start_proxy<S>(
    mut source: S,
    on_flow: Sender<FlowUpdate>,
    interceptor: Arc<dyn Interceptor>,
    ca: Arc<CertificateAuthority>,
    policy: watch::Receiver<ProxyPolicy>,
    client: Option<Arc<HttpsClient>>,
    shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    rule_engine: Option<Arc<RuleEngine>>,
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

    // Initialize Circuit Breaker (P3)
    let circuit_breaker = Arc::new(CircuitBreaker::default());

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

        let conn_id = Uuid::new_v4();
        let conn_info = ConnectionInfo {
            id: conn_id,
            client_addr,
            server_addr: target_addr,
            // TODO: extract SNI from TLS ClientHello (requires pre-rustls intercept)
            tls_sni: None,
        };

        match interceptor.on_connect(&conn_info).await {
            ConnectAction::Drop { reason } => {
                info!("Connection {} dropped by onConnect: {}", conn_id, reason);
                interceptor
                    .on_disconnect(&conn_info, &Default::default())
                    .await;
                continue;
            }
            ConnectAction::Allow => {}
        }

        let mut connect_target = target_addr;
        if let Some(ref engine) = rule_engine
            && engine.has_rules_for_stage(RuleStage::Connect)
        {
            let mut flow = Flow {
                id: conn_id,
                start_time: Utc::now(),
                end_time: None,
                network: NetworkInfo {
                    client_ip: client_addr.ip().to_string(),
                    client_port: client_addr.port(),
                    server_ip: target_addr.map(|a| a.ip().to_string()).unwrap_or_default(),
                    server_port: target_addr.map(|a| a.port()).unwrap_or(0),
                    protocol: TransportProtocol::TCP,
                    tls: false,
                    tls_version: None,
                    sni: None,
                },
                layer: Layer::Tcp(TcpLayer {
                    bytes_up: 0,
                    bytes_down: 0,
                }),
                tags: vec![],
                meta: HashMap::new(),
                resilience_trace: None,
                rule_variables: HashMap::new(),
                matched_rules: vec![],
            };
            let ctx = engine.execute(RuleStage::Connect, &mut flow).await;
            if ctx.is_terminated() {
                info!("Connection {} terminated by Connect stage rule", conn_id);
                interceptor
                    .on_disconnect(&conn_info, &Default::default())
                    .await;
                continue;
            }
            if let Some(conn_override) = &ctx.connect_override {
                match conn_override {
                    ConnectOverride::ForwardPort { host: _, port } => {
                        // ForwardPort preserves the original destination IP and only
                        // changes the port. The host field is retained for future
                        // host-redirect support (1.x).
                        connect_target = Some(SocketAddr::new(
                            target_addr
                                .unwrap_or_else(|| "0.0.0.0:0".parse().unwrap())
                                .ip(),
                            *port,
                        ));
                        tracing::debug!("Connect stage ForwardPort -> port {}", port);
                    }
                    ConnectOverride::RedirectIp { ip } => {
                        let port = connect_target.map(|a| a.port()).unwrap_or(0);
                        connect_target = Some(SocketAddr::new(*ip, port));
                        tracing::debug!("Connect stage RedirectIp -> {}", ip);
                    }
                    ConnectOverride::SetTtl { ttl } => {
                        // Socket-level TTL application requires access to the raw
                        // TcpStream before it enters the hyper HTTP stack, which is
                        // not feasible with the current connection model.
                        // Deferred to 1.x.
                        tracing::warn!(
                            "Connect stage SetTtl({}) is not yet implemented; TTL unchanged",
                            ttl
                        );
                    }
                }
            }
        }

        let target_addr = connect_target;

        let io = TokioIo::new(stream);
        let on_flow = on_flow.clone();
        let ca = ca.clone();
        let client = client.clone();
        let interceptor = interceptor.clone();
        let policy = policy.clone();
        let loop_detector = loop_detector.clone();

        let circuit_breaker = circuit_breaker.clone();

        let engine_index = CONN_COUNTER.fetch_add(1, Ordering::Relaxed);

        let conn_info_2 = conn_info.clone();
        let interceptor_2 = interceptor.clone();
        tokio::task::spawn(ENGINE_INDEX.scope(engine_index, async move {
            let conn_start = Instant::now();
            let result = http1::Builder::new()
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
                            circuit_breaker.clone(),
                        )
                    }),
                )
                .with_upgrades()
                .await;

            let stats = crate::interceptor::ConnectionStats {
                duration_ms: conn_start.elapsed().as_millis() as u64,
                // TODO: populate bytes_sent, bytes_received, flows_count when real-time stats tracking is added
                ..Default::default()
            };
            interceptor_2.on_disconnect(&conn_info_2, &stats).await;

            if let Err(err) = result {
                error!("Error serving connection: {:?}", err);
            }
        }));
    }

    Ok(())
}
