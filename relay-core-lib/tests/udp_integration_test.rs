// Every test here drives the UDP proxy with an explicit `remote_addr`, which is the *plain* forward:
// the destination is configured, not recovered from the kernel, so nothing needs `IP_TRANSPARENT`
// and no privilege is required.
//
// This file used to be gated off entirely on Linux, because `UdpProxy::run` enabled TPROXY
// unconditionally — so a plain forward asked the kernel for capabilities a CI runner does not have,
// and the only platform that gates a release ran none of this. The proxy now opens a transparent
// socket pair only when the destination came from the kernel, so these run everywhere.

use relay_core_api::flow::{Flow, FlowUpdate};
use relay_core_lib::capture::udp::UdpProxy;
use relay_core_lib::interceptor::{InterceptionResult, Interceptor};
use std::sync::Arc;
use tokio::sync::mpsc;

struct DropAllInterceptor;
#[async_trait::async_trait]
impl Interceptor for DropAllInterceptor {
    async fn on_udp_session(&self, _: &mut Flow) -> InterceptionResult {
        InterceptionResult::Drop
    }

    async fn on_request(
        &self,
        _: &mut Flow,
        body: relay_core_lib::interceptor::HttpBody,
    ) -> Result<relay_core_lib::interceptor::RequestAction, relay_core_lib::interceptor::BoxError>
    {
        Ok(relay_core_lib::interceptor::RequestAction::Continue(body))
    }

    async fn on_response(
        &self,
        _: &mut Flow,
        body: relay_core_lib::interceptor::HttpBody,
    ) -> Result<relay_core_lib::interceptor::ResponseAction, relay_core_lib::interceptor::BoxError>
    {
        Ok(relay_core_lib::interceptor::ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _: &mut Flow,
        msg: relay_core_api::flow::WebSocketMessage,
    ) -> Result<
        relay_core_lib::interceptor::WebSocketMessageAction,
        relay_core_lib::interceptor::BoxError,
    > {
        Ok(relay_core_lib::interceptor::WebSocketMessageAction::Continue(msg))
    }
}

/// Verify that a UDP proxy with DropAll interceptor produces no flows.
///
/// Excluded on Linux for one specific reason: `UdpProxy::run` unconditionally calls
/// `LinuxTproxy::enable_tproxy` there (`capture/udp.rs`), so even a plain `UdpProxy::new` with an
/// explicit remote address needs CAP_NET_ADMIN and a TPROXY rule. Guarding that call to the
/// transparent path would let both tests run on Linux — which is the only platform CI blocks on, so
/// today plain UDP forwarding has no coverage where it counts.
#[tokio::test]
async fn test_udp_proxy_drop_all_interceptor() {
    // Bind echo server
    let echo = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    let echo_shutdown_rx = {
        let echo = Arc::new(echo);
        let echo2 = echo.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                tokio::select! {
                    result = echo2.recv_from(&mut buf) => {
                        if let Ok((len, addr)) = result {
                            let _ = echo2.send_to(&buf[..len], addr).await;
                        }
                    }
                    _ = rx.changed() => break,
                }
            }
        });
        tx
    };

    // Bind proxy on a separate socket
    let proxy_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_sock.local_addr().unwrap();
    let interceptor = Arc::new(DropAllInterceptor);
    let proxy = UdpProxy::new(proxy_sock, std::time::Duration::from_secs(5))
        .with_remote(echo_addr)
        .with_interceptor(interceptor);

    let (tx, mut rx) = mpsc::channel::<FlowUpdate>(10);
    tokio::spawn(async move {
        let _ = proxy.run(tx).await;
    });

    // Send a packet through the proxy
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"hello", proxy_addr).await.unwrap();

    // Wait and verify no flow was emitted (interceptor dropped it)
    let timeout = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
    assert!(
        timeout.is_err() || matches!(timeout, Ok(None)),
        "expected no flow from dropped UDP session"
    );

    let _ = echo_shutdown_rx.send(false);
}

/// Verify that a UDP proxy without interceptor forwards packets normally.
///
/// Same Linux exclusion and same reason as above: this exercises the explicit-`remote_addr` path,
/// which is not the transparent one, yet `run` still enables TPROXY on Linux.
#[tokio::test]
async fn test_udp_proxy_allow_all() {
    let echo = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    let echo = Arc::new(echo);
    let echo2 = echo.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            tokio::select! {
                result = echo2.recv_from(&mut buf) => {
                    if let Ok((len, addr)) = result {
                        let _ = echo2.send_to(&buf[..len], addr).await;
                    }
                }
                _ = stop_rx.changed() => break,
            }
        }
    });

    let proxy_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_sock.local_addr().unwrap();
    let proxy = UdpProxy::new(proxy_sock, std::time::Duration::from_secs(5)).with_remote(echo_addr);

    let (tx, mut rx) = mpsc::channel::<FlowUpdate>(10);
    tokio::spawn(async move {
        let _ = proxy.run(tx).await;
    });

    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"hello", proxy_addr).await.unwrap();

    // A datagram session declares that it began, and then publishes its flow. The declaration comes
    // first on purpose — it is what the runtime turns into `FlowEvent::Started`, and a consumer that
    // may only see the first few updates must learn the session started rather than infer it.
    //
    // This runs on every platform now: a configured destination needs no kernel privilege.
    let mut declared_started = false;
    let mut saw_flow = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && !saw_flow {
        let Ok(Some(update)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        else {
            continue;
        };
        match update {
            FlowUpdate::Stage { stage, .. } => {
                assert_eq!(
                    stage,
                    relay_core_api::flow::ExchangeStage::Started,
                    "a datagram session declares started, and nothing else: UDP has no headers, so                      there is no header stage to declare"
                );
                declared_started = true;
            }
            FlowUpdate::Full(flow) => {
                assert!(
                    matches!(flow.layer, relay_core_api::flow::Layer::Udp(_)),
                    "the emitted flow must be a UDP layer flow"
                );
                assert_eq!(
                    flow.protocol_stack,
                    vec!["udp".to_string()],
                    "the stack records the transport it actually was"
                );
                saw_flow = true;
            }
            _ => {}
        }
    }
    assert!(
        declared_started,
        "the session must declare that it started, not only publish its flow"
    );
    assert!(saw_flow, "expected UDP flow to be emitted");

    let _ = stop_tx.send(false);
}
