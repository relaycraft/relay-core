//! An HTTP/2 RST_STREAM must read as "this exchange was reset", not as "the upstream went away".
//!
//! The outbound connector erases the hyper error into an `io::Error`, so the proxy classifies the
//! remaining text. This runs a real h2c upstream that accepts the request and immediately resets the
//! stream, then asserts both the 502 and the recorded `CloseReason::Reset`.

use relay_core_api::event::CloseReason;
use relay_core_api::flow::{FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::capture::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::server::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn spawn_resetting_h2_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind h2 upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let Ok(mut connection) = h2::server::handshake(socket).await else {
                    return;
                };
                while let Some(accepted) = connection.accept().await {
                    let Ok((_request, mut send_response)) = accepted else {
                        continue;
                    };
                    // The client's request is refused at the stream level; the connection stays up.
                    send_response.send_reset(h2::Reason::CANCEL);
                }
            });
        }
    });
    addr
}

async fn start_proxy_with_flows() -> (u16, tokio::sync::mpsc::Receiver<FlowUpdate>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            Arc::new(NoOpInterceptor {}),
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    (proxy_port, flow_rx)
}

#[tokio::test]
async fn a_reset_upstream_stream_is_recorded_as_a_reset() {
    let upstream = spawn_resetting_h2_upstream().await;
    let (proxy_port, mut flows) = start_proxy_with_flows().await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    // gRPC-shaped headers are what make the direct connector choose h2c for a plaintext target.
    stream
        .write_all(
            format!(
                "POST http://{upstream}/pkg.Service/Method HTTP/1.1\r\n\
                 Host: {upstream}\r\n\
                 content-type: application/grpc\r\n\
                 te: trailers\r\n\
                 content-length: 0\r\n\
                 Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write request");

    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut response))
        .await
        .expect("the proxy must answer a reset stream")
        .expect("read response");
    let head = String::from_utf8_lossy(&response);
    assert!(
        head.starts_with("HTTP/1.1 502"),
        "a reset upstream stream must surface as a bad gateway, got {head}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut recorded: Option<Option<CloseReason>> = None;
    let mut recorded_error: Option<String> = None;
    while tokio::time::Instant::now() < deadline && recorded.is_none() {
        match tokio::time::timeout(Duration::from_millis(200), flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let Layer::Http(http) = &flow.layer
                    && http.request.url.path() == "/pkg.Service/Method"
                    && http.response.is_none()
                    && flow.close_reason.is_some()
                {
                    recorded = Some(flow.close_reason.clone());
                    recorded_error = http.error.clone();
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    assert_eq!(
        recorded,
        Some(Some(CloseReason::Reset)),
        "the flow must say the stream was reset, not merely that the upstream closed; error={recorded_error:?}"
    );
}
