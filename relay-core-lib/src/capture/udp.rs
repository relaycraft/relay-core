use chrono::Utc;
use relay_core_api::flow::{Flow, FlowUpdate, Layer, NetworkInfo, TransportProtocol, UdpLayer};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::interceptor::{InterceptionResult, Interceptor};

#[cfg(target_os = "linux")]
use crate::capture::linux_tproxy::LinuxTproxy;

#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
use crate::capture::macos_pf::MacOsOriginalDstProvider;

use std::sync::atomic::{AtomicUsize, Ordering};

/// Key for UDP session (5-tuple)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UdpSessionKey {
    pub src_ip: IpAddr,
    pub src_port: u16,
    pub dst_ip: IpAddr,
    pub dst_port: u16,
    // Protocol is implicitly UDP
}

impl UdpSessionKey {
    pub fn new(src: SocketAddr, dst: SocketAddr) -> Self {
        Self {
            src_ip: src.ip(),
            src_port: src.port(),
            dst_ip: dst.ip(),
            dst_port: dst.port(),
        }
    }
}

/// UDP Session Metadata
#[derive(Debug, Clone)]
pub struct UdpSession {
    pub flow_id: Uuid,
    pub key: UdpSessionKey,
    pub created_at: Instant,
    pub last_activity: Arc<RwLock<Instant>>,
    pub packet_count: Arc<AtomicUsize>,
    pub bytes_transferred: Arc<AtomicUsize>,
    #[cfg(target_os = "linux")]
    pub upstream_socket: Option<Arc<UdpSocket>>, // Bound to src, connected to dst
    #[cfg(target_os = "linux")]
    pub downstream_socket: Option<Arc<UdpSocket>>, // Bound to dst, connected to src
}

/// Manager for tracking active UDP sessions
pub struct UdpSessionManager {
    sessions: RwLock<HashMap<UdpSessionKey, UdpSession>>,
    idle_timeout: Duration,
}

impl UdpSessionManager {
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            idle_timeout,
        }
    }

    /// Get existing session or create new one
    /// Returns (session, is_new)
    pub async fn get_or_create_session(
        &self,
        src: SocketAddr,
        dst: SocketAddr,
    ) -> std::io::Result<(UdpSession, bool)> {
        let key = UdpSessionKey::new(src, dst);
        // Fast path: read lock
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&key) {
                let mut last = session.last_activity.write().await;
                *last = Instant::now();
                session.packet_count.fetch_add(1, Ordering::Relaxed);
                return Ok((session.clone(), false));
            }
        }

        // Slow path: write lock
        let mut sessions = self.sessions.write().await;
        // Check again
        if let Some(session) = sessions.get(&key) {
            let mut last = session.last_activity.write().await;
            *last = Instant::now();
            session.packet_count.fetch_add(1, Ordering::Relaxed);
            return Ok((session.clone(), false));
        }

        #[cfg(target_os = "linux")]
        let (upstream, downstream) = {
            // Create upstream socket: Bound to src, connect to dst
            let up = LinuxTproxy::create_transparent_udp_socket(src)?;
            up.connect(dst).await?;

            // Create downstream socket: Bound to dst, connect to src
            let down = LinuxTproxy::create_transparent_udp_socket(dst)?;
            down.connect(src).await?;

            (Some(Arc::new(up)), Some(Arc::new(down)))
        };

        // Create new session
        let session = UdpSession {
            flow_id: Uuid::new_v4(),
            key: key.clone(),
            created_at: Instant::now(),
            last_activity: Arc::new(RwLock::new(Instant::now())),
            packet_count: Arc::new(AtomicUsize::new(1)),
            bytes_transferred: Arc::new(AtomicUsize::new(0)),
            #[cfg(target_os = "linux")]
            upstream_socket: upstream,
            #[cfg(target_os = "linux")]
            downstream_socket: downstream,
        };

        // Spawn reverse proxy task (B -> A)
        #[cfg(target_os = "linux")]
        if let (Some(up), Some(down)) = (&session.upstream_socket, &session.downstream_socket) {
            let up_clone = up.clone();
            let down_clone = down.clone();
            let last_activity = session.last_activity.clone();
            let bytes_transferred = session.bytes_transferred.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 65535];
                loop {
                    // Read from upstream (response from Server B)
                    match up_clone.recv(&mut buf).await {
                        Ok(n) => {
                            // Update activity
                            if let Ok(mut last) = last_activity.try_write() {
                                *last = Instant::now();
                            }
                            bytes_transferred.fetch_add(n, Ordering::Relaxed);

                            // Send to downstream (to Client A)
                            if let Err(e) = down_clone.send(&buf[..n]).await {
                                tracing::debug!("UDP downstream send error: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::debug!("UDP upstream recv error: {}", e);
                            break;
                        }
                    }
                }
            });
        }

        sessions.insert(key, session.clone());
        Ok((session, true))
    }

    /// Clean up idle sessions
    pub async fn cleanup_idle_sessions(&self) -> Vec<Uuid> {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        let mut removed_ids = Vec::new();
        let mut keys_to_remove = Vec::new();

        // Identify idle sessions
        for (key, session) in sessions.iter() {
            let last = *session.last_activity.read().await;
            if now.duration_since(last) > self.idle_timeout {
                removed_ids.push(session.flow_id);
                keys_to_remove.push(key.clone());
            }
        }

        // Remove them
        for key in keys_to_remove {
            sessions.remove(&key);
        }

        removed_ids
    }
}

