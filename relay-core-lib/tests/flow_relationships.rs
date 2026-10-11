//! Connection / stream relationships on the wire (roadmap §4-1, §4 acceptance criteria).
//!
//! The model this locks is the one the reference implementations converged on: a connection identity
//! carried by every exchange, plus the protocols actually spoken. mitmproxy attaches `client_conn` /
//! `server_conn` objects to each flow and gives each a UUID (its comment says why: object identity
//! suffices while a flow is live, but a *recorded* flow still has to express "same connection"); HAR
//! 1.2 defines `entry.connection` as the unique id of the parent TCP/IP connection; Envoy exposes
//! `%CONNECTION_ID%` to cross-reference one downstream connection across logs. What none of them
//! expose is a wire stream id — that is a packet-capture fact (tshark's `http2.streamid`), and hyper
//! does not surface it either, so this engine does not invent one.
//!
//! Every assertion here needs a **real** proxy: the properties under test are produced by the accept
//! loop and by hyper's connection handling, so a synthesized `Flow` cannot show them.

use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::flow::WebSocketMessage;
use relay_core_api::flow::{CONNECT_TAG, FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::{
    BoxError, ConnectionInfo, ConnectionStats, HttpBody, Interceptor, RequestAction,
    ResponseAction, WebSocketMessageAction,
};
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

fn init_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// An upstream that answers every request the same way and keeps the connection open.
///
/// Keep-alive matters here: the point is that *one* client connection carries several exchanges.
async fn spawn_keepalive_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                // Serve every request this connection sends, so the client can reuse it.
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {
                            let body = b"ok";
                            let response = format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n",
                                body.len()
                            );
                            if socket.write_all(response.as_bytes()).await.is_err()
                                || socket.write_all(body).await.is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });

    addr
}

/// Records what `on_disconnect` was told, so `flows_count` can be asserted from outside.
#[derive(Default)]
struct StatsRecorder {
    stats: Mutex<Vec<ConnectionStats>>,
}

#[async_trait::async_trait]
impl Interceptor for StatsRecorder {
    async fn on_disconnect(&self, _conn: &ConnectionInfo, stats: &ConnectionStats) {
        self.stats.lock().expect("stats lock").push(stats.clone());
    }

    async fn on_request(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        Ok(WebSocketMessageAction::Continue(message))
    }
}

/// A proxy, plus the updates it publishes.
struct Proxy {
    port: u16,
    flows: mpsc::Receiver<FlowUpdate>,
}

async fn spawn_proxy(interceptor: Arc<dyn Interceptor>) -> Proxy {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    // `handle_http_request` sends with `send(..).await`, so a receiver nobody reads stalls the proxy
    // once the channel fills — the channel has to be wide enough, or drained, by design.
    let (flow_tx, flow_rx) = mpsc::channel::<FlowUpdate>(256);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
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

/// One client connection that stays open, so several exchanges share it.
struct ClientConn {
    sender: hyper::client::conn::http1::SendRequest<String>,
}

impl ClientConn {
    async fn connect(proxy_port: u16) -> Self {
        let stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
            .await
            .expect("connect proxy");
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .expect("handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Self { sender }
    }

    async fn get(&mut self, target: SocketAddr, path: &str) -> String {
        let uri = format!("http://{target}{path}")
            .parse::<hyper::Uri>()
            .expect("uri");
        let req = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header("host", target.to_string())
            .body(String::new())
            .expect("request");
        let resp = self.sender.send_request(req).await.expect("send");
        assert_eq!(resp.status(), 200, "the upstream must answer");
        let _ = resp.into_body().collect().await.expect("collect");
        path.to_string()
    }
}

/// Collect flows until `wanted` **distinct** flows have been seen, keeping the newest state of each.
///
/// One exchange publishes several `FlowUpdate::Full` snapshots over its life (creation, response
/// headers, completion), so counting updates would count the same exchange three times. Keying by
/// flow id is what makes "how many exchanges happened" answerable from the update stream.
async fn collect_flows(
    proxy: &mut Proxy,
    wanted: usize,
    timeout: Duration,
) -> Vec<relay_core_api::flow::Flow> {
    let mut found: std::collections::BTreeMap<uuid::Uuid, relay_core_api::flow::Flow> =
        std::collections::BTreeMap::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while found.len() < wanted {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                found.insert(flow.id, *flow);
            }
            Ok(Some(_)) => continue,
            Ok(None) => panic!("proxy dropped the update channel"),
            Err(_) => panic!(
                "only {} of {wanted} distinct flows arrived; request targets seen: {:?}",
                found.len(),
                found
                    .values()
                    .filter_map(|f| match &f.layer {
                        Layer::Http(http) => Some(http.request.url.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            ),
        }
    }
    found.into_values().collect()
}

/// Wait until the flow with `id` is republished in a state `predicate` accepts.
///
/// The CONNECT flow is published when the instruction arrives and republished when the tunnel ends,
/// so its ending is only visible on the later update.
async fn wait_for_flow(
    proxy: &mut Proxy,
    id: uuid::Uuid,
    predicate: impl Fn(&relay_core_api::flow::Flow) -> bool,
    timeout: Duration,
) -> relay_core_api::flow::Flow {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) if flow.id == id && predicate(&flow) => return *flow,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("proxy dropped the update channel"),
            Err(_) => panic!("the flow was never republished in the expected state"),
        }
    }
}

