//! Wire contracts for target-certificate verification (roadmap §8.5).
//!
//! The policy must change what the engine actually trusts when it dials out, not just what
//! `GET /api/v1/policy` reports. Each case therefore runs a real TLS upstream with a self-signed
//! certificate and sends a real forward-proxy request.

use rcgen::generate_simple_self_signed;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::{ProxyPolicy, UpstreamTlsPolicy, UpstreamTlsVerification};
use relay_core_lib::capture::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::server::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

struct TlsUpstream {
    addr: SocketAddr,
    cert_pem: String,
}

async fn spawn_tls_upstream(body: &'static str) -> TlsUpstream {
    init_crypto();

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("generate a self-signed certificate");
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind TLS upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
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
        cert_pem: cert.cert.pem(),
    }
}

async fn start_proxy_with(policy: ProxyPolicy) -> u16 {
    start_proxy_with_updates(policy).await.0
}

/// Start the proxy and also return the policy channel, so a test can change trust mid-run.
async fn start_proxy_with_updates(
    policy: ProxyPolicy,
) -> (u16, tokio::sync::watch::Sender<ProxyPolicy>) {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, _flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    let (policy_tx, policy_rx) = tokio::sync::watch::channel(policy);

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            Arc::new(NoOpInterceptor {}),
            ca,
            policy_rx,
            // No injected client: the proxy must build one from the policy under test.
            None,
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    (proxy_port, policy_tx)
}

/// One forward-proxy request for an `https://` target the proxy must dial itself.
async fn request_through_proxy(proxy_port: u16, target_port: u16) -> (u16, String) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    stream
        .write_all(
            format!(
                "GET https://localhost:{target_port}/verify HTTP/1.1\r\n\
                 Host: localhost:{target_port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write request");

    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .expect("the proxy must answer")
        .expect("read response");
    let text = String::from_utf8_lossy(&response).to_string();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[tokio::test]
async fn a_self_signed_target_is_rejected_under_the_default_policy() {
    let upstream = spawn_tls_upstream("SHOULD-NOT-BE-TRUSTED").await;
    let proxy_port = start_proxy_with(ProxyPolicy::default()).await;

    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    assert_ne!(
        status, 200,
        "an unverifiable certificate must not be accepted, got {body}"
    );
    assert!(
        !body.contains("SHOULD-NOT-BE-TRUSTED"),
        "the upstream body must not reach the client"
    );
}

#[tokio::test]
async fn a_custom_ca_bundle_is_what_makes_the_target_trusted() {
    let upstream = spawn_tls_upstream("TRUSTED-BY-BUNDLE").await;
    let dir = std::env::temp_dir();
    let ca_path = dir.join(format!(
        "relaycore-upstream-ca-{}.pem",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&ca_path, &upstream.cert_pem).expect("write CA bundle");

    let policy = ProxyPolicy {
        upstream_tls: UpstreamTlsPolicy {
            verification: UpstreamTlsVerification::CustomCa {
                path: ca_path.clone(),
            },
        },
        ..ProxyPolicy::default()
    };
    let proxy_port = start_proxy_with(policy).await;

    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    let _ = std::fs::remove_file(&ca_path);
    assert_eq!(
        status, 200,
        "the bundle must make the target trusted: {body}"
    );
    assert!(body.contains("TRUSTED-BY-BUNDLE"), "got {body}");
}

#[tokio::test]
async fn an_explicit_insecure_policy_accepts_the_certificate() {
    let upstream = spawn_tls_upstream("INSECURE-ACCEPTED").await;
    let policy = ProxyPolicy {
        upstream_tls: UpstreamTlsPolicy {
            verification: UpstreamTlsVerification::Insecure {
                reason: "lab fixture with a self-signed certificate".to_string(),
            },
        },
        ..ProxyPolicy::default()
    };
    let proxy_port = start_proxy_with(policy).await;

    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    assert_eq!(
        status, 200,
        "an explicit insecure policy must accept the certificate: {body}"
    );
    assert!(body.contains("INSECURE-ACCEPTED"), "got {body}");
}

#[tokio::test]
async fn a_custom_ca_does_not_fall_back_to_the_native_roots() {
    // A bundle that does not contain the target's certificate must fail, even though the native
    // roots are available. Otherwise a typo in the path would silently widen trust.
    let upstream = spawn_tls_upstream("SHOULD-NOT-BE-TRUSTED").await;
    let unrelated = generate_simple_self_signed(vec!["unrelated.example".to_string()])
        .expect("generate unrelated cert");
    let dir = std::env::temp_dir();
    let ca_path = dir.join(format!("relaycore-other-ca-{}.pem", uuid::Uuid::new_v4()));
    std::fs::write(&ca_path, unrelated.cert.pem()).expect("write CA bundle");

    let policy = ProxyPolicy {
        upstream_tls: UpstreamTlsPolicy {
            verification: UpstreamTlsVerification::CustomCa {
                path: ca_path.clone(),
            },
        },
        ..ProxyPolicy::default()
    };
    let proxy_port = start_proxy_with(policy).await;

    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    let _ = std::fs::remove_file(&ca_path);
    assert_ne!(
        status, 200,
        "a bundle without the target's chain must not trust it: {body}"
    );
}

#[tokio::test]
async fn tightening_or_relaxing_verification_applies_without_a_restart() {
    let upstream = spawn_tls_upstream("AFTER-POLICY-CHANGE").await;
    let (proxy_port, policy_tx) = start_proxy_with_updates(ProxyPolicy::default()).await;

    // The default rejects the self-signed target...
    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    assert_ne!(
        status, 200,
        "the first request must fail verification: {body}"
    );

    // ...and an updated policy takes effect for the next connection, using a freshly built client.
    policy_tx
        .send(ProxyPolicy {
            upstream_tls: UpstreamTlsPolicy {
                verification: UpstreamTlsVerification::Insecure {
                    reason: "live update fixture".to_string(),
                },
            },
            ..ProxyPolicy::default()
        })
        .expect("policy channel is open");
    tokio::time::sleep(Duration::from_millis(250)).await;

    let (status, body) = request_through_proxy(proxy_port, upstream.addr.port()).await;
    assert_eq!(
        status, 200,
        "a policy update must rebuild the outbound client: {body}"
    );
    assert!(body.contains("AFTER-POLICY-CHANGE"), "got {body}");
}
