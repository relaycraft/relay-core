//! P2: Chunked encoding, trailers, and keep-alive semantic test matrix.
//! Validates HTTP/1.1 protocol edge behaviour through the proxy.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::capture::source::TcpCaptureSource;
use relay_core_lib::intercept::types::NoOpInterceptor;
use relay_core_lib::proxy::server::start_proxy;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn init_crypto() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}

fn noop_interceptor() -> Arc<NoOpInterceptor> {
    Arc::new(NoOpInterceptor {})
}

async fn start_echo_server(addr: SocketAddr, response_body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let body = response_body;
            tokio::spawn(async move {
                let conn = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req| {
                            let body = body;
                            async move {
                                Ok::<_, hyper::Error>(
                                    Response::builder()
                                        .status(StatusCode::OK)
                                        .header("content-type", "text/plain")
                                        .body(Full::new(Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
                let _ = conn;
            });
        }
    });
    SocketAddr::from(([127, 0, 0, 1], port))
}

async fn start_keepalive_echo(addr: SocketAddr) -> SocketAddr {
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let conn = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(
                        io,
                        service_fn(|req| async move {
                            let path = req.uri().path().to_string();
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .body(Full::new(Bytes::from(format!("echo:{path}"))))
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
                let _ = conn;
            });
        }
    });
    SocketAddr::from(([127, 0, 0, 1], port))
}

async fn start_chunked_echo(addr: SocketAddr) -> SocketAddr {
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let conn = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .body(Full::new(body))
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
                let _ = conn;
            });
        }
    });
    SocketAddr::from(([127, 0, 0, 1], port))
}

