//! Request trailers must reach the capture, not only the upstream.
//!
//! HTTP/1.1 chunked bodies can carry trailers after the final chunk. `TapBody` already forwarded
//! them, but `HttpRequest` had no field, so a capture could not answer what the client sent. This
//! runs a real chunked upload through the proxy and reads the recorded flow.

use bytes::Bytes;
use hyper::body::{Body, Frame};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use relay_core_api::flow::{FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::capture::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::server::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One data frame followed by one trailers frame, which is the shape a chunked upload takes.
struct TraileredRequest {
    phase: u8,
}

impl Body for TraileredRequest {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.phase {
            0 => {
                self.phase = 1;
                Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"payload")))))
            }
            1 => {
                self.phase = 2;
                let mut trailers = hyper::HeaderMap::new();
                trailers.insert("x-checksum", "abc".parse().expect("header value"));
                Poll::Ready(Some(Ok(Frame::trailers(trailers))))
            }
            _ => Poll::Ready(None),
        }
    }
}

async fn spawn_draining_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(|req: Request<hyper::body::Incoming>| async move {
                            // Draining the body is what makes the proxy drive the stream to its
                            // trailers; an upstream that answered early would leave them unread.
                            let _ = http_body_util::BodyExt::collect(req.into_body()).await;
                            Ok::<_, std::convert::Infallible>(Response::new(
                                http_body_util::Full::new(Bytes::from_static(b"ok")),
                            ))
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

async fn start_proxy_with_flows() -> (u16, tokio::sync::mpsc::Receiver<FlowUpdate>) {
    let _ = rustls::crypto::ring::default_provider().install_default();

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
async fn request_trailers_are_recorded_on_the_flow() {
    let upstream = spawn_draining_upstream().await;
    let (proxy_port, mut flows) = start_proxy_with_flows().await;

    let stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake with the proxy");
    tokio::spawn(conn);

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        sender.send_request(
            Request::builder()
                .method("POST")
                .uri(format!("http://{upstream}/trailers"))
                .header("host", upstream.to_string())
                .header("transfer-encoding", "chunked")
                .header("trailer", "x-checksum")
                .body(TraileredRequest { phase: 0 })
                .expect("request"),
        ),
    )
    .await
    .expect("the request must not hang")
    .expect("the proxy must answer");
    assert_eq!(response.status(), 200);

    // The recording is an incremental update or part of the final snapshot; accept either, but
    // require that the trailer's value is on the flow before the test ends.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut recorded: Option<Vec<(String, String)>> = None;
    while tokio::time::Instant::now() < deadline && recorded.is_none() {
        match tokio::time::timeout(Duration::from_millis(200), flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let Layer::Http(http) = &flow.layer
                    && http.request.url.path() == "/trailers"
                    && !http.request.trailers.is_empty()
                {
                    recorded = Some(http.request.trailers.clone());
                }
            }
            Ok(Some(FlowUpdate::RequestTrailers { trailers, .. })) if !trailers.is_empty() => {
                recorded = Some(trailers);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    assert_eq!(
        recorded,
        Some(vec![("x-checksum".to_string(), "abc".to_string())]),
        "the request trailer the client sent must be recorded on the flow"
    );
}