/// UDP Proxy capable of handling multiple sessions
pub struct UdpProxy {
    socket: Arc<UdpSocket>,
    session_manager: Arc<UdpSessionManager>,
    remote_addr: Option<SocketAddr>,
    interceptor: Option<Arc<dyn Interceptor>>,
    #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
    original_dst_provider: Option<Arc<MacOsOriginalDstProvider>>,
}

impl UdpProxy {
    pub fn new(socket: UdpSocket, idle_timeout: Duration) -> Self {
        Self {
            socket: Arc::new(socket),
            session_manager: Arc::new(UdpSessionManager::new(idle_timeout)),
            remote_addr: None,
            interceptor: None,
            #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
            original_dst_provider: None,
        }
    }

    pub fn with_remote(mut self, addr: SocketAddr) -> Self {
        self.remote_addr = Some(addr);
        self
    }

    pub fn with_interceptor(mut self, interceptor: Arc<dyn Interceptor>) -> Self {
        self.interceptor = Some(interceptor);
        self
    }

    #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
    pub fn with_original_dst_provider(mut self, provider: Arc<MacOsOriginalDstProvider>) -> Self {
        self.original_dst_provider = Some(provider);
        self
    }

    async fn check_udp_session(&self, flow: &mut Flow) -> bool {
        if let Some(interceptor) = &self.interceptor {
            match interceptor.on_udp_session(flow).await {
                InterceptionResult::Continue => true,
                InterceptionResult::Drop => {
                    tracing::debug!(
                        "UDP session dropped by interceptor: {}:{} -> {}:{}",
                        flow.network.client_ip,
                        flow.network.client_port,
                        flow.network.server_ip,
                        flow.network.server_port
                    );
                    false
                }
                other => {
                    tracing::warn!("Unexpected UDP intercept result {:?}, allowing", other);
                    true
                }
            }
        } else {
            true
        }
    }