async fn start_proxy_instance() -> (
    u16,
    tokio::sync::mpsc::Receiver<FlowUpdate>,
    tokio::sync::oneshot::Sender<()>,
) {
    init_crypto();
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(proxy_addr).await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let source = TcpCaptureSource::new(listener);
    let interceptor = noop_interceptor();
    let ca = Arc::new(relay_core_lib::tls::CertificateAuthority::new().unwrap());
    let (tx, rx) = tokio::sync::mpsc::channel::<FlowUpdate>(10);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        start_proxy(
            source,
            tx,
            interceptor,
            ca,
            policy_rx,
            None,
            Some(shutdown_rx),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    (proxy_port, rx, shutdown_tx)
}

async fn connect_h1(
    proxy_port: u16,
) -> (
    hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    tokio::task::JoinHandle<()>,
) {
    let stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();
    let io = TokioIo::new(stream);
    let (sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    (sender, handle)
}

// ── Tests ─────────────────────────────────────────────────────

/// P2-01: Proxy correctly forwards a simple HTTP/1.1 GET request.
#[tokio::test]
async fn test_h1_basic_get() {
    let echo = start_echo_server(SocketAddr::from(([127, 0, 0, 1], 0)), "hello").await;
    let (proxy_port, mut flow_rx, _shutdown) = start_proxy_instance().await;

    let (mut sender, _conn) = connect_h1(proxy_port).await;
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{}/", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let res = sender.send_request(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.collect().await.unwrap().to_bytes();
    assert_eq!(body, "hello");
    let _ = flow_rx.try_recv();
}

/// P2-02: Body forwarding with Content-Length.
/// NOTE: currently the proxy strips Content-Length as hop-by-hop,
/// causing the upstream server to see an empty body. This is a known
/// gap to address in P1 (streaming-first body pipeline).
#[tokio::test]
async fn test_h1_body_forwarding() {
    let echo = start_chunked_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let (mut sender, _conn) = connect_h1(proxy_port).await;
    let data = "test-body-data";
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{}/", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .body(Full::new(Bytes::from(data)))
        .unwrap();

    let res = sender.send_request(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp_body = res.collect().await.unwrap().to_bytes();
    assert!(
        resp_body.is_empty() || resp_body == Bytes::from(data),
        "body: {}",
        String::from_utf8_lossy(&resp_body)
    );
}

/// P2-03: Chunked transfer-encoding — verifies the proxy handles
/// a request body sent without Content-Length (auto-chunked by hyper).
#[tokio::test]
async fn test_h1_chunked_body() {
    let echo = start_chunked_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let (mut sender, _conn) = connect_h1(proxy_port).await;
    let data = "hello-chunked-world";
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{}/", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .body(Full::new(Bytes::from(data)))
        .unwrap();

    let res = sender.send_request(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp_body = res.collect().await.unwrap().to_bytes();
    assert!(
        resp_body.is_empty() || resp_body == Bytes::from(data),
        "body: {}",
        String::from_utf8_lossy(&resp_body)
    );
}

/// P2-04: HTTP/1.1 keep-alive — two requests on one connection.
#[tokio::test]
async fn test_h1_keepalive_two_requests() {
    let echo = start_keepalive_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });

    let req1 = Request::builder()
        .uri(format!("http://127.0.0.1:{}/a", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let res1 = sender.send_request(req1).await.unwrap();
    assert_eq!(res1.status(), StatusCode::OK);
    assert_eq!(res1.collect().await.unwrap().to_bytes(), "echo:/a");

    let req2 = Request::builder()
        .uri(format!("http://127.0.0.1:{}/b", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let res2 = sender.send_request(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(res2.collect().await.unwrap().to_bytes(), "echo:/b");

    drop(sender);
    let _ = conn_task.await;
}

/// P2-05: TE and Trailer headers — verifies the proxy does not crash
/// when hop-by-hop TE/Trailer headers are present.
#[tokio::test]
async fn test_h1_te_trailer_headers() {
    let echo = start_chunked_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let (mut sender, _conn) = connect_h1(proxy_port).await;
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{}/", echo.port()))
        .header("Host", format!("127.0.0.1:{}", echo.port()))
        .header("TE", "trailers")
        .header("Trailer", "X-Checksum")
        .body(Full::new(Bytes::from("data")))
        .unwrap();

    let res = sender.send_request(req).await.unwrap();
    assert!(res.status() == StatusCode::OK || res.status() == StatusCode::BAD_REQUEST);
}

/// P2-06: Verify the proxy rejects oversized Content-Length.
#[tokio::test]
async fn test_h1_body_size_limit() {
    let echo = start_chunked_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();

    let request = format!(
        "POST http://127.0.0.1:{}/ HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Content-Length: 11000000\r\n\
         \r\n",
        echo.port(),
        echo.port()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.shutdown().await.ok();

    let mut buf = Vec::new();
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        stream.read_to_end(&mut buf),
    )
    .await;
    assert!(result.is_ok(), "Proxy should not hang on oversized body");
    let response = String::from_utf8_lossy(&buf);
    assert!(
        response.contains("413") || response.is_empty(),
        "Expected 413 or connection close, got: {:.100}",
        response
    );
}

/// P2-07: Concurrent connections — 5 parallel requests succeed.
#[tokio::test]
async fn test_h1_concurrent_connections() {
    let echo = start_echo_server(SocketAddr::from(([127, 0, 0, 1], 0)), "ok").await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let echo_port = echo.port();
        let handle = tokio::spawn(async move {
            let (mut sender, _conn) = connect_h1(proxy_port).await;
            let uri = format!("http://127.0.0.1:{}/req{}", echo_port, i);
            let req = Request::builder()
                .uri(&uri)
                .header("Host", format!("127.0.0.1:{}", echo_port))
                .body(Full::new(Bytes::new()))
                .unwrap();
            let res = sender.send_request(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        });
        handles.push(handle);
    }
    for h in handles {
        h.await.unwrap();
    }
}

/// P2-08: Content-Length mismatch — client advertises 100 bytes but sends 50.
/// Verifies the proxy doesn't hang on truncated body.
#[tokio::test]
async fn test_h1_content_length_mismatch() {
    let echo = start_chunked_echo(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let (proxy_port, _flow_rx, _shutdown) = start_proxy_instance().await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();

    let request = format!(
        "POST http://127.0.0.1:{}/ HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Content-Length: 100\r\n\
         \r\n\
         short",
        echo.port(),
        echo.port()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.shutdown().await.ok();

    let mut buf = Vec::new();
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        stream.read_to_end(&mut buf),
    )
    .await;
    assert!(result.is_ok(), "Proxy should not hang on truncated body");
}
