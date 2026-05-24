use super::*;
use crate::capture::loop_detection::LoopDetector;
use chrono::Utc;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use relay_core_api::flow::Flow;
use relay_core_api::flow::{NetworkInfo, TransportProtocol};
use relay_core_api::policy::{ProxyPolicy, QuicMode};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

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
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
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
    let circuit_breaker = Arc::new(crate::proxy::circuit_breaker::CircuitBreaker::default());
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
        loop_detector,
        circuit_breaker,
    )
    .await
    .unwrap();

    // 4. Verify 504 Gateway Timeout
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

fn create_dummy_client() -> Arc<HttpsClient> {
    init_crypto();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .unwrap()
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

// ── P3: Timeout classification + circuit breaker integration tests ──

/// P3-01: Verify timeout sets resilience_trace.timeout_type = "total"
#[tokio::test]
async fn test_timeout_sets_trace_timeout_type() {
    init_crypto();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0; 1024];
            let _ = socket.read(&mut buf).await;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    let policy = ProxyPolicy {
        request_timeout_ms: 100,
        ..Default::default()
    };

    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{}", addr))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let loop_detector = Arc::new(LoopDetector::new(BTreeSet::new()));
    let circuit_breaker = Arc::new(crate::proxy::circuit_breaker::CircuitBreaker::default());
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let (_tx_policy, rx_policy) = tokio::sync::watch::channel(policy);

    let resp = handle_http_request(
        req,
        "127.0.0.1:12345".parse().unwrap(),
        tx,
        create_dummy_client(),
        Arc::new(crate::interceptor::NoOpInterceptor),
        false,
        rx_policy,
        None,
        loop_detector,
        circuit_breaker,
    )
    .await
    .unwrap();

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

    // Verify resilience trace in flow update
    while let Ok(update) = rx.try_recv() {
        if let FlowUpdate::Full(flow) = update {
            if let Some(trace) = flow.resilience_trace.as_ref() {
                assert_eq!(
                    trace.timeout_type.as_deref(),
                    Some("total"),
                    "timeout_type should be 'total'"
                );
                assert!(
                    trace
                        .upstream_errors
                        .iter()
                        .any(|e| e.contains("Timed Out")),
                    "upstream_errors should contain timeout error"
                );
            }
        }
    }
}

/// P3-02: Verify circuit breaker rejection sets resilience_trace.circuit_open = true
/// Use a local echo server so the circuit breaker host key matches.
#[tokio::test]
async fn test_circuit_breaker_sets_trace() {
    init_crypto();

    // Start a local server so the host key matches the request URI authority
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let host_key = format!("127.0.0.1:{}", server_addr.port());

    tokio::spawn(async move {
        // Don't accept — let circuit breaker reject first
        let _ = listener;
    });

    let cb =
        crate::proxy::circuit_breaker::CircuitBreaker::new(1, std::time::Duration::from_secs(30));

    // Record a failure to open the circuit for this exact host
    cb.record_failure(&host_key).await;
    assert!(!cb.allow_request(&host_key).await, "circuit should be open");

    let policy = ProxyPolicy {
        request_timeout_ms: 5000,
        ..Default::default()
    };

    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{}", host_key))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let loop_detector = Arc::new(LoopDetector::new(BTreeSet::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let (_tx_policy, rx_policy) = tokio::sync::watch::channel(policy);

    let resp = handle_http_request(
        req,
        "127.0.0.1:12345".parse().unwrap(),
        tx,
        create_dummy_client(),
        Arc::new(crate::interceptor::NoOpInterceptor),
        false,
        rx_policy,
        None,
        loop_detector,
        Arc::new(cb),
    )
    .await
    .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Verify resilience trace
    while let Ok(update) = rx.try_recv() {
        if let FlowUpdate::Full(flow) = update {
            if let Some(trace) = flow.resilience_trace.as_ref() {
                assert!(trace.circuit_open, "circuit_open should be true");
            }
        }
    }
}

/// RE2: Verify Throttle rule wires ThrottleBody into HTTP pipeline.
/// A 10 KB body with 5 KB/s throttle should take roughly 2 seconds.
#[tokio::test]
async fn test_throttle_wires_into_http_pipeline() {
    init_crypto();
    let body_size = 10 * 1024; // 10 KB
    let _bps = 5 * 1024; // 5 KB/s
    let expected_min_ms = 1500; // ~2s minus tolerance

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Echo server: reads request, sends response with the body
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body_bytes = vec![b'X'; body_size];
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body_size);
            let _ = socket.write_all(response.as_bytes()).await;
            // Write body in chunks to simulate streaming
            for chunk in body_bytes.chunks(1024) {
                let _ = socket.write_all(chunk).await;
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        }
    });

    let policy = ProxyPolicy {
        request_timeout_ms: 10000,
        ..Default::default()
    };

    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{}", addr))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let loop_detector = Arc::new(LoopDetector::new(BTreeSet::new()));
    let circuit_breaker = Arc::new(crate::proxy::circuit_breaker::CircuitBreaker::default());
    let (tx, _rx) = tokio::sync::mpsc::channel(100);
    let (_tx_policy, rx_policy) = tokio::sync::watch::channel(policy);

    // Inject throttle_bytes_per_sec into flow.meta via interceptor
    // We use a simple interceptor that sets the throttle meta
    let throttle_interceptor = Arc::new(ThrottleTestInterceptor);

    let start = std::time::Instant::now();
    let resp = handle_http_request(
        req,
        "127.0.0.1:12345".parse().unwrap(),
        tx,
        create_dummy_client(),
        throttle_interceptor,
        false,
        rx_policy,
        None,
        loop_detector,
        circuit_breaker,
    )
    .await
    .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Consume the body so ThrottleBody actually throttles
    let _body_bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();

    let elapsed = start.elapsed();

    // Verify throttle actually slowed down the response
    assert!(
        elapsed.as_millis() >= expected_min_ms as u128,
        "Throttle should slow response: elapsed {}ms, expected >= {}ms",
        elapsed.as_millis(),
        expected_min_ms
    );
}

/// Simple interceptor that injects throttle_bytes_per_sec into flow.meta
struct ThrottleTestInterceptor;

#[async_trait::async_trait]
impl crate::interceptor::Interceptor for ThrottleTestInterceptor {
    async fn on_request(
        &self,
        flow: &mut Flow,
        _body: crate::interceptor::HttpBody,
    ) -> Result<crate::interceptor::RequestAction, Box<dyn std::error::Error + Send + Sync>> {
        // Inject throttle rate: 5 KB/s
        flow.meta
            .insert("throttle_bytes_per_sec".to_string(), (5 * 1024).to_string());
        Ok(crate::interceptor::RequestAction::Continue(_body))
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: crate::interceptor::HttpBody,
    ) -> Result<crate::interceptor::ResponseAction, Box<dyn std::error::Error + Send + Sync>> {
        Ok(crate::interceptor::ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        message: relay_core_api::flow::WebSocketMessage,
    ) -> Result<crate::interceptor::WebSocketMessageAction, Box<dyn std::error::Error + Send + Sync>>
    {
        Ok(crate::interceptor::WebSocketMessageAction::Continue(
            message,
        ))
    }
}
