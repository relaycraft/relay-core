use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioIo;
use rcgen::generate_simple_self_signed;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::capture::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::http_utils::HttpsClient;
use relay_core_lib::proxy::server::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use rustls::ClientConfig;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

// Helper to create a self-signed server cert
fn make_server_cert() -> (
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let cert = generate_simple_self_signed(subject_alt_names).unwrap();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    (vec![cert_der], key)
}

// Helper to create a client that trusts everything (for testing)
fn make_insecure_client_config() -> ClientConfig {
    let root_store = rustls::RootCertStore::empty();
    // We don't actually add roots, we just disable verification
    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    // Dangerous: disable verification
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

    config
        .dangerous()
        .set_certificate_verifier(Arc::new(NoVerifier));
    // config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()]; // Let caller or hyper-rustls set this
    config
}

#[tokio::test]
async fn test_https_mitm_h1() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Start Mock Upstream HTTPS Server
    let (certs, key) = make_server_cert();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));

    let echo_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let echo_listener = TcpListener::bind(echo_addr)
        .await
        .expect("Failed to bind echo server");
    let echo_port = echo_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = echo_listener.accept().await {
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(socket).await {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut stream = stream;
                        let mut buf = [0; 1024];
                        if let Ok(n) = stream.read(&mut buf).await {
                            let request = String::from_utf8_lossy(&buf[..n]);
                            if request.contains("GET / HTTP/1.1") {
                                let response = "HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nHTTPS Secured";
                                let _ = stream.write_all(response.as_bytes()).await;
                            }
                        }
                    }
                });
            }
        }
    });

    // 2. Start Proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr)
        .await
        .expect("Failed to bind proxy");
    let proxy_port = listener.local_addr().unwrap().port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(100);
    let on_flow = tx.clone();

    // Inject insecure client
    let insecure_config = make_insecure_client_config();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(insecure_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    let client: HttpsClient = Client::builder(hyper_util::rt::TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https);
    let client = Some(Arc::new(client));

    tokio::spawn(async move {
        let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
        start_proxy(
            source,
            on_flow,
            interceptor,
            ca,
            policy_rx,
            client,
            None,
            None,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3. Client Connects to Proxy (CONNECT)
    let mut proxy_stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Send CONNECT
    let connect_req = format!(
        "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        echo_port, echo_port
    );
    proxy_stream
        .write_all(connect_req.as_bytes())
        .await
        .unwrap();

    let mut buf = [0; 1024];
    let n = proxy_stream.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(response.contains("200 OK"), "CONNECT failed: {}", response);

    // 4. Client Upgrade to TLS (over the tunnel)
    let client_config = make_insecure_client_config();
    let connector = TlsConnector::from(Arc::new(client_config));
    let domain = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();

    let mut tls_stream = connector
        .connect(domain, proxy_stream)
        .await
        .expect("TLS handshake with MITM failed");

    // 5. Send HTTPS Request
    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        echo_port
    );
    tls_stream.write_all(req.as_bytes()).await.unwrap();

    let mut resp_buf = [0; 1024];
    let n = tls_stream.read(&mut resp_buf).await.unwrap();
    let resp_str = String::from_utf8_lossy(&resp_buf[..n]);
    println!("H1 Test Response: {}", resp_str);

    assert!(
        resp_str.contains("HTTPS Secured"),
        "Response was: {}",
        resp_str
    );

    // 6. Verify Interception Flow
    // We expect flows for the decrypted traffic
    loop {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let relay_core_api::flow::Layer::Http(http) = flow.layer
                    && http.request.url.as_str().contains("https") {
                        // Found our HTTPS flow
                        break;
                    }
            }
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timeout waiting for HTTPS flow update"),
            Ok(Some(_)) => continue,
        }
    }
}

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper::{Request, Response};
use std::convert::Infallible;

