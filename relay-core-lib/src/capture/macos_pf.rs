use crate::capture::original_dst::OriginalDstProvider;
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::io::AsRawFd;
use tokio::net::TcpStream;
use tracing::warn;

// Constants for PF
const PF_DEV_PATH: &str = "/dev/pf";

// FFI definitions for macOS PF
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
struct pf_addr {
    pub addr: [u32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
struct pfioc_natlook {
    pub saddr: pf_addr,
    pub daddr: pf_addr,
    pub rsaddr: pf_addr,
    pub rdaddr: pf_addr,
    pub sport: u16,
    pub dport: u16,
    pub rsport: u16,
    pub rdport: u16,
    pub af: u8,
    pub proto: u8,
    pub direction: u8,
    pub pad: [u8; 1], // Alignment padding
}

// IOCTL definition
// _IOWR('D', 23, struct pfioc_natlook)
// IOC_INOUT = 0xc0000000
// Size = 76 (0x4c)
// 'D' = 0x44
// 23 = 0x17
// 0xc0000000 | (0x4c << 16) | (0x44 << 8) | 0x17 = 0xc04c4417
const DIOCNATLOOK: u64 = 0xc04c4417;

#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
pub struct MacOsOriginalDstProvider {
    // listen_addrs is not strictly needed for PF lookup but good for filtering
    listen_addrs: BTreeSet<SocketAddr>,
    pf_dev: File,
}

#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
impl MacOsOriginalDstProvider {
    pub fn new(listen_addrs: BTreeSet<SocketAddr>) -> io::Result<Self> {
        let pf_dev = File::open(PF_DEV_PATH)?;
        Ok(Self {
            listen_addrs,
            pf_dev,
        })
    }

    fn nat_lookup(&self, src: SocketAddr, dst: SocketAddr) -> io::Result<SocketAddr> {
        let mut pnl = pfioc_natlook {
            af: match src {
                SocketAddr::V4(_) => libc::AF_INET as u8,
                SocketAddr::V6(_) => libc::AF_INET6 as u8,
            },
            proto: libc::IPPROTO_TCP as u8,
            direction: 1, // PF_IN
            ..Default::default()
        };

        // Fill addresses
        match src {
            SocketAddr::V4(addr) => {
                // addr[0] expects Network Byte Order (Big Endian) in memory.
                // on LE systems, we must swap.
                // addr.ip().to_bits() returns Host Byte Order.
                // .to_be() converts Host -> BE (swaps on LE).
                pnl.saddr.addr[0] = addr.ip().to_bits().to_be();
                pnl.sport = addr.port().to_be();
            }
            SocketAddr::V6(addr) => {
                let octets = addr.ip().octets();
                for i in 0..4 {
                    let val = u32::from_be_bytes([
                        octets[i * 4],
                        octets[i * 4 + 1],
                        octets[i * 4 + 2],
                        octets[i * 4 + 3],
                    ]);
                    pnl.saddr.addr[i] = val.to_be();
                }
                pnl.sport = addr.port().to_be();
            }
        }

        match dst {
            SocketAddr::V4(addr) => {
                pnl.daddr.addr[0] = addr.ip().to_bits().to_be();
                pnl.dport = addr.port().to_be();
            }
            SocketAddr::V6(addr) => {
                let octets = addr.ip().octets();
                for i in 0..4 {
                    let val = u32::from_be_bytes([
                        octets[i * 4],
                        octets[i * 4 + 1],
                        octets[i * 4 + 2],
                        octets[i * 4 + 3],
                    ]);
                    pnl.daddr.addr[i] = val.to_be();
                }
                pnl.dport = addr.port().to_be();
            }
        }

        let fd = self.pf_dev.as_raw_fd();
        let ret = unsafe { libc::ioctl(fd, DIOCNATLOOK, &mut pnl) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // Read result from rdaddr/rdport (Redirect Destination Address/Port)
        // This is the original destination.
        // rdport is in Network Byte Order (BE).
        let port = u16::from_be(pnl.rdport);

        if pnl.af == libc::AF_INET as u8 {
            // rdaddr.addr[0] is in BE. u32::from_be converts it to Host.
            // Ipv4Addr::from(u32) expects Host Order?
            // Wait, Ipv4Addr::from(u32) creates it from host u32.
            // But let's check.
            // Ipv4Addr::new(a,b,c,d).
            // Let's use octets to be safe.
            let val = u32::from_be(pnl.rdaddr.addr[0]);
            let ip = Ipv4Addr::from(val);
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        } else {
            // Reconstruct IPv6
            let mut octets = [0u8; 16];
            for i in 0..4 {
                let val = u32::from_be(pnl.rdaddr.addr[i]);
                let bytes = val.to_be_bytes();
                octets[i * 4] = bytes[0];
                octets[i * 4 + 1] = bytes[1];
                octets[i * 4 + 2] = bytes[2];
                octets[i * 4 + 3] = bytes[3];
            }
            let ip = Ipv6Addr::from(octets);
            Ok(SocketAddr::new(IpAddr::V6(ip), port))
        }
    }
}

#[cfg(all(target_os = "macos", feature = "transparent-macos"))]
#[async_trait]
impl OriginalDstProvider for MacOsOriginalDstProvider {
    fn get_listen_addrs(&self) -> BTreeSet<SocketAddr> {
        self.listen_addrs.clone()
    }

    fn get_original_dst(&self, stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
        let peer_addr = stream.peer_addr()?;
        let local_addr = stream.local_addr()?;

        // Perform NAT lookup
        // Note: For PF rdr-to, we look up the mapping using (saddr, sport, daddr, dport)
        // Here, saddr is peer_addr (client), daddr is local_addr (proxy).
        match self.nat_lookup(peer_addr, local_addr) {
            Ok(addr) => Ok(Some(addr)),
            Err(e) => {
                // ENOENT (2) means no state found, which is common for direct connections
                if e.raw_os_error() == Some(libc::ENOENT) {
                    return Ok(None);
                }
                warn!("PF NAT lookup failed: {}", e);
                Ok(None)
            }
        }
    }
}
