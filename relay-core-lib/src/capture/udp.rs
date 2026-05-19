#[allow(unused_imports)]
use chrono::Utc;
#[allow(unused_imports)]
use relay_core_api::flow::{Flow, FlowUpdate, Layer, NetworkInfo, TransportProtocol, UdpLayer};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use crate::capture::linux_tproxy::LinuxTproxy;

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
    #[allow(dead_code)]
    session_manager: Arc<UdpSessionManager>,
}

impl UdpProxy {
    pub fn new(socket: UdpSocket, idle_timeout: Duration) -> Self {
        Self {
            socket: Arc::new(socket),
            session_manager: Arc::new(UdpSessionManager::new(idle_timeout)),
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
                                // Create initial flow
                                let flow = Flow {
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
                                };
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
            let _ = on_flow;
            loop {
                let (_len, _src_addr) = self.socket.recv_from(&mut buf).await?;
                // Without TPROXY, we don't know the original destination easily
                // Just consume packets to avoid buffer bloat
            }
        }
    }
}
