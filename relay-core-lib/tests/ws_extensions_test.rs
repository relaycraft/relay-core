//! WebSocket extension negotiation (roadmap §7.1: "support / pass through / explicit downgrade").
//!
//! `permessage-deflate` changes how every frame after the handshake is framed, and this engine
//! implements none of the extensions. Passing the client's offer through would therefore make the
//! *upstream* accept it and the client see it in our 101 — two peers believing an extension is
//! negotiated end to end while the tunnel copies bytes. That is the silent-breakage case the roadmap
//! forbids, so the behaviour under test is the third option: **explicit downgrade**, in both
//! directions, made visible on the flow.
//!
//! Both directions matter and for different reasons: stripping only the request leaves an upstream
//! that offers the extension unprompted able to convince the client to compress into a tunnel that
//! cannot read it.

use relay_core_api::flow::{FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::proxy::websocket::WS_EXTENSIONS_STRIPPED_TAG;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

fn init_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A hand-rolled WebSocket upstream that reports the request bytes it was sent.
///
/// Hand-rolled rather than `tokio_tungstenite` because the assertion is about *the bytes the upstream
/// received*: a library that sanitises the handshake would hide exactly what is being checked. It
/// answers with an extension of its own on purpose — the case a request-only strip would miss.
async fn spawn_capturing_ws_upstream() -> (SocketAddr, oneshot::Receiver<String>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ws upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let mut request = Vec::new();
        // Read until the end of the handshake head.
        while !String::from_utf8_lossy(&request).contains("\r\n\r\n") {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&request).to_string());

        // `Sec-WebSocket-Accept` for the key below; the engine does not verify it, but a real
        // handshake is what makes this a test of the proxy rather than of a malformed upstream.
        let response = "HTTP/1.1 101 Switching Protocols\r\n\
                        Upgrade: websocket\r\n\
                        Connection: Upgrade\r\n\
                        Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                        Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n";
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    });

    (addr, rx)
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

/// Send a WebSocket handshake through the proxy and return what the client received.
async fn handshake_via_client(proxy_port: u16, target: SocketAddr, extra_headers: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{target}/ws HTTP/1.1\r\nHost: {target}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n{extra_headers}\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write handshake");

    let mut buf = vec![0u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake timed out")
        .expect("read handshake response");
    String::from_utf8_lossy(&buf[..read]).to_string()
}

/// Collect WebSocket-layer flow snapshots until one carries the downgrade tag, or time out.
async fn wait_for_ws_flow(proxy: &mut Proxy, timeout: Duration) -> Option<Vec<String>> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_tags: Option<Vec<String>> = None;
    while tokio::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, proxy.flows.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if matches!(flow.layer, Layer::WebSocket(_)) {
                    last_tags = Some(flow.tags.clone());
                    if flow
                        .tags
                        .iter()
                        .any(|tag| tag == WS_EXTENSIONS_STRIPPED_TAG)
                    {
                        return last_tags;
                    }
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
    last_tags
}

/// The offer never reaches the upstream, never comes back to the client, and is on the flow.
#[tokio::test]
async fn an_extension_offer_is_downgraded_in_both_directions() {
    let (upstream, captured) = spawn_capturing_ws_upstream().await;
    let mut proxy = spawn_proxy().await;

    let client_saw = handshake_via_client(
        proxy.port,
        upstream,
        "Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n",
    )
    .await;

    let upstream_request = tokio::time::timeout(Duration::from_secs(5), captured)
        .await
        .expect("the upstream never received a handshake")
        .expect("the upstream's capture was dropped");

    assert!(
        !upstream_request
            .to_ascii_lowercase()
            .contains("sec-websocket-extensions"),
        "the offer must not reach the upstream, which would otherwise accept it:\n{upstream_request}"
    );
    assert!(
        !client_saw
            .to_ascii_lowercase()
            .contains("sec-websocket-extensions"),
        "the upstream offered an extension unprompted, and telling the client would make it \
         compress into a tunnel that cannot read the frames:\n{client_saw}"
    );
    assert!(
        client_saw.starts_with("HTTP/1.1 101"),
        "the handshake must still succeed — a downgrade is not a refusal:\n{client_saw}"
    );

    let tags = wait_for_ws_flow(&mut proxy, Duration::from_secs(3))
        .await
        .expect("the WebSocket flow must be recorded");
    assert!(
        tags.iter().any(|tag| tag == WS_EXTENSIONS_STRIPPED_TAG),
        "a downgrade that leaves no trace is the silent kind: {tags:?}"
    );

    // The parts of the handshake that must *not* be touched: a downgrade of extensions is not a
    // downgrade of the upgrade.
    // Case-insensitively: re-sending the head normalises header names to lower case, which is
    // ordinary HTTP/1.1 behaviour and not something this test should pin.
    assert!(
        upstream_request
            .to_ascii_lowercase()
            .contains("sec-websocket-key"),
        "the handshake key must reach the upstream:\n{upstream_request}"
    );
}

/// A client that asked for nothing gets no downgrade tag, so the tag means what it says.
#[tokio::test]
async fn a_client_that_offered_no_extension_is_not_tagged() {
    let (upstream, captured) = spawn_capturing_ws_upstream().await;
    let mut proxy = spawn_proxy().await;

    let _ = handshake_via_client(proxy.port, upstream, "").await;
    let _ = tokio::time::timeout(Duration::from_secs(5), captured).await;

    // Give the flow snapshots a moment, then assert none of them claims a downgrade.
    let tags = wait_for_ws_flow(&mut proxy, Duration::from_secs(2)).await;
    if let Some(tags) = tags {
        assert!(
            !tags.iter().any(|tag| tag == WS_EXTENSIONS_STRIPPED_TAG),
            "nothing was stripped, so nothing may be tagged: {tags:?}"
        );
    }
}

/// A subprotocol is not an extension: stripping it would break every client that asked for one.
#[tokio::test]
async fn a_subprotocol_is_passed_through_untouched() {
    let (upstream, captured) = spawn_capturing_ws_upstream().await;
    let proxy = spawn_proxy().await;

    let _ = handshake_via_client(
        proxy.port,
        upstream,
        "Sec-WebSocket-Protocol: graphql-ws\r\n",
    )
    .await;

    let upstream_request = tokio::time::timeout(Duration::from_secs(5), captured)
        .await
        .expect("the upstream never received a handshake")
        .expect("capture dropped");

    assert!(
        upstream_request
            .to_ascii_lowercase()
            .contains("sec-websocket-protocol: graphql-ws"),
        "a subprotocol does not change framing, so it must be forwarded:\n{upstream_request}"
    );
}
