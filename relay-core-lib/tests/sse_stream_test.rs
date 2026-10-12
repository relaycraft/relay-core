//! Proxied `text/event-stream` responses (roadmap §7.2).
//!
//! The point of this file is the property that separates "parsed the stream" from "parsed the stream
//! *while* it was running": an SSE response may never end, so an implementation that reads the body
//! and then parses it would deliver nothing at all for the case that matters. The upstream here
//! therefore **blocks between two events** and the test only releases it after the first event has
//! already been reported — which is a deterministic statement, not a timing guess.
//!
//! The client's bytes are asserted too: parsing is observation, and an observer that changes what the
//! client receives is a bug, not a feature.

use relay_core_api::flow::{FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};

fn init_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

const EVENT_ONE: &str = "event: one\ndata: first\n\n";
const EVENT_TWO: &str = "event: two\ndata: second\n\n";

/// An upstream that writes one event, waits to be released, then writes a second and closes.
///
/// The wait is the whole design: while the upstream is parked, the only way an SSE update can exist
/// is if the engine parsed the event that already crossed the wire.
async fn spawn_blocking_sse_upstream(release: Arc<Notify>) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 4096];
        let _ = socket.read(&mut buf).await;

        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        if socket.write_all(EVENT_ONE.as_bytes()).await.is_err() {
            return;
        }
        let _ = socket.flush().await;

        release.notified().await;

        let _ = socket.write_all(EVENT_TWO.as_bytes()).await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });

    addr
}

/// An upstream that answers with a body that is not an event stream.
async fn spawn_json_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let body = r#"{"not":"an event stream"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });

    addr
}

struct Proxy {
    port: u16,
    flows: mpsc::Receiver<FlowUpdate>,
}

async fn spawn_proxy() -> Proxy {
    init_crypto();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, flow_rx) = mpsc::channel::<FlowUpdate>(256);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            Arc::new(NoOpInterceptor),
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    Proxy {
        port,
        flows: flow_rx,
    }
}

/// Wait for one parsed SSE event with the given payload, ignoring every other update.
async fn next_sse_event(proxy: &mut Proxy, expected: &str, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::SseEvent { event, .. })) if event.data == expected => {
                return event.kind().to_string();
            }
            Ok(Some(_)) => continue,
            Ok(None) => panic!("proxy dropped the update channel"),
            Err(_) => panic!("no SSE event carrying {expected:?} was reported in time"),
        }
    }
}

/// Events are reported **while the stream is still open**, and the client sees the bytes unchanged.
#[tokio::test]
async fn a_proxied_event_stream_is_parsed_as_it_runs() {
    let release = Arc::new(Notify::new());
    let upstream = spawn_blocking_sse_upstream(release.clone()).await;
    let mut proxy = spawn_proxy().await;

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
        .await
        .expect("connect proxy");
    let request = format!("GET http://{upstream}/events HTTP/1.1\r\nhost: {upstream}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("send request");

    // Read what the client has so far: headers plus the first event.
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&seen).contains("first") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first event never reached the client: {:?}",
            String::from_utf8_lossy(&seen)
        );
        let mut chunk = [0u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(5), client.read(&mut chunk))
            .await
            .expect("read timed out")
            .expect("read failed");
        assert!(read > 0, "the stream closed before the first event");
        seen.extend_from_slice(&chunk[..read]);
    }

    // The upstream is still parked, so this can only succeed if the event was parsed on the fly.
    let kind = next_sse_event(&mut proxy, "first", Duration::from_secs(3)).await;
    assert_eq!(kind, "one", "the event's own name must survive parsing");

    // Release the second event and check the client receives both, byte for byte.
    release.notify_one();
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut rest)).await;
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&seen),
        String::from_utf8_lossy(&rest)
    );
    assert!(
        all.contains("event: one\ndata: first\n\n"),
        "the first event must arrive exactly as the upstream wrote it: {all:?}"
    );
    assert!(
        all.contains("event: two\ndata: second\n\n"),
        "the second event must arrive too: {all:?}"
    );

    let kind = next_sse_event(&mut proxy, "second", Duration::from_secs(3)).await;
    assert_eq!(kind, "two");
}

/// The parser is armed by the peer's content type, not for every body.
#[tokio::test]
async fn a_body_that_is_not_an_event_stream_produces_no_events() {
    let upstream = spawn_json_upstream().await;
    let mut proxy = spawn_proxy().await;

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
        .await
        .expect("connect proxy");
    let request = format!("GET http://{upstream}/data HTTP/1.1\r\nhost: {upstream}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("send request");

    let mut seen = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut seen)).await;

    // Drain the updates for a moment: the body update must arrive, and no event may.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut body_updates = 0;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::HttpBody { .. })) => body_updates += 1,
            Ok(Some(FlowUpdate::SseEvent { event, .. })) => panic!(
                "a JSON body must not be parsed as an event stream, got {:?}",
                event.data
            ),
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert!(
        body_updates > 0,
        "the body itself must still be observed; only the event parsing is content-type gated"
    );
}

/// The recorded flow still says what it was, and the response body is untouched by parsing.
#[tokio::test]
async fn the_flow_records_the_stream_it_carried() {
    let release = Arc::new(Notify::new());
    let upstream = spawn_blocking_sse_upstream(release.clone()).await;
    let mut proxy = spawn_proxy().await;

    let mut client = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
        .await
        .expect("connect proxy");
    let request = format!("GET http://{upstream}/events HTTP/1.1\r\nhost: {upstream}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("send request");
    release.notify_one();

    let mut seen = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut seen)).await;

    // Find the flow snapshot and check the response is described as the stream it is.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut content_type = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let Layer::Http(http) = &flow.layer
                    && let Some(response) = &http.response
                {
                    content_type = response
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                        .map(|(_, value)| value.clone());
                    if content_type.is_some() {
                        break;
                    }
                }
            }
            Ok(Some(_)) | Err(_) => continue,
            Ok(None) => break,
        }
    }

    assert_eq!(
        content_type.as_deref(),
        Some("text/event-stream"),
        "the flow must record the content type that made it an event stream"
    );
}
