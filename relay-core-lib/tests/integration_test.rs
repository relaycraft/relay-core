use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}

#[tokio::test]
async fn test_proxy_basic_http_request() {
    init_crypto();

    // 1. Setup Echo Server (Target)
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
                    let mut buf = [0; 4096];
                    let _ = socket.read(&mut buf).await;
                    // Simple HTTP Response
                    let response = "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nHello World!";
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        }
    });

    // 2. Setup Proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr)
        .await
        .expect("Failed to bind proxy");
    let proxy_port = listener.local_addr().unwrap().port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    // Capture flows to verify
    let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(10);

    // Spawn Proxy
    tokio::spawn(async move {
        let (_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
        start_proxy(source, tx, interceptor, ca, policy_rx, None, None, None)
            .await
            .unwrap();
    });

    // Give it a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 3. Make Request via Proxy
    // We connect to Proxy Port, but send request with Absolute URI (Standard Proxy)
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .expect("Failed to connect to proxy");
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("Handshake failed");
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Connection failed: {:?}", e);
        }
    });

    let req = hyper::Request::builder()
        .uri(format!("http://127.0.0.1:{}/", echo_port))
        .header("Host", format!("127.0.0.1:{}", echo_port))
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .unwrap();

    let res = sender.send_request(req).await.expect("Request failed");

    assert_eq!(res.status(), 200);
    let body = res.collect().await.unwrap().to_bytes();
    assert_eq!(body, "Hello World!");

    // 4. Verify Flow Capture
    // We expect at least one FlowUpdate
    let update = rx.recv().await.expect("Should receive flow update");
    match update {
        FlowUpdate::Full(flow) => {
            if let relay_core_api::flow::Layer::Http(http) = flow.layer {
                assert_eq!(http.request.method, "GET");
            } else {
                panic!("Expected HTTP Layer");
            }
        }
        _ => panic!("Expected Full Flow update initially"),
    }
}

#[tokio::test]
async fn test_proxy_large_request_body() {
    init_crypto();

    // 1. Setup Dummy Upstream (Target)
    let upstream_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let upstream_listener = TcpListener::bind(upstream_addr)
        .await
        .expect("Failed to bind upstream");
    let upstream_port = upstream_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = upstream_listener.accept().await {
            // Parse HTTP request headers to determine body length, then read body,
            // then send a 200 response.
            let mut header_buf = Vec::new();
            let mut byte = [0u8; 1];
            let mut content_length: usize = 0;
            loop {
                if tokio::io::AsyncReadExt::read(&mut socket, &mut byte)
                    .await
                    .ok()
                    != Some(1)
                {
                    break;
                }
                header_buf.push(byte[0]);
                if header_buf.len() >= 4 && &header_buf[header_buf.len() - 4..] == b"\r\n\r\n" {
                    // Parse Content-Length from headers
                    let header_str = String::from_utf8_lossy(&header_buf);
                    for line in header_str.lines() {
                        if let Some(val) = line.strip_prefix("Content-Length: ") {
                            content_length = val.trim().parse().unwrap_or(0);
                        } else if let Some(val) = line.strip_prefix("content-length: ") {
                            content_length = val.trim().parse().unwrap_or(0);
                        }
                    }
                    break;
                }
            }
            // Read body bytes
            let mut body_read = 0;
            let mut tmp = [0u8; 8192];
            while body_read < content_length {
                let to_read = std::cmp::min(tmp.len(), content_length - body_read);
                match tokio::io::AsyncReadExt::read(&mut socket, &mut tmp[..to_read]).await {
                    Ok(0) => break,
                    Ok(n) => body_read += n,
                    Err(_) => break,
                }
            }
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, resp.as_bytes()).await;
        }
    });

    // 2. Setup Proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr)
        .await
        .expect("Failed to bind proxy");
    let proxy_port = listener.local_addr().unwrap().port();

    let source = TcpCaptureSource::new(listener);
    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    // Capture flows (ignore for this test)
    let (tx, _rx) = tokio::sync::mpsc::channel::<FlowUpdate>(100);

    // Spawn Proxy
    tokio::spawn(async move {
        let (_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
        start_proxy(source, tx, interceptor, ca, policy_rx, None, None, None)
            .await
            .unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 2. Make Request with Large Body
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .expect("Failed to connect to proxy");
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("Handshake failed");
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Connection failed: {:?}", e);
        }
    });

    // Create 11MB body
    // Use stream to avoid allocating all at once if possible, but hyper client body needs to be known size or chunked.
    // For simplicity, let's just make a large vector. 11MB is fine for test.
    let body_data = vec![0u8; 11 * 1024 * 1024];

    let req = hyper::Request::builder()
        .uri(format!("http://127.0.0.1:{}/", upstream_port))
        .header("Host", format!("127.0.0.1:{}", upstream_port))
        .body(http_body_util::Full::new(bytes::Bytes::from(body_data)))
        .unwrap();

    match sender.send_request(req).await {
        Ok(res) => {
            // P1a streaming-first: large body is no longer hard-rejected with 413.
            // The proxy forwards the request to upstream and returns upstream's response.
            assert_eq!(res.status(), hyper::StatusCode::OK);
        }
        Err(e) => {
            // If the connection was reset (e.g., upstream closed early), that's also acceptable
            // for the streaming-first pipeline.
            let s = e.to_string();
            let d = format!("{:?}", e);
            if s.contains("Connection reset")
                || s.contains("Broken pipe")
                || s.contains("Connection closed")
                || d.contains("ConnectionReset")
                || d.contains("BodyWrite")
            {
                println!(
                    "Got connection error (acceptable for large body streaming): {} / {:?}",
                    s, d
                );
            } else {
                panic!("Request failed with unexpected error: {:?}", e);
            }
        }
    }
}