fn request_flows(flows: &[relay_core_api::flow::Flow]) -> Vec<&relay_core_api::flow::Flow> {
    flows
        .iter()
        .filter(|flow| !flow.tags.iter().any(|tag| tag == CONNECT_TAG))
        .collect()
}

/// Two exchanges over one client connection must be recognisable as such afterwards.
#[tokio::test]
async fn exchanges_on_one_connection_share_a_connection_id() {
    let upstream = spawn_keepalive_upstream().await;
    let mut proxy = spawn_proxy(Arc::new(relay_core_lib::interceptor::NoOpInterceptor)).await;

    let mut client = ClientConn::connect(proxy.port).await;
    client.get(upstream, "/first").await;
    client.get(upstream, "/second").await;
    drop(client);

    let flows = collect_flows(&mut proxy, 2, Duration::from_secs(5)).await;
    let flows = request_flows(&flows);
    assert_eq!(flows.len(), 2, "both exchanges must be recorded");

    let first = flows[0].connection_id.as_deref();
    let second = flows[1].connection_id.as_deref();
    assert!(
        first.is_some(),
        "each exchange must name the connection it arrived on"
    );
    assert_eq!(
        first, second,
        "two exchanges on one client connection must share a connection id"
    );
    assert_ne!(
        flows[0].id, flows[1].id,
        "sharing a connection must not mean sharing a flow id"
    );
    for flow in &flows {
        assert_eq!(
            flow.protocol_stack,
            vec!["tcp".to_string(), "h1".to_string()],
            "a cleartext HTTP/1.1 exchange is exactly that"
        );
    }
}

/// Two connections must not be confusable: that is the whole point of the id.
#[tokio::test]
async fn separate_connections_get_separate_ids() {
    let upstream = spawn_keepalive_upstream().await;
    let mut proxy = spawn_proxy(Arc::new(relay_core_lib::interceptor::NoOpInterceptor)).await;

    let mut first = ClientConn::connect(proxy.port).await;
    first.get(upstream, "/one").await;
    drop(first);

    let mut second = ClientConn::connect(proxy.port).await;
    second.get(upstream, "/two").await;
    drop(second);

    let flows = collect_flows(&mut proxy, 2, Duration::from_secs(5)).await;
    let flows = request_flows(&flows);
    assert_eq!(flows.len(), 2);
    assert_ne!(
        flows[0].connection_id, flows[1].connection_id,
        "two client connections must get two ids"
    );
}

