use super::*;
use relay_core_api::flow::Flow;
use relay_core_api::policy::{ProxyPolicy, QuicMode};
use http_body_util::Full;
use std::sync::Arc;
use hyper::{Request, Response, StatusCode};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::Once;
use std::collections::{BTreeSet, HashMap};
use crate::capture::loop_detection::LoopDetector;
use uuid::Uuid;
use chrono::Utc;
use relay_core_api::flow::{NetworkInfo, TransportProtocol};

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn create_test_flow() -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_ip: "1.1.1.1".to_string(),
            server_port: 80,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: relay_core_api::flow::Layer::Unknown,
        tags: vec![],
        meta: HashMap::new(),
    }
}

#[tokio::test]
async fn test_request_timeout() {
    init_crypto();
    // 1. Setup a slow server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            // Read request
            let mut buf = [0; 1024];
            let _ = socket.read(&mut buf).await;
            // Sleep longer than timeout
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            // Write response
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    // 2. Setup Policy with short timeout
    let policy = ProxyPolicy {
        request_timeout_ms: 100, // 100ms timeout
        ..Default::default()
    };

    // 3. Make request
    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{}", addr))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let loop_detector = Arc::new(LoopDetector::new(BTreeSet::new()));
    let (tx, _rx) = tokio::sync::mpsc::channel(100);
    let (_tx_policy, rx_policy) = tokio::sync::watch::channel(policy);

    let resp = handle_http_request(
        req,
        "127.0.0.1:12345".parse().unwrap(),
        tx,
        create_dummy_client(),
        Arc::new(crate::interceptor::NoOpInterceptor),
        false, // is_mitm
        rx_policy,
        None, // target_addr
        loop_detector
    ).await.unwrap();

    // 4. Verify 504 Gateway Timeout
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}


fn create_dummy_client() -> Arc<HttpsClient> {
    init_crypto();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots().unwrap()
        .https_or_http()
        .enable_http1()
        .build();
    Arc::new(Client::builder(hyper_util::rt::TokioExecutor::new()).build(https))
}

#[test]
fn test_quic_downgrade_logic() {
    // 1. Setup Policy (Downgrade Mode)
    let policy = ProxyPolicy {
        quic_mode: QuicMode::Downgrade,
        quic_downgrade_clear_cache: true,
        ..Default::default()
    };

    // 2. Setup Response with Alt-Svc
    let response = Response::builder()
        .header("Alt-Svc", "h3=\":443\"; ma=86400, h3-29=\":443\"; ma=86400")
        .body(())
        .unwrap();
    let (mut parts, _) = response.into_parts();

    // 3. Setup Flow
    let mut flow = create_test_flow(); 

    // 4. Apply Logic
    super::apply_quic_downgrade(&mut parts, &mut flow, &policy);

    // 5. Verify
    // Alt-Svc should be removed
    assert!(parts.headers.get("Alt-Svc").is_none());
    // Tag added
    assert!(flow.tags.contains(&"quic-downgraded".to_string()));
    // Clear-Site-Data added
    if let Some(val) = parts.headers.get("clear-site-data") {
        assert_eq!(val, "\"cache\"");
    } else {
        panic!("clear-site-data header missing");
    }
}

#[test]
fn test_quic_passthrough_logic() {
    // 1. Setup Policy (Passthrough Mode)
    let policy = ProxyPolicy {
        quic_mode: QuicMode::Passthrough,
        ..Default::default()
    };

    // 2. Setup Response with Alt-Svc
    let response = Response::builder()
        .header("Alt-Svc", "h3=\":443\"")
        .body(())
        .unwrap();
    let (mut parts, _) = response.into_parts();
    let mut flow = create_test_flow();

    // 3. Apply Logic
    super::apply_quic_downgrade(&mut parts, &mut flow, &policy);

    // 4. Verify
    // Alt-Svc should remain
    assert!(parts.headers.get("Alt-Svc").is_some());
    // No tag
    assert!(!flow.tags.contains(&"quic-downgraded".to_string()));
}

#[test]
fn test_quic_downgrade_without_clear_cache_does_not_add_header() {
    let policy = ProxyPolicy {
        quic_mode: QuicMode::Downgrade,
        quic_downgrade_clear_cache: false,
        ..Default::default()
    };

    let response = Response::builder()
        .header("Alt-Svc", "h3=\":443\"; ma=86400")
        .body(())
        .unwrap();
    let (mut parts, _) = response.into_parts();
    let mut flow = create_test_flow();

    super::apply_quic_downgrade(&mut parts, &mut flow, &policy);

    assert!(parts.headers.get("Alt-Svc").is_none());
    assert!(flow.tags.contains(&"quic-downgraded".to_string()));
    assert!(
        parts.headers.get("clear-site-data").is_none(),
        "clear-site-data should only be injected when quic_downgrade_clear_cache=true"
    );
}
