use std::net::SocketAddr;
use tokio::net::TcpStream;
use std::collections::BTreeSet;
use std::io;
use async_trait::async_trait;

/// Platform-agnostic interface for retrieving original destination address
#[async_trait]
pub trait OriginalDstProvider: Send + Sync {
    /// Returns the original destination address if available
    fn get_original_dst(&self, stream: &TcpStream) -> io::Result<Option<SocketAddr>>;
    
    /// Returns all addresses this proxy is listening on (for loop detection)
    fn get_listen_addrs(&self) -> BTreeSet<SocketAddr>;
}

#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
pub use crate::capture::macos_pf::MacOsOriginalDstProvider;

#[cfg(target_os = "windows")]
pub use crate::capture::windows::WindowsOriginalDstProvider;

/// Linux implementation using SO_ORIGINAL_DST
#[cfg(all(target_os = "linux", feature = "transparent-linux"))]
pub struct LinuxOriginalDstProvider {
    listen_addrs: BTreeSet<SocketAddr>,
}

#[cfg(all(target_os = "linux", feature = "transparent-linux"))]
impl LinuxOriginalDstProvider {
    pub fn new(listen_addrs: BTreeSet<SocketAddr>) -> Self {
        Self { listen_addrs }
    }
}

#[cfg(all(target_os = "linux", feature = "transparent-linux"))]
#[async_trait]
impl OriginalDstProvider for LinuxOriginalDstProvider {
    fn get_original_dst(&self, stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        
        unsafe {
            let mut addr: libc::sockaddr_storage = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            
            // SO_ORIGINAL_DST = 80
            // Note: This is specific to IPv4. IPv6 uses IP6T_SO_ORIGINAL_DST (80) with SOL_IPV6.
            if libc::getsockopt(
                fd,
                libc::SOL_IP,
                80,
                &mut addr as *mut _ as *mut libc::c_void,
                &mut len,
            ) != 0 {
                // If failing, return None rather than error to allow fallback
                return Ok(None);
            }
            
            if addr.ss_family as i32 == libc::AF_INET {
                let addr_in = *(&addr as *const _ as *const libc::sockaddr_in);
                // s_addr is network byte order (big endian)
                // from_be is correct because u32::from_be takes BE and converts to native
                // But s_addr is u32 in C (usually), let's be careful.
                // libc::in_addr.s_addr is u32.
                let ip = std::net::Ipv4Addr::from(u32::from_be(addr_in.sin_addr.s_addr));
                let port = u16::from_be(addr_in.sin_port);
                Ok(Some(SocketAddr::new(std::net::IpAddr::V4(ip), port)))
            } else {
                // TODO: Support IPv6 transparent proxy
                // Explicitly return error for IPv6 to avoid silent fallback/failures
                return Err(io::Error::new(io::ErrorKind::Unsupported, "IPv6 transparent proxy not implemented"));
            }
        }
    }

    fn get_listen_addrs(&self) -> BTreeSet<SocketAddr> {
        self.listen_addrs.clone()
    }
}

/// No-op provider for platforms without transparent proxy support or when disabled
pub struct NoOpOriginalDstProvider {
    listen_addrs: BTreeSet<SocketAddr>,
}

impl NoOpOriginalDstProvider {
    pub fn new(listen_addrs: BTreeSet<SocketAddr>) -> Self {
        Self { listen_addrs }
    }
}

#[async_trait]
impl OriginalDstProvider for NoOpOriginalDstProvider {
    fn get_original_dst(&self, _stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
        Ok(None)
    }
    
    fn get_listen_addrs(&self) -> BTreeSet<SocketAddr> {
        self.listen_addrs.clone()
    }
}