#[tokio::test]
async fn test_https_mitm_h2() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Start Mock Upstream HTTPS/2 Server
    // Create specific H2 config
    let (certs, key) = make_server_cert();
    let mut sc = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    sc.alpn_protocols = vec![b"h2".to_vec()];
    let server_config = Arc::new(sc);

    let tls_acceptor = TlsAcceptor::from(server_config);

    let h2_echo_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let h2_echo_listener = TcpListener::bind(h2_echo_addr)
        .await
        .expect("Failed to bind h2 echo server");
    let h2_echo_port = h2_echo_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = h2_echo_listener.accept().await {
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(socket).await {
                        let service =
                            service_fn(|_req: Request<hyper::body::Incoming>| async move {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "H2 Secured",
                                ))))
                            });

                        if let Err(e) = hyper::server::conn::http2::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        )
                        .timer(hyper_util::rt::TokioTimer::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                        {
                            eprintln!("H2 server error: {:?}", e);
                        }
                    }
                });
            }
        }
    });

    // 2. Start Proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr)
        .await
        .expect("Failed to bind proxy");
    let proxy_port = listener.local_addr().unwrap().port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(100);
    let on_flow = tx.clone();

    // Inject insecure client
    let insecure_config = make_insecure_client_config();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(insecure_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    let client: HttpsClient = Client::builder(hyper_util::rt::TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https);
    let client = Some(Arc::new(client));

    tokio::spawn(async move {
        let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
        start_proxy(
            source,
            on_flow,
            interceptor,
            ca,
            policy_rx,
            client,
            None,
            None,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3. Client Connects to Proxy (CONNECT)
    let mut proxy_stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Send CONNECT
    let connect_req = format!(
        "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        h2_echo_port, h2_echo_port
    );
    proxy_stream
        .write_all(connect_req.as_bytes())
        .await
        .unwrap();

    let mut buf = [0; 1024];
    let n = proxy_stream.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(response.contains("200 OK"), "CONNECT failed: {}", response);

    // 4. Client Upgrade to TLS (ALPN=h2)
    let mut client_config = make_insecure_client_config();
    client_config.alpn_protocols = vec![b"h2".to_vec()]; // Force H2 on client
    let connector = TlsConnector::from(Arc::new(client_config));
    let domain = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();

    let tls_stream = connector
        .connect(domain, proxy_stream)
        .await
        .expect("TLS handshake failed");

    // Verify ALPN is actually H2
    let (_, session) = tls_stream.get_ref();
    assert_eq!(session.alpn_protocol(), Some(b"h2".as_slice()));

    let tls_io = TokioIo::new(tls_stream);

    // 5. Send H2 Request via Hyper Client
    let (mut sender, conn) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .timer(hyper_util::rt::TokioTimer::new())
            .handshake(tls_io)
            .await
            .expect("H2 handshake failed");

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("H2 connection failed: {:?}", e);
        }
    });

    let req = Request::builder()
        .uri(format!("https://127.0.0.1:{}/", h2_echo_port))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let res = sender.send_request(req).await.expect("H2 request failed");
    assert!(res.status().is_success());

    let body = res.collect().await.unwrap().to_bytes();
    assert_eq!(body, "H2 Secured");

    // 6. Verify Interception Flow
    loop {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let relay_core_api::flow::Layer::Http(http) = flow.layer {
                    // Check if version is HTTP/2
                    // Note: relay-core might normalize to HTTP/1.1 in Flow struct if it converts,
                    // but let's check what we have.
                    // Actually, the `http` struct in `relay-core-api` might have a version field.
                    // If not, we just check we got the flow.
                    if http.request.url.as_str().contains("https") {
                        break;
                    }
                }
            }
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timeout waiting for H2 flow update"),
            Ok(Some(_)) => continue,
        }
    }
}

#[tokio::test]
async fn test_concurrent_requests() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Start Proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr)
        .await
        .expect("Failed to bind proxy");
    let proxy_port = listener.local_addr().unwrap().port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    let (tx, _rx) = tokio::sync::mpsc::channel::<FlowUpdate>(1000);
    tokio::spawn(async move {
        let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
        start_proxy(source, tx, interceptor, ca, policy_rx, None, None, None)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Start Echo Server
    let echo_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let echo_listener = TcpListener::bind(echo_addr)
        .await
        .expect("Failed to bind echo server");
    let echo_port = echo_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            if let Ok((mut socket, _)) = echo_listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0; 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                        .await;
                });
            }
        }
    });

    // Run 50 concurrent requests
    let mut handles = vec![];
    for i in 0..50 {

        handles.push(tokio::spawn(async move {
            let stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
                .await
                .unwrap();
            let io = TokioIo::new(stream);
            let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
            tokio::spawn(async move { let _ = conn.await; });

            let req = hyper::Request::builder()
                .uri(format!("http://127.0.0.1:{}/{}", echo_port, i))
                .header("Host", format!("127.0.0.1:{}", echo_port))
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .unwrap();

            let res = sender.send_request(req).await.unwrap();
            assert_eq!(res.status(), 200);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}
