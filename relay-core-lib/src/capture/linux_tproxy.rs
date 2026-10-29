#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::mem;
#[cfg(target_os = "linux")]
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use tokio::net::UdpSocket;

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
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }

            // IP_RECVORIGDSTADDR (20)
            if libc::setsockopt(
                fd,
                libc::SOL_IP,
                libc::IP_RECVORIGDSTADDR,
                &enable as *const _ as *const libc::c_void,
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            ) < 0
            {
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
                }
                SocketAddr::V6(v6) => {
                    let mut sin6: libc::sockaddr_in6 = mem::zeroed();
                    sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                    sin6.sin6_port = v6.port().to_be();
                    std::ptr::copy_nonoverlapping(
                        v6.ip().octets().as_ptr(),
                        sin6.sin6_addr.s6_addr.as_mut_ptr(),
                        16,
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
            ) < 0
            {
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
            ) < 0
            {
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
    pub async fn recv_original_dst(
        socket: &UdpSocket,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, Option<SocketAddr>)> {
        let fd = socket.as_raw_fd();
        socket
            .async_io(tokio::io::Interest::READABLE, || {
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
            })
            .await
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
            Ok(SocketAddr::V6(SocketAddrV6::new(
                ip,
                port,
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unknown address family",
        )),
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
            } else if (*cmsg).cmsg_level == libc::SOL_IPV6
                && (*cmsg).cmsg_type == libc::IPV6_RECVORIGDSTADDR
            {
                let data = libc::CMSG_DATA(cmsg) as *const libc::sockaddr_in6;
                let ip = Ipv6Addr::from((*data).sin6_addr.s6_addr);
                let port = u16::from_be((*data).sin6_port);
                return Some(SocketAddr::V6(SocketAddrV6::new(
                    ip,
                    port,
                    (*data).sin6_flowinfo,
                    (*data).sin6_scope_id,
                )));
            }

            cmsg = libc::CMSG_NXTHDR(msg, cmsg);
        }
    }

    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{parse_orig_dst, sockaddr_to_socket_addr};
    use std::mem;

    /// Build a `msghdr` whose ancillary data is one `cmsg` of `level`/`kind`, filled by `fill`.
    ///
    /// The buffer is heap-allocated and returned alongside the header: moving the `Vec` moves the
    /// handle, not the bytes, so the pointer inside `msg` stays valid.
    fn message(
        level: libc::c_int,
        kind: libc::c_int,
        payload: usize,
        fill: impl FnOnce(*mut libc::c_void),
    ) -> (Vec<u64>, libc::msghdr) {
        let space = unsafe { libc::CMSG_SPACE(payload as u32) } as usize;
        let mut buffer = vec![0u64; space.div_ceil(8)];
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_control = buffer.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&msg);
            assert!(!header.is_null(), "CMSG_FIRSTHDR must find the space");
            (*header).cmsg_level = level;
            (*header).cmsg_type = kind;
            (*header).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
            fill(libc::CMSG_DATA(header) as *mut libc::c_void);
        }
        (buffer, msg)
    }

    fn ipv4_message(octets: [u8; 4], port: u16) -> (Vec<u64>, libc::msghdr) {
        message(
            libc::SOL_IP,
            libc::IP_RECVORIGDSTADDR,
            mem::size_of::<libc::sockaddr_in>(),
            |data| unsafe {
                let addr = data as *mut libc::sockaddr_in;
                addr.write(libc::sockaddr_in {
                    sin_family: libc::AF_INET as libc::sa_family_t,
                    sin_port: port.to_be(),
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(octets),
                    },
                    sin_zero: [0; 8],
                });
            },
        )
    }

    #[test]
    fn an_ipv4_original_destination_is_decoded() {
        let (_buffer, msg) = ipv4_message([10, 1, 2, 3], 8443);
        let parsed = unsafe { parse_orig_dst(&msg) };
        assert_eq!(
            parsed,
            Some("10.1.2.3:8443".parse().expect("expected address")),
            "the original destination must survive the cmsg encoding"
        );
    }

    #[test]
    fn an_ipv6_original_destination_is_decoded() {
        let expected: std::net::SocketAddr = "[2001:db8::1]:9443".parse().expect("address");
        let std::net::SocketAddr::V6(v6) = expected else {
            panic!("expected an IPv6 address");
        };
        let (_buffer, msg) = message(
            libc::SOL_IPV6,
            libc::IPV6_RECVORIGDSTADDR,
            mem::size_of::<libc::sockaddr_in6>(),
            |data| unsafe {
                let addr = data as *mut libc::sockaddr_in6;
                addr.write(libc::sockaddr_in6 {
                    sin6_family: libc::AF_INET6 as libc::sa_family_t,
                    sin6_port: v6.port().to_be(),
                    sin6_flowinfo: 0,
                    sin6_addr: libc::in6_addr {
                        s6_addr: v6.ip().octets(),
                    },
                    sin6_scope_id: 0,
                });
            },
        );
        assert_eq!(unsafe { parse_orig_dst(&msg) }, Some(expected));
    }

    #[test]
    fn a_packet_without_ancillary_data_has_no_original_destination() {
        let msg: libc::msghdr = unsafe { mem::zeroed() };
        assert_eq!(unsafe { parse_orig_dst(&msg) }, None);
    }

    #[test]
    fn an_unrelated_control_message_is_not_mistaken_for_an_original_destination() {
        let (_buffer, msg) = message(libc::SOL_IP, libc::IP_TTL, 4, |data| unsafe {
            (data as *mut i32).write(64);
        });
        assert_eq!(
            unsafe { parse_orig_dst(&msg) },
            None,
            "only IP_RECVORIGDSTADDR / IPV6_RECVORIGDSTADDR carry the destination"
        );
    }

    #[test]
    fn a_sockaddr_storage_decodes_by_family() {
        let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
        unsafe {
            let v4 = &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in);
            v4.sin_family = libc::AF_INET as libc::sa_family_t;
            v4.sin_port = 8080u16.to_be();
            v4.sin_addr = libc::in_addr {
                s_addr: u32::from_ne_bytes([127, 0, 0, 1]),
            };
        }
        assert_eq!(
            unsafe { sockaddr_to_socket_addr(&storage) }.expect("IPv4 storage"),
            "127.0.0.1:8080".parse().expect("address")
        );

        let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
        storage.ss_family = libc::AF_UNIX as libc::sa_family_t;
        let error = unsafe { sockaddr_to_socket_addr(&storage) }
            .expect_err("an unknown family must be reported, not guessed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