/// The CONNECT instruction is recorded, tagged, and is what the tunnel's exchanges point at.
#[tokio::test]
async fn a_tunnel_records_its_connect_flow_and_links_the_exchanges_inside_it() {
    let upstream = spawn_keepalive_upstream().await;
    let mut proxy = spawn_proxy(Arc::new(relay_core_lib::interceptor::NoOpInterceptor)).await;

    // A plaintext CONNECT: the tunnel sniffs the client's first bytes and finds an HTTP/1.x request,
    // which is the path a `curl --proxytunnel` or a gRPC client with HTTP_PROXY takes.
    let mut socket = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
        .await
        .expect("connect proxy");
    let connect = format!(
        "CONNECT {upstream} HTTP/1.1\r\nHost: {upstream}\r\n\r\n",
        upstream = upstream
    );
    socket
        .write_all(connect.as_bytes())
        .await
        .expect("send CONNECT");

    let mut head = vec![0u8; 1024];
    let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut head))
        .await
        .expect("CONNECT answer timed out")
        .expect("read CONNECT answer");
    let answer = String::from_utf8_lossy(&head[..read]).to_string();
    assert!(
        answer.starts_with("HTTP/1.1 200"),
        "the tunnel must be established: {answer:?}"
    );

    // One request inside the tunnel, in origin form — the CONNECT line supplied the authority.
    socket
        .write_all(b"GET /inside HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .await
        .expect("send tunneled request");
    let mut body = vec![0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut body)).await;
    // Closing the client ends the tunnel, which is what lets the CONNECT flow report its ending.
    drop(socket);

    let flows = collect_flows(&mut proxy, 2, Duration::from_secs(5)).await;

    let connect_flow = flows
        .iter()
        .find(|flow| flow.tags.iter().any(|tag| tag == CONNECT_TAG))
        .expect("the CONNECT instruction must be recorded as a flow");
    assert!(
        matches!(connect_flow.layer, Layer::Http(_)),
        "CONNECT is an HTTP exchange, not a transport flow"
    );
    let connect_id = connect_flow
        .connection_id
        .as_deref()
        .expect("the CONNECT flow belongs to the client connection");

    let inside = request_flows(&flows)
        .into_iter()
        .find(|flow| match &flow.layer {
            Layer::Http(http) => http.request.url.path() == "/inside",
            _ => false,
        })
        .expect("the exchange inside the tunnel must be recorded");
    assert_eq!(
        inside.parent_flow_id,
        Some(connect_flow.id),
        "the exchange inside the tunnel must point back at the CONNECT that opened it"
    );
    assert_eq!(
        inside.connection_id.as_deref(),
        Some(connect_id),
        "a tunnel's exchanges stay on the same client connection"
    );

    // And the tunnel's own ending is recorded, on the later update of the same flow.
    let connect_id = connect_flow.id;
    let finished = wait_for_flow(
        &mut proxy,
        connect_id,
        |flow| flow.end_time.is_some(),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        finished.close_reason,
        Some(relay_core_api::event::CloseReason::Completed),
        "a tunnel that ended because the client hung up completed normally"
    );
    assert!(
        finished.end_time >= Some(finished.start_time),
        "the tunnel's duration must be real"
    );
}

/// `ConnectionStats.flows_count` used to be a hard-coded zero; it is now counted where flows are
/// built, which is the only place that sees every path (plain exchange, WebSocket, tunnel interior).
#[tokio::test]
async fn disconnect_reports_how_many_flows_the_connection_produced() {
    let upstream = spawn_keepalive_upstream().await;
    let recorder = Arc::new(StatsRecorder::default());
    let proxy = spawn_proxy(recorder.clone()).await;

    let mut client = ClientConn::connect(proxy.port).await;
    client.get(upstream, "/one").await;
    client.get(upstream, "/two").await;
    drop(client);

    // The connection ends when the client drops it, and `on_disconnect` follows.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(stats) = recorder.stats.lock().expect("stats lock").last().cloned() {
            assert_eq!(
                stats.flows_count, 2,
                "the connection produced two exchanges, and the count must say two"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "on_disconnect was never called for the closed connection"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Keep the receiver alive so the proxy is not dropped mid-test.
    let _ = proxy.port;
}

/// The stack records where a request came from in the capture, not in the configuration.
#[test]
fn the_protocol_stack_names_what_was_spoken() {
    use relay_core_api::flow::http_protocol_stack;

    assert_eq!(
        http_protocol_stack(true, "HTTP/2.0"),
        vec!["tcp".to_string(), "tls".to_string(), "h2".to_string()],
        "h2 inside a terminated tunnel"
    );
    assert_eq!(
        http_protocol_stack(false, "HTTP/2.0"),
        vec!["tcp".to_string(), "h2c".to_string()],
        "prior-knowledge HTTP/2 in the clear is h2c, not h2"
    );
    assert_eq!(
        http_protocol_stack(true, "HTTP/1.1"),
        vec!["tcp".to_string(), "tls".to_string(), "h1".to_string()]
    );
    assert_eq!(
        http_protocol_stack(false, "HTTP/1.0"),
        vec!["tcp".to_string(), "h1".to_string()],
        "HTTP/1.0 is still HTTP/1"
    );
    assert_eq!(
        http_protocol_stack(false, "not-a-version"),
        vec!["tcp".to_string()],
        "an unparsable version stops the stack rather than naming a guess"
    );
}
