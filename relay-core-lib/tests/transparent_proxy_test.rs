use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::capture::source::{CaptureSource, IncomingConnection};
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

struct MockTransparentSource {
    listener: TcpListener,
    target_addr: SocketAddr,
}

impl MockTransparentSource {
    async fn new(addr: SocketAddr, target_addr: SocketAddr) -> Self {
        let listener = TcpListener::bind(addr)
            .await
            .expect("Failed to bind mock source");
        Self {
            listener,
            target_addr,
        }
    }
}

impl CaptureSource for MockTransparentSource {
    type IO = TcpStream;

    fn accept(
        &mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = relay_core_lib::error::Result<IncomingConnection<Self::IO>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let (stream, client_addr) = self.listener.accept().await?;
            Ok(IncomingConnection {
                stream,
                client_addr,
                target_addr: Some(self.target_addr),
            })
        })
    }

    fn listen_addrs(&self) -> Vec<SocketAddr> {
        if let Ok(addr) = self.listener.local_addr() {
            vec![addr]
        } else {
            vec![]
        }
    }
}

#[tokio::test]
async fn test_transparent_proxy_routing() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Setup Echo Server (Target)
    let echo_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let echo_listener = TcpListener::bind(echo_addr)
        .await
        .expect("Failed to bind echo server");
    let echo_port = echo_listener.local_addr().unwrap().port();
    let echo_socket_addr = echo_listener.local_addr().unwrap();

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

    // 2. Setup Transparent Proxy with Mock Source
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    // We bind to a random port, and the MockSource will listen on it
    let source = MockTransparentSource::new(proxy_addr, echo_socket_addr).await;
    let proxy_port = source.listener.local_addr().unwrap().port();

    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    let (tx, _rx) = tokio::sync::mpsc::channel::<FlowUpdate>(10);
    let on_flow = tx.clone();

    tokio::spawn(async move {
        let policy = ProxyPolicy { transparent_enabled: true, ..Default::default() };

        let (_policy_tx, policy_rx) = tokio::sync::watch::channel(policy);
        start_proxy(
            source,
            on_flow,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 3. Make Request to Proxy (acting as if redirected)
    // The client connects to the proxy, but sends a request intended for the target
    // In transparent mode, the client thinks it connects to target (but iptables redirected it)
    // Here we simulate that by connecting to proxy port, but sending a request with Host header for target
    // AND our MockSource injects target_addr = echo_socket_addr.

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

    // Request with relative URI (transparent clients usually send absolute URI? No, usually relative because they think they talk to origin)
    // But most HTTP clients send absolute URI to proxy.
    // In transparent mode, the client doesn't know it's a proxy. So it sends relative URI + Host header.
    let req = hyper::Request::builder()
        .uri("/")
        .header("Host", format!("127.0.0.1:{}", echo_port))
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .unwrap();

    let res = sender.send_request(req).await.expect("Request failed");

    assert_eq!(res.status(), 200);
    let body = res.collect().await.unwrap().to_bytes();
    assert_eq!(body, "Hello World!");
}

#[tokio::test]
async fn test_transparent_proxy_loop_detection() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Setup Transparent Proxy with Mock Source pointing to ITSELF (Loop)
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    // First bind to get a port
    let listener = TcpListener::bind(proxy_addr).await.unwrap();
    let proxy_socket_addr = listener.local_addr().unwrap();

    // Create source that targets the proxy itself
    let source = MockTransparentSource {
        listener,
        target_addr: proxy_socket_addr,
    };
    let proxy_port = proxy_socket_addr.port();

    let interceptor = Arc::new(NoOpInterceptor {});
    let ca = Arc::new(CertificateAuthority::new().expect("Failed to create CA"));

    let (tx, _rx) = tokio::sync::mpsc::channel::<FlowUpdate>(10);
    let on_flow = tx.clone();

    tokio::spawn(async move {
        let policy = ProxyPolicy { transparent_enabled: true, ..Default::default() };

        let (_policy_tx, policy_rx) = tokio::sync::watch::channel(policy);
        start_proxy(
            source,
            on_flow,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 3. Connect to Proxy
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
        .uri("/")
        .header("Host", "example.com")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .unwrap();

    let res = sender.send_request(req).await.expect("Request failed");

    // Should be 508 Loop Detected
    assert_eq!(res.status(), 508);
}
