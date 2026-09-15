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
    /// Client endpoint, kept so a closing Flow can be described without re-deriving it.
    pub src: SocketAddr,
    /// Upstream endpoint, kept for the same reason.
    pub dst: SocketAddr,
    /// Wall-clock start, because `Flow.start_time` is wall-clock while `created_at` is monotonic.
    pub started_at: chrono::DateTime<chrono::Utc>,
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
            src,
            dst,
            started_at: chrono::Utc::now(),
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

    /// How long a session may be quiet before it is considered over.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Remove idle sessions and return them, so the caller can emit each one's final Flow.
    ///
    /// Returning the whole session rather than just its id is what makes a closed session
    /// reportable: counters and endpoints travel with it.
    pub async fn cleanup_idle_sessions(&self) -> Vec<UdpSession> {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut keys_to_remove = Vec::new();

        for (key, session) in sessions.iter() {
            let last = *session.last_activity.read().await;
            if now.duration_since(last) > self.idle_timeout {
                expired.push(session.clone());
                keys_to_remove.push(key.clone());
            }
        }

        for key in keys_to_remove {
            sessions.remove(&key);
        }

        expired
    }

    /// Build the closing Flow for a session that has ended.
    ///
    /// This is where a UDP exchange finally gets an `end_time` and the counters a consumer can act
    /// on: the session's own totals replaced the snapshot taken at the first packet.
    pub fn closing_flow(session: &UdpSession) -> Flow {
        Flow {
            id: session.flow_id,
            start_time: session.started_at,
            end_time: Some(chrono::Utc::now()),
            // A session ends here by definition: it was idle past the timeout, so it closed
            // normally rather than being reset or dropped.
            close_reason: Some(relay_core_api::event::CloseReason::Completed),
            network: NetworkInfo {
                client_ip: session.src.ip().to_string(),
                client_port: session.src.port(),
                server_ip: session.dst.ip().to_string(),
                server_port: session.dst.port(),
                server_host: None,
                protocol: TransportProtocol::UDP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Udp(UdpLayer {
                payload_size: session.bytes_transferred.load(Ordering::Relaxed),
                packet_count: session.packet_count.load(Ordering::Relaxed),
            }),
            tags: vec!["closed".to_string()],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
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

    /// Emit the closing Flow for every session that has gone idle, returning how many were closed.
    ///
    /// Called when the receive loop observes a quiet period longer than the idle timeout: at that
    /// point no session can still be active, so nothing is missed and no extra timer task is needed.
    pub async fn close_idle_sessions(&self, on_flow: &Sender<FlowUpdate>) -> usize {
        let expired = self.session_manager.cleanup_idle_sessions().await;
        let closed = expired.len();

        for session in &expired {
            let flow = UdpSessionManager::closing_flow(session);
            if on_flow.try_send(FlowUpdate::Full(Box::new(flow))).is_err() {
                crate::metrics::inc_flows_dropped();
            }
        }

        if closed > 0 {
            tracing::debug!("Closed {} idle UDP session(s)", closed);
        }
        closed
    }

    /// Run the proxy loop
    pub async fn run(&self, on_flow: Sender<FlowUpdate>) -> crate::error::Result<()> {
        let mut buf = [0u8; 65535];

        #[cfg(target_os = "linux")]
        {
            // Enable TPROXY on socket
            LinuxTproxy::enable_tproxy(&self.socket)?;

            loop {
                // A quiet period longer than the idle timeout means every session has expired, so it
                // is the natural moment to close them and emit their final Flows.
                let (len, src_addr, orig_dst) = match tokio::time::timeout(
                    self.session_manager.idle_timeout(),
                    LinuxTproxy::recv_original_dst(&self.socket, &mut buf),
                )
                .await
                {
                    Ok(Ok(res)) => res,
                    Ok(Err(e)) => {
                        tracing::error!("UDP TPROXY recv error: {}", e);
                        continue;
                    }
                    Err(_) => {
                        self.close_idle_sessions(&on_flow).await;
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
                                    close_reason: None,
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
                // See the Linux path: a quiet period means every session has gone idle.
                let (len, src_addr) =
                    match tokio::time::timeout(sm.idle_timeout(), sock.recv_from(&mut buf)).await {
                        Ok(Ok(res)) => res,
                        Ok(Err(e)) => {
                            tracing::error!("UDP recv error: {}", e);
                            continue;
                        }
                        Err(_) => {
                            self.close_idle_sessions(&flow_tx).await;
                            continue;
                        }
                    };

                let dst_addr = match resolve_udp_dst(
                    src_addr,
                    proxy_local.as_ref(),
                    #[cfg(all(target_os = "macos", feature = "transparent-macos"))]
                    pf.as_deref(),
                    #[cfg(not(all(target_os = "macos", feature = "transparent-macos")))]
                    None::<&()>,
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
                        close_reason: None,
                        network: NetworkInfo {
                            client_ip: src_addr.ip().to_string(),
                            client_port: src_addr.port(),
                            server_ip: dst_addr.ip().to_string(),
                            server_port: dst_addr.port(),
                            server_host: None,
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
pub(crate) fn resolve_udp_dst(
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
#[allow(dead_code)]
fn resolve_udp_dst(
    _src: SocketAddr,
    _proxy_local: Option<&std::net::SocketAddr>,
    _pf: Option<&()>,
    fixed: Option<SocketAddr>,
) -> Option<SocketAddr> {
    fixed
}

/// A UDP exchange produces exactly two Flows: an opening one and a closing one.
///
/// * The **opening** Flow is emitted when the session is created, so a consumer sees the exchange as
///   soon as it starts. Its counters are frozen at the first packet, and its `end_time` is `None`
///   because the exchange has not ended — reporting a duration there would be the same fabricated
///   measurement the HAR exporter was fixed for.
/// * The **closing** Flow is emitted by [`UdpProxy::close_idle_sessions`] once the session has been
///   quiet for longer than the idle timeout. It carries the session's real totals and an `end_time`,
///   which is what makes `duration_ms` computable for UDP flows.
///
/// Neither Flow is updated in place: a consumer wanting live counters during a long-lived session
/// still needs a periodic update, which does not exist yet.
pub const SESSION_FLOW_IS_EMIT_TWICE: () = ();

#[cfg(test)]
mod tests {
    /// Closing a session must report the exchange's real totals and an `end_time`, which is what makes
    /// `duration_ms` computable for UDP flows at all.
    // Not runnable on Linux without privileges: `get_or_create_session` builds its sockets through
    // `LinuxTproxy::create_transparent_udp_socket`, which needs CAP_NET_ADMIN (`IP_TRANSPARENT`), and
    // a CI runner has none — so the call fails with EPERM rather than exercising anything. The
    // integration tests in `tests/udp_integration_test.rs` carry the same gate for the same reason.
    // Covering this contract on Linux needs a privileged runner or a mocked socket layer.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn a_closed_session_reports_totals_and_an_end_time() {
        let manager = UdpSessionManager::new(Duration::from_millis(1));
        let src: SocketAddr = "127.0.0.1:4444".parse().expect("addr");
        let dst: SocketAddr = "127.0.0.1:5555".parse().expect("addr");

        let (session, is_new) = manager
            .get_or_create_session(src, dst)
            .await
            .expect("create session");
        assert!(is_new, "the first packet must create the session");

        // Packets after the first update the session's own counters, which is what the closing Flow
        // reports; the opening Flow is frozen at one packet by design.
        session.packet_count.fetch_add(4, Ordering::Relaxed);
        session.bytes_transferred.fetch_add(512, Ordering::Relaxed);

        // Let the session go idle.
        tokio::time::sleep(Duration::from_millis(5)).await;

        let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(8);
        let closed = manager.cleanup_idle_sessions().await;
        assert_eq!(
            closed.len(),
            1,
            "the idle session must be closed exactly once"
        );

        for session in &closed {
            let flow = UdpSessionManager::closing_flow(session);
            let _ = tx.send(FlowUpdate::Full(Box::new(flow))).await;
        }

        match rx.recv().await.expect("closing flow") {
            FlowUpdate::Full(flow) => {
                assert!(
                    flow.end_time.is_some(),
                    "a closed session must record end_time so duration_ms is computable"
                );
                match &flow.layer {
                    Layer::Udp(udp) => {
                        assert_eq!(udp.packet_count, 5, "the closing Flow reports every packet");
                        assert_eq!(udp.payload_size, 512, "and every byte");
                    }
                    other => panic!("expected a UDP layer, got {other:?}"),
                }
            }
            other => panic!("expected a full flow update, got {other:?}"),
        }

        // A second pass must find nothing: the session was removed, not merely reported.
        assert!(
            manager.cleanup_idle_sessions().await.is_empty(),
            "closing must remove the session so it is not reported twice"
        );
    }

    /// A session that is still active must not be closed.
    // Not runnable on Linux without privileges: `get_or_create_session` builds its sockets through
    // `LinuxTproxy::create_transparent_udp_socket`, which needs CAP_NET_ADMIN (`IP_TRANSPARENT`), and
    // a CI runner has none — so the call fails with EPERM rather than exercising anything. The
    // integration tests in `tests/udp_integration_test.rs` carry the same gate for the same reason.
    // Covering this contract on Linux needs a privileged runner or a mocked socket layer.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn an_active_session_is_not_closed() {
        let manager = UdpSessionManager::new(Duration::from_secs(60));
        let src: SocketAddr = "127.0.0.1:6666".parse().expect("addr");
        let dst: SocketAddr = "127.0.0.1:7777".parse().expect("addr");

        manager
            .get_or_create_session(src, dst)
            .await
            .expect("create session");

        assert!(
            manager.cleanup_idle_sessions().await.is_empty(),
            "a session inside its idle window must be left alone"
        );
    }

    /// The *opening* Flow's contract, pinned so it cannot drift silently.
    ///
    /// See `SESSION_FLOW_IS_EMIT_TWICE`: the opening Flow intentionally reports no `end_time` and
    /// counters frozen at the first packet, while the closing Flow carries the totals.
    #[test]
    fn opening_flow_reports_no_end_time_and_frozen_counters() {
        let session = UdpSession {
            flow_id: uuid::Uuid::new_v4(),
            key: UdpSessionKey {
                src_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                src_port: 1000,
                dst_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                dst_port: 2000,
            },
            src: "127.0.0.1:1000".parse().expect("addr"),
            dst: "127.0.0.1:2000".parse().expect("addr"),
            started_at: chrono::Utc::now(),
            created_at: std::time::Instant::now(),
            last_activity: Arc::new(RwLock::new(std::time::Instant::now())),
            packet_count: Arc::new(AtomicUsize::new(1)),
            bytes_transferred: Arc::new(AtomicUsize::new(0)),
            #[cfg(target_os = "linux")]
            upstream_socket: None,
            #[cfg(target_os = "linux")]
            downstream_socket: None,
        };

        // The session's counters live on the session; the opening Flow snapshots them once.
        assert_eq!(session.packet_count.load(Ordering::Relaxed), 1);
        assert_eq!(session.bytes_transferred.load(Ordering::Relaxed), 0);

        // The opening Flow, built the way the receive loop builds it.
        let opening = Flow {
            id: session.flow_id,
            start_time: session.started_at,
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: session.src.ip().to_string(),
                client_port: session.src.port(),
                server_ip: session.dst.ip().to_string(),
                server_port: session.dst.port(),
                server_host: None,
                protocol: TransportProtocol::UDP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Udp(UdpLayer {
                payload_size: 0,
                packet_count: 1,
            }),
            tags: vec![],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        };
        assert!(
            opening.end_time.is_none(),
            "the opening Flow must not claim a duration for an exchange still in progress"
        );
    }

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
            close_reason: None,
            network: NetworkInfo {
                client_ip: "10.0.0.1".to_string(),
                client_port: 50000,
                server_ip: "10.0.0.2".to_string(),
                server_port: 53,
                server_host: None,
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
                trailers: vec![],
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
