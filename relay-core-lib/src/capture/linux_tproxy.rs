#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use tokio::net::UdpSocket;
#[cfg(target_os = "linux")]
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6, Ipv4Addr, Ipv6Addr};
#[cfg(target_os = "linux")]
use std::mem;

#[cfg(target_os = "linux")]
pub struct LinuxTproxy;

#[cfg(target_os = "linux")]
impl LinuxTproxy {
    /// Enable IP_TRANSPARENT and IP_RECVORIGDSTADDR on the socket
    pub fn enable_tproxy(socket: &UdpSocket) -> io::Result<()> {
        let fd = socket.as_raw_fd();
        unsafe {
            let enable: libc::c_int = 1;
            
            // IP_TRANSPARENT (19)
            if libc::setsockopt(
                fd,
                libc::SOL_IP,
                libc::IP_TRANSPARENT,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            ) < 0 {
                return Err(io::Error::last_os_error());
            }

            // IP_RECVORIGDSTADDR (20)
            if libc::setsockopt(
                fd,
                libc::SOL_IP,
                libc::IP_RECVORIGDSTADDR,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            ) < 0 {
                return Err(io::Error::last_os_error());
            }
            
            // IPV6_TRANSPARENT (75)
            // We ignore errors here as IPv6 might be disabled
            let _ = libc::setsockopt(
                fd,
                libc::SOL_IPV6,
                libc::IPV6_TRANSPARENT,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // IPV6_RECVORIGDSTADDR (74)
            let _ = libc::setsockopt(
                fd,
                libc::SOL_IPV6,
                libc::IPV6_RECVORIGDSTADDR,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        Ok(())
    }

    /// Create a UDP socket bound to a specific address with IP_TRANSPARENT enabled.
    /// This allows binding to non-local addresses (spoofing source IP).
    pub fn create_transparent_udp_socket(addr: SocketAddr) -> io::Result<UdpSocket> {
        use std::os::unix::io::FromRawFd;

        unsafe {
            let (domain, sockaddr, socklen) = match addr {
                SocketAddr::V4(v4) => {
                    let mut sin: libc::sockaddr_in = mem::zeroed();
                    sin.sin_family = libc::AF_INET as libc::sa_family_t;
                    sin.sin_port = v4.port().to_be();
                    sin.sin_addr.s_addr = u32::from(*v4.ip()).to_be();
                    (
                        libc::AF_INET,
                        &sin as *const _ as *const libc::sockaddr,
                        mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    )
                },
                SocketAddr::V6(v6) => {
                     let mut sin6: libc::sockaddr_in6 = mem::zeroed();
                     sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                     sin6.sin6_port = v6.port().to_be();
                     std::ptr::copy_nonoverlapping(
                         v6.ip().octets().as_ptr(), 
                         sin6.sin6_addr.s6_addr.as_mut_ptr(), 
                         16
                     );
                     
                     (
                         libc::AF_INET6, 
                         &sin6 as *const _ as *const libc::sockaddr, 
                         mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                     )
                }
            };

            let fd = libc::socket(domain, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            
            // Helper to close fd on error
            let close_fd = |fd: libc::c_int| {
                libc::close(fd);
            };

            // Enable IP_TRANSPARENT
            let enable: libc::c_int = 1;
            if libc::setsockopt(
                fd,
                libc::SOL_IP,
                libc::IP_TRANSPARENT,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            ) < 0 {
                 let err = io::Error::last_os_error();
                 close_fd(fd);
                 return Err(err);
            }
            
            if domain == libc::AF_INET6 {
                 // Ignore IPv6 errors for now as it might not be supported
                 let _ = libc::setsockopt(
                    fd,
                    libc::SOL_IPV6,
                    libc::IPV6_TRANSPARENT,
                    &enable as *const _ as *const libc::c_void,
                    mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }

            // SO_REUSEADDR
            if libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            ) < 0 {
                 let err = io::Error::last_os_error();
                 close_fd(fd);
                 return Err(err);
            }
            
            // Bind
            if libc::bind(fd, sockaddr, socklen) < 0 {
                 let err = io::Error::last_os_error();
                 close_fd(fd);
                 return Err(err);
            }
            
            // Non-blocking (Tokio needs this)
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                 let err = io::Error::last_os_error();
                 close_fd(fd);
                 return Err(err);
            }

            let std_socket = std::net::UdpSocket::from_raw_fd(fd);
            UdpSocket::from_std(std_socket)
        }
    }

    /// Receive a packet with original destination address
    /// Returns (bytes_read, source_addr, original_dest_addr)
    pub async fn recv_original_dst(socket: &UdpSocket, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<SocketAddr>)> {
        let fd = socket.as_raw_fd();
        socket.async_io(tokio::io::Interest::READABLE, || {
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            };
            
            // Buffer for control messages (ancillary data)
            // Enough space for IPv4 or IPv6 address
            let mut cmsg_buf = [0u8; 64]; 
            
            // Prepare sockaddr storage for source address
            let mut src_addr: libc::sockaddr_storage = unsafe { mem::zeroed() };
            
            let mut msg = libc::msghdr {
                msg_name: &mut src_addr as *mut _ as *mut libc::c_void,
                msg_namelen: mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
                msg_iov: &mut iov,
                msg_iovlen: 1,
                msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
                msg_controllen: cmsg_buf.len(),
                msg_flags: 0,
            };

            let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
            
            if n < 0 {
                return Err(io::Error::last_os_error());
            }

            let source = unsafe { sockaddr_to_socket_addr(&src_addr)? };
            let orig_dst = unsafe { parse_orig_dst(&msg) };

            Ok((n as usize, source, orig_dst))
        }).await
    }
}

#[cfg(target_os = "linux")]
unsafe fn sockaddr_to_socket_addr(storage: &libc::sockaddr_storage) -> io::Result<SocketAddr> {
    match storage.ss_family as libc::c_int {
        libc::AF_INET => {
            let addr: &libc::sockaddr_in = unsafe { mem::transmute(storage) };
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            let port = u16::from_be(addr.sin_port);
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        }
        libc::AF_INET6 => {
            let addr: &libc::sockaddr_in6 = unsafe { mem::transmute(storage) };
            let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
            let port = u16::from_be(addr.sin6_port);
            Ok(SocketAddr::V6(SocketAddrV6::new(ip, port, addr.sin6_flowinfo, addr.sin6_scope_id)))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "Unknown address family")),
    }
}

#[cfg(target_os = "linux")]
unsafe fn parse_orig_dst(msg: &libc::msghdr) -> Option<SocketAddr> {
    let mut cmsg: *mut libc::cmsghdr = unsafe { libc::CMSG_FIRSTHDR(msg) };
    
    while !cmsg.is_null() {
        unsafe {
            if (*cmsg).cmsg_level == libc::SOL_IP && (*cmsg).cmsg_type == libc::IP_RECVORIGDSTADDR {
                let data = libc::CMSG_DATA(cmsg) as *const libc::sockaddr_in;
                let ip = Ipv4Addr::from(u32::from_be((*data).sin_addr.s_addr));
                let port = u16::from_be((*data).sin_port);
                return Some(SocketAddr::V4(SocketAddrV4::new(ip, port)));
            } else if (*cmsg).cmsg_level == libc::SOL_IPV6 && (*cmsg).cmsg_type == libc::IPV6_RECVORIGDSTADDR {
                 let data = libc::CMSG_DATA(cmsg) as *const libc::sockaddr_in6;
                 let ip = Ipv6Addr::from((*data).sin6_addr.s6_addr);
                 let port = u16::from_be((*data).sin6_port);
                 return Some(SocketAddr::V6(SocketAddrV6::new(ip, port, (*data).sin6_flowinfo, (*data).sin6_scope_id)));
            }
        
            cmsg = libc::CMSG_NXTHDR(msg, cmsg);
        }
    }
    
    None
}
