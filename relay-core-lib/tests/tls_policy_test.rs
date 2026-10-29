//! Wire-level contracts for the TLS policy: per-host passthrough/drop, and the handshake metadata
//! (SNI, ALPN, negotiated version) recorded on the flows inside a MITM tunnel.
//!
//! These live outside `wire_matrix` because they need their own TLS upstream and a client verifier
//! that records the certificate it was shown — the assertion "the client saw the upstream
//! certificate, not ours" cannot be expressed without it.

use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use rcgen::generate_simple_self_signed;
use relay_core_api::flow::{FlowUpdate, Layer};
use relay_core_api::policy::{ProxyPolicy, TlsPolicy};
use relay_core_lib::capture::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::http_utils::HttpsClient;
use relay_core_lib::proxy::server::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use rustls::ClientConfig;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

struct TlsUpstream {
    addr: SocketAddr,
    cert_der: Vec<u8>,
    served: Arc<std::sync::atomic::AtomicUsize>,
}

/// Start a TLS HTTP/1.1 upstream that answers one request per connection.
///
/// ALPN advertises only `http/1.1`, so a client that offers `h2` too still negotiates a protocol
/// the test can assert on.
async fn spawn_tls_upstream(body: &'static str) -> TlsUpstream {
    init_crypto();

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("generate a self-signed certificate");
    let cert_der = cert.cert.der().to_vec();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key)
        .expect("server config");
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind TLS upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let served = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let served_for_task = served.clone();

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let served = served_for_task.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                // One read is enough for a request head that fits; these tests send small requests.
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    TlsUpstream {
        addr,
        cert_der,
        served,
    }
}

/// A verifier that accepts every certificate and keeps the leaf it was shown.
///
/// Recording the leaf is what makes "somebody else terminated TLS" observable: the certificate the
/// client sees inside a MITM tunnel is signed by this engine's CA, so its bytes differ from the
/// upstream's. A trust-chain assertion would need the CA to be installed, which tests the harness
/// rather than the engine.
#[derive(Debug)]
struct LeafRecorder(Arc<Mutex<Vec<u8>>>);

impl rustls::client::danger::ServerCertVerifier for LeafRecorder {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().expect("leaf lock") = end_entity.as_ref().to_vec();
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

fn recording_client_config(leaf: Arc<Mutex<Vec<u8>>>) -> ClientConfig {
    let mut config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(LeafRecorder(leaf)));
    config
}

fn insecure_upstream_client() -> HttpsClient {
    #[derive(Debug)]
    struct NoVerifier;
    impl rustls::client::danger::ServerCertVerifier for NoVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PKCS1_SHA1,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    let mut config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(NoVerifier));

    let https = HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    let client: HttpsClient = Client::builder(hyper_util::rt::TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https);
    client
}

/// Start the engine with `policy` and return its port plus the flow-update receiver.
async fn start_test_proxy(policy: ProxyPolicy) -> (u16, tokio::sync::mpsc::Receiver<FlowUpdate>) {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(256);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(policy);

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
            ca,
            policy_rx,
            Some(Arc::new(insecure_upstream_client())),
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    (proxy_port, flow_rx)
}

/// Send CONNECT and consume exactly the response head, leaving the stream positioned at the tunnel.
async fn open_connect_tunnel(proxy_port: u16, authority: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    stream
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .expect("write CONNECT");

    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
            .await
            .expect("CONNECT must be answered")
            .expect("read CONNECT response");
        assert_eq!(n, 1, "the tunnel closed before the CONNECT response ended");
        response.push(byte[0]);
    }
    (stream, String::from_utf8_lossy(&response).to_string())
}

