#[cfg(target_os = "windows")]
use crate::capture::original_dst::OriginalDstProvider;
#[cfg(target_os = "windows")]
use async_trait::async_trait;
#[cfg(target_os = "windows")]
use moka::sync::Cache;
#[cfg(target_os = "windows")]
use std::collections::BTreeSet;
#[cfg(target_os = "windows")]
use std::io;
#[cfg(target_os = "windows")]
use std::net::SocketAddr;
#[cfg(all(target_os = "windows", feature = "transparent-windows"))]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
#[cfg(target_os = "windows")]
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use std::time::Duration;
#[cfg(target_os = "windows")]
use tokio::net::TcpStream;

#[cfg(all(target_os = "windows", feature = "transparent-windows"))]
use etherparse::{IpHeader, PacketHeaders, TransportHeader};
#[cfg(all(target_os = "windows", feature = "transparent-windows"))]
use windivert::{WinDivert, WinDivertFlags, WinDivertLayer};

// Global NAT table: ClientAddr -> OriginalDstAddr
// Key: Source Address (Client)
// Value: Original Destination Address
#[cfg(target_os = "windows")]
static NAT_TABLE: OnceLock<Cache<SocketAddr, SocketAddr>> = OnceLock::new();

#[cfg(target_os = "windows")]
fn get_nat_table() -> &'static Cache<SocketAddr, SocketAddr> {
    NAT_TABLE.get_or_init(|| {
        Cache::builder()
            .time_to_live(Duration::from_secs(60)) // Entries expire after 60s
            .build()
    })
}

#[cfg(target_os = "windows")]
pub struct WindowsOriginalDstProvider {
    listen_addrs: BTreeSet<SocketAddr>,
}

#[cfg(target_os = "windows")]
impl WindowsOriginalDstProvider {
    pub fn new(listen_addrs: BTreeSet<SocketAddr>) -> Self {
        Self { listen_addrs }
    }
}

#[cfg(target_os = "windows")]
#[async_trait]
impl OriginalDstProvider for WindowsOriginalDstProvider {
    fn get_original_dst(&self, stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
        let peer_addr = stream.peer_addr()?;
        if let Some(original_dst) = get_nat_table().get(&peer_addr) {
            return Ok(Some(original_dst));
        }
        // Fallback: Check if we can get it via other means, or return None
        Ok(None)
    }

    fn get_listen_addrs(&self) -> BTreeSet<SocketAddr> {
        self.listen_addrs.clone()
    }
}

/// Starts the WinDivert capture loop to redirect traffic to the proxy.
/// This function should be run in a separate task.
///
/// Arguments:
/// - `filter`: WinDivert filter string (e.g., "outbound and tcp.DstPort == 80 and !loopback")
/// - `proxy_port`: The port where the proxy is listening (e.g., 8080)
#[cfg(all(target_os = "windows", feature = "transparent-windows"))]
pub async fn start_windivert_capture(filter: String, proxy_port: u16) {
    tracing::info!("Starting WinDivert capture with filter: {}", filter);

    // Spawn a blocking task for the WinDivert loop
    let result = tokio::task::spawn_blocking(move || {
        // Layer: Network (to capture IP packets)
        // Priority: 0
        // Flags: 0 (default) or SNIFF if we just want to watch
        // We want to modify, so standard mode.
        let divert =
            match WinDivert::new(&filter, WinDivertLayer::Network, 0, WinDivertFlags::new()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Failed to open WinDivert handle: {:?}", e);
                    return;
                }
            };

        tracing::info!("WinDivert handle opened successfully");

        let mut buf = [0u8; 65535];
        let mut addr = windivert::WinDivertAddress::default();

        loop {
            // Recv packet
            match divert.recv(&mut buf, &mut addr) {
                Ok(len) => {
                    let packet_data = &mut buf[..len];

                    // Parse packet to extract headers
                    match PacketHeaders::from_ip_slice(packet_data) {
                        Ok(headers) => {
                            let mut src_addr: Option<IpAddr> = None;
                            let mut dst_addr: Option<IpAddr> = None;
                            let mut src_port: Option<u16> = None;
                            let mut dst_port: Option<u16> = None;

                            // Extract IP info
                            match headers.ip {
                                Some(IpHeader::Version4(ref ipv4, _)) => {
                                    src_addr = Some(IpAddr::V4(ipv4.source.into()));
                                    dst_addr = Some(IpAddr::V4(ipv4.destination.into()));
                                }
                                Some(IpHeader::Version6(ref ipv6, _)) => {
                                    src_addr = Some(IpAddr::V6(ipv6.source.into()));
                                    dst_addr = Some(IpAddr::V6(ipv6.destination.into()));
                                }
                                None => {}
                            }

                            // Extract TCP info
                            match headers.transport {
                                Some(TransportHeader::Tcp(ref tcp)) => {
                                    src_port = Some(tcp.source_port);
                                    dst_port = Some(tcp.destination_port);
                                }
                                _ => {}
                            }

                            if let (Some(s_ip), Some(d_ip), Some(s_port), Some(d_port)) =
                                (src_addr, dst_addr, src_port, dst_port)
                            {
                                // Construct SocketAddrs
                                let src = SocketAddr::new(s_ip, s_port);
                                let dst = SocketAddr::new(d_ip, d_port);

                                // Store mapping in NAT table
                                get_nat_table().insert(src, dst);

                                // Modify packet: redirect to local proxy
                                let modified = match s_ip {
                                    IpAddr::V4(_) => {
                                        let ip_hdr_len = {
                                            let ihl = (packet_data[0] & 0x0F) as usize;
                                            if ihl < 5 {
                                                continue;
                                            }
                                            ihl * 4
                                        };
                                        if packet_data.len() < ip_hdr_len + 4 {
                                            continue;
                                        }
                                        // Rewrite IP dst to 127.0.0.1
                                        packet_data[16..20].copy_from_slice(&[127, 0, 0, 1]);
                                        // Rewrite TCP dst port to proxy_port (big-endian)
                                        let port_bytes = proxy_port.to_be_bytes();
                                        packet_data[ip_hdr_len + 2..ip_hdr_len + 4]
                                            .copy_from_slice(&port_bytes);
                                        // Zero IP header checksum for recalculation
                                        packet_data[10..12].copy_from_slice(&[0u8; 2]);
                                        true
                                    }
                                    IpAddr::V6(_) => {
                                        if packet_data.len() < 44 {
                                            continue;
                                        }
                                        // Rewrite IP dst to ::1
                                        packet_data[24..40].copy_from_slice(&[
                                            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
                                        ]);
                                        // Rewrite TCP dst port (after IPv6 fixed header)
                                        let port_bytes = proxy_port.to_be_bytes();
                                        packet_data[42..44].copy_from_slice(&port_bytes);
                                        true
                                    }
                                };

                                if modified {
                                    if let Err(e) = divert.calc_checksums(&mut packet_data, 0) {
                                        tracing::warn!("Failed to recalculate checksums: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("Failed to parse packet: {:?}", e);
                        }
                    }

                    // Re-inject packet
                    if let Err(e) = divert.send(packet_data, &addr) {
                        tracing::warn!("Failed to re-inject packet: {:?}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("WinDivert recv failed: {:?}", e);
                }
            }
        }
    });

    if let Err(e) = result.await {
        tracing::error!("WinDivert task failed: {}", e);
    }
}

#[cfg(not(all(target_os = "windows", feature = "transparent-windows")))]
pub async fn start_windivert_capture(_filter: String, _proxy_port: u16) {
    tracing::warn!("WinDivert capture not supported on this platform or feature disabled");
}
