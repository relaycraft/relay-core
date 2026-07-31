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
#[cfg(not(target_os = "linux"))]
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
/// Only works on non-Linux because Linux requires TPROXY (iptables) setup
/// for the proxy socket; the explicit remote_addr path is macOS/Windows only.
#[cfg(not(target_os = "linux"))]
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

    // Expect a Full flow update
    let update = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    assert!(
        update.is_ok() && update.unwrap().is_some(),
        "expected UDP flow to be emitted"
    );

    let _ = stop_tx.send(false);
}