async fn send_get_over_tls(
    proxy_port: u16,
    authority: &str,
    servername: &str,
    leaf: Arc<Mutex<Vec<u8>>>,
) -> (Vec<u8>, String) {
    let (stream, connect_response) = open_connect_tunnel(proxy_port, authority).await;
    assert!(
        connect_response.starts_with("HTTP/1.1 200"),
        "the tunnel must open, got {connect_response:?}"
    );

    let connector = TlsConnector::from(Arc::new(recording_client_config(leaf)));
    let name =
        rustls::pki_types::ServerName::try_from(servername.to_string()).expect("server name");
    let mut tls = connector
        .connect(name, stream)
        .await
        .expect("TLS handshake with the proxy");

    tls.write_all(
        format!(
            "GET /tls-policy-probe HTTP/1.1\r\nHost: {servername}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .expect("write request");

    let mut response = Vec::new();
    tls.read_to_end(&mut response).await.expect("read response");
    (response, connect_response)
}

/// Drain the updates that have been published so far, without waiting for more.
fn drain_flows(rx: &mut tokio::sync::mpsc::Receiver<FlowUpdate>) -> Vec<FlowUpdate> {
    let mut updates = Vec::new();
    while let Ok(update) = rx.try_recv() {
        updates.push(update);
    }
    updates
}

#[tokio::test]
async fn tls_passthrough_shows_the_client_the_upstream_certificate() {
    let upstream = spawn_tls_upstream("PASSTHROUGH").await;
    let authority = format!("localhost:{}", upstream.addr.port());

    let policy = ProxyPolicy {
        tls: TlsPolicy {
            passthrough_hosts: vec![authority.clone()],
            drop_hosts: Vec::new(),
        },
        ..ProxyPolicy::default()
    };

    let (proxy_port, mut flows) = start_test_proxy(policy).await;
    let leaf = Arc::new(Mutex::new(Vec::new()));

    let (response, _) = send_get_over_tls(proxy_port, &authority, "localhost", leaf.clone()).await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("PASSTHROUGH"),
        "the request must reach the upstream inside a passthrough tunnel, got {text:?}"
    );

    let seen = leaf.lock().expect("leaf lock").clone();
    assert_eq!(
        seen, upstream.cert_der,
        "the client must have been shown the upstream certificate, not one signed by our CA"
    );
    assert_eq!(
        upstream.served.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the upstream must have served the request"
    );

    // The tunnel is not decrypted, so no HTTP flow for the probe may exist. The CONNECT itself must
    // still be recorded and tagged, so the capture can answer "this was not inspected".
    tokio::time::sleep(Duration::from_millis(300)).await;
    let updates = drain_flows(&mut flows);
    let mut saw_tagged_connect = false;
    for update in &updates {
        if let FlowUpdate::Full(flow) = update {
            if flow.tags.iter().any(|tag| tag == "tls-passthrough") {
                saw_tagged_connect = true;
            }
            if let Layer::Http(http) = &flow.layer {
                assert!(
                    !http.request.url.as_str().contains("/tls-policy-probe"),
                    "a passthrough tunnel must not produce a decrypted HTTP flow"
                );
            }
        }
    }
    assert!(
        saw_tagged_connect,
        "the CONNECT must be recorded and tagged as passthrough"
    );
}

#[tokio::test]
async fn tls_is_intercepted_by_default() {
    let upstream = spawn_tls_upstream("MITM").await;
    let authority = format!("localhost:{}", upstream.addr.port());

    let (proxy_port, mut flows) = start_test_proxy(ProxyPolicy::default()).await;
    let leaf = Arc::new(Mutex::new(Vec::new()));

    let (response, _) = send_get_over_tls(proxy_port, &authority, "localhost", leaf.clone()).await;
    assert!(
        String::from_utf8_lossy(&response).contains("MITM"),
        "an intercepted tunnel must still forward the exchange"
    );

    let seen = leaf.lock().expect("leaf lock").clone();
    assert_ne!(
        seen, upstream.cert_der,
        "the default policy must terminate TLS, so the client must not see the upstream certificate"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let updates = drain_flows(&mut flows);
    let http_flow = updates
        .iter()
        .find_map(|update| match update {
            FlowUpdate::Full(flow) => match &flow.layer {
                Layer::Http(http) if http.request.url.as_str().contains("/tls-policy-probe") => {
                    Some(flow)
                }
                _ => None,
            },
            _ => None,
        })
        .expect("an intercepted exchange must be recorded as an HTTP flow");

    assert_eq!(
        http_flow.network.sni.as_deref(),
        Some("localhost"),
        "the SNI the client sent must be recorded"
    );
    assert_eq!(
        http_flow.network.alpn.as_deref(),
        Some("http/1.1"),
        "the negotiated ALPN protocol must be recorded"
    );
    assert!(
        http_flow
            .network
            .tls_version
            .as_deref()
            .is_some_and(|version| version.starts_with("TLSv1.")),
        "the negotiated TLS version must be recorded, got {:?}",
        http_flow.network.tls_version
    );
    assert!(
        http_flow.network.tls,
        "the flow must state that TLS was terminated"
    );
}

#[tokio::test]
async fn tls_drop_hosts_refuse_the_connect_and_record_why() {
    let upstream = spawn_tls_upstream("SHOULD-NOT-BE-REACHED").await;
    let authority = format!("localhost:{}", upstream.addr.port());

    let policy = ProxyPolicy {
        tls: TlsPolicy {
            passthrough_hosts: Vec::new(),
            drop_hosts: vec![authority.clone()],
        },
        ..ProxyPolicy::default()
    };

    let (proxy_port, mut flows) = start_test_proxy(policy).await;
    let (stream, response) = open_connect_tunnel(proxy_port, &authority).await;
    drop(stream);

    assert!(
        response.starts_with("HTTP/1.1 403"),
        "a dropped target must be refused at CONNECT time, got {response:?}"
    );
    assert_eq!(
        upstream.served.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a dropped target must never be contacted"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    let updates = drain_flows(&mut flows);
    let blocked = updates
        .iter()
        .find_map(|update| match update {
            FlowUpdate::Full(flow) if flow.tags.iter().any(|tag| tag == "tls-blocked") => {
                Some(flow)
            }
            _ => None,
        })
        .expect("the refused CONNECT must be recorded");

    assert_eq!(
        blocked.close_reason,
        Some(relay_core_api::event::CloseReason::PolicyDrop {
            detail: "TLS interception refused by policy".to_string()
        })
    );
    assert!(
        blocked.end_time.is_some(),
        "a refused CONNECT must still carry an end time"
    );
}