    /// Run the proxy loop
    pub async fn run(&self, on_flow: Sender<FlowUpdate>) -> crate::error::Result<()> {
        let mut buf = [0u8; 65535];

        #[cfg(target_os = "linux")]
        {
            // Enable TPROXY on socket
            LinuxTproxy::enable_tproxy(&self.socket)?;

            loop {
                // Use recv_original_dst
                let (len, src_addr, orig_dst) =
                    match LinuxTproxy::recv_original_dst(&self.socket, &mut buf).await {
                        Ok(res) => res,
                        Err(e) => {
                            tracing::error!("UDP TPROXY recv error: {}", e);
                            continue;
                        }
                    };

                if let Some(dst_addr) = orig_dst {
                    match self
                        .session_manager
                        .get_or_create_session(src_addr, dst_addr)
                        .await
                    {
                        Ok((session, is_new)) => {
                            if is_new {
                                let mut flow = Flow {
                                    id: session.flow_id,
                                    start_time: Utc::now(),
                                    end_time: None,
                                    network: NetworkInfo {
                                        client_ip: src_addr.ip().to_string(),
                                        client_port: src_addr.port(),
                                        server_ip: dst_addr.ip().to_string(),
                                        server_port: dst_addr.port(),
                                        protocol: TransportProtocol::UDP,
                                        tls: false,
                                        tls_version: None,
                                        sni: None,
                                    },
                                    layer: Layer::Udp(UdpLayer {
                                        payload_size: len,
                                        packet_count: 1,
                                    }),
                                    tags: vec![],
                                    meta: HashMap::new(),
                                    resilience_trace: None,
                                    rule_variables: HashMap::new(),
                                    matched_rules: vec![],
                                };
                                if !self.check_udp_session(&mut flow).await {
                                    continue;
                                }
                                if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                                    crate::metrics::inc_flows_dropped();
                                }
                            }

                            // Forward packet logic (A -> B)
                            // Using upstream socket bound to src_addr
                            if let Some(upstream) = &session.upstream_socket {
                                if let Err(e) = upstream.send(&buf[..len]).await {
                                    tracing::debug!("UDP upstream send error: {}", e);
                                } else {
                                    session.bytes_transferred.fetch_add(len, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to create UDP session: {}", e);
                        }
                    }
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let sm = self.session_manager.clone();
            let sock = self.socket.clone();
            let flow_tx = on_flow;
            let proxy_local = sock.local_addr().ok();

            #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
            let pf = self.original_dst_provider.clone();
            let fixed_remote = self.remote_addr;

            if fixed_remote.is_none()
                && cfg!(not(all(target_os = "macos", feature = "transparent-macos")))
            {
                tracing::warn!("UDP proxy started without remote_addr on non-Linux; no forwarding");
                loop {
                    match sock.recv_from(&mut buf).await {
                        Ok((_len, _src_addr)) => {}
                        Err(e) => {
                            tracing::error!("UDP drain recv error: {}", e);
                            continue;
                        }
                    }
                }
            }

            loop {
                let (len, src_addr) = match sock.recv_from(&mut buf).await {
                    Ok(res) => res,
                    Err(e) => {
                        tracing::error!("UDP recv error: {}", e);
                        continue;
                    }
                };

                let dst_addr = match resolve_udp_dst(
                    src_addr,
                    proxy_local.as_ref(),
                    #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
                    pf.as_deref(),
                    fixed_remote,
                ) {
                    Some(addr) => addr,
                    None => continue,
                };

                let (session, is_new) = match sm.get_or_create_session(src_addr, dst_addr).await {
                    Ok(res) => res,
                    Err(e) => {
                        tracing::warn!("Failed to create UDP session: {}", e);
                        continue;
                    }
                };

                if is_new {
                    let mut flow = Flow {
                        id: session.flow_id,
                        start_time: Utc::now(),
                        end_time: None,
                        network: NetworkInfo {
                            client_ip: src_addr.ip().to_string(),
                            client_port: src_addr.port(),
                            server_ip: dst_addr.ip().to_string(),
                            server_port: dst_addr.port(),
                            protocol: TransportProtocol::UDP,
                            tls: false,
                            tls_version: None,
                            sni: None,
                        },
                        layer: Layer::Udp(UdpLayer {
                            payload_size: len,
                            packet_count: 1,
                        }),
                        tags: vec![],
                        meta: HashMap::new(),
                        resilience_trace: None,
                        rule_variables: HashMap::new(),
                        matched_rules: vec![],
                    };
                    if !self.check_udp_session(&mut flow).await {
                        continue;
                    }
                    let _ = flow_tx
                        .try_send(FlowUpdate::Full(Box::new(flow)))
                        .inspect_err(|_| {
                            crate::metrics::inc_flows_dropped();
                        });

                    let sock_clone = sock.clone();
                    let bytes = session.bytes_transferred.clone();
                    let last = session.last_activity.clone();
                    let rmt = dst_addr;
                    tokio::spawn(async move {
                        let mut rbuf = [0u8; 65535];
                        loop {
                            match sock_clone.recv_from(&mut rbuf).await {
                                Ok((n, addr)) => {
                                    if addr == rmt {
                                        let _ = sock_clone.send_to(&rbuf[..n], src_addr).await;
                                        bytes.fetch_add(n, Ordering::Relaxed);
                                        if let Ok(mut la) = last.try_write() {
                                            *la = Instant::now();
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "UDP reverse recv error for {}: {}",
                                        session.flow_id,
                                        e
                                    );
                                    break;
                                }
                            }
                        }
                    });
                }

                match sock.send_to(&buf[..len], dst_addr).await {
                    Ok(_) => {
                        session.bytes_transferred.fetch_add(len, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::debug!("UDP send_to {} error: {}", dst_addr, e);
                    }
                }
            }
        }
    }
}

/// Resolve UDP destination: PF NAT lookup on macOS, fallthrough to fixed remote.
#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
fn resolve_udp_dst(
    src: SocketAddr,
    proxy_local: Option<&std::net::SocketAddr>,
    pf: Option<&MacOsOriginalDstProvider>,
    fixed: Option<SocketAddr>,
) -> Option<SocketAddr> {
    if let (Some(provider), Some(local)) = (pf, proxy_local) {
        match provider.nat_lookup_udp(src, *local) {
            Ok(addr) => return Some(addr),
            Err(e) => {
                if e.raw_os_error() != Some(libc::ENOENT) {
                    tracing::warn!("PF NAT lookup failed for UDP {}: {}", src, e);
                }
            }
        }
    }
    fixed
}

#[cfg(not(all(target_os = "macos", feature = "transparent-macos")))]
fn resolve_udp_dst(
    _src: SocketAddr,
    _proxy_local: Option<&std::net::SocketAddr>,
    _pf: Option<&()>,
    fixed: Option<SocketAddr>,
) -> Option<SocketAddr> {
    fixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interceptor::{
        BoxError, HttpBody, RequestAction, ResponseAction, WebSocketMessageAction,
    };
    use async_trait::async_trait;
    use relay_core_api::flow::{NetworkInfo, TransportProtocol, UdpLayer, WebSocketMessage};

    struct DropAllInterceptor;
    #[async_trait]
    impl Interceptor for DropAllInterceptor {
        async fn on_udp_session(&self, _flow: &mut Flow) -> InterceptionResult {
            InterceptionResult::Drop
        }
        async fn on_request(
            &self,
            _flow: &mut Flow,
            body: HttpBody,
        ) -> Result<RequestAction, BoxError> {
            Ok(RequestAction::Continue(body))
        }
        async fn on_response(
            &self,
            _flow: &mut Flow,
            body: HttpBody,
        ) -> Result<ResponseAction, BoxError> {
            Ok(ResponseAction::Continue(body))
        }
        async fn on_websocket_message(
            &self,
            _flow: &mut Flow,
            msg: WebSocketMessage,
        ) -> Result<WebSocketMessageAction, BoxError> {
            Ok(WebSocketMessageAction::Continue(msg))
        }
    }

    fn make_udp_flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: chrono::Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "10.0.0.1".to_string(),
                client_port: 50000,
                server_ip: "10.0.0.2".to_string(),
                server_port: 53,
                protocol: TransportProtocol::UDP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Udp(UdpLayer {
                payload_size: 100,
                packet_count: 1,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[tokio::test]
    async fn test_udp_interceptor_allows_by_default() {
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy = UdpProxy::new(sock, Duration::from_secs(60));
        assert!(proxy.check_udp_session(&mut make_udp_flow()).await);
    }

    #[tokio::test]
    async fn test_udp_interceptor_drops() {
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy = UdpProxy::new(sock, Duration::from_secs(60))
            .with_interceptor(Arc::new(DropAllInterceptor));
        assert!(!proxy.check_udp_session(&mut make_udp_flow()).await);
    }

    // ── resolve_udp_dst ──

    #[test]
    fn test_resolve_udp_dst_non_macos_returns_fixed() {
        // On non-macOS, resolve_udp_dst should return the fixed addr directly
        let fixed = SocketAddr::from(([127, 0, 0, 1], 8080));
        let result = resolve_udp_dst("127.0.0.1:12345".parse().unwrap(), None, None, Some(fixed));
        assert_eq!(result, Some(fixed));
    }

    #[test]
    fn test_resolve_udp_dst_returns_none_when_all_empty() {
        let result = resolve_udp_dst("127.0.0.1:12345".parse().unwrap(), None, None, None);
        assert_eq!(result, None);
    }

    // ── UDP InterceptionResult fallthrough ──

    struct MockResponseInterceptor;
    #[async_trait]
    impl Interceptor for MockResponseInterceptor {
        async fn on_udp_session(&self, _flow: &mut Flow) -> InterceptionResult {
            InterceptionResult::MockResponse(relay_core_api::flow::HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                timing: relay_core_api::flow::ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            })
        }
        async fn on_request(
            &self,
            _flow: &mut Flow,
            body: HttpBody,
        ) -> Result<RequestAction, BoxError> {
            Ok(RequestAction::Continue(body))
        }
        async fn on_response(
            &self,
            _flow: &mut Flow,
            body: HttpBody,
        ) -> Result<ResponseAction, BoxError> {
            Ok(ResponseAction::Continue(body))
        }
        async fn on_websocket_message(
            &self,
            _flow: &mut Flow,
            msg: WebSocketMessage,
        ) -> Result<WebSocketMessageAction, BoxError> {
            Ok(WebSocketMessageAction::Continue(msg))
        }
    }

    #[tokio::test]
    async fn test_udp_mock_response_fallthrough_continues() {
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy = UdpProxy::new(sock, Duration::from_secs(60))
            .with_interceptor(Arc::new(MockResponseInterceptor));
        // MockResponse is nonsensical for UDP — CompositeIterator treats
        // it as Continue (with a warn log). This test asserts the defensive
        // default: the session is allowed through.
        assert!(proxy.check_udp_session(&mut make_udp_flow()).await);
    }
}
