use std::net::SocketAddr;
use std::future::Future;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

/// Represents an incoming connection from any source (TCP Listener, eBPF, TUN, etc.)
pub struct IncomingConnection<IO> {
    pub stream: IO,
    pub client_addr: SocketAddr,
    /// Original destination address (if known via TPROXY/eBPF/NAT lookup)
    pub target_addr: Option<SocketAddr>, 
}

/// Trait for capturing traffic.
/// This abstracts away the difference between a simple TCP Listener, 
/// a Transparent Proxy (TPROXY), or a TUN interface with a user-space stack.
pub trait CaptureSource {
    type IO: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Accept the next connection from the source
    #[allow(clippy::type_complexity)]
    fn accept(&mut self) -> Pin<Box<dyn Future<Output = crate::error::Result<IncomingConnection<Self::IO>>> + Send + '_>>;

    /// Returns the addresses this source is listening on (for loop detection)
    fn listen_addrs(&self) -> Vec<SocketAddr> {
        vec![]
    }
}

// Implement CaptureSource for Tokio TcpListener (Standard Explicit Proxy)
use tokio::net::TcpListener;

pub struct TcpCaptureSource {
    listener: TcpListener,
}

impl TcpCaptureSource {
    pub fn new(listener: TcpListener) -> Self {
        Self { listener }
    }
}

impl CaptureSource for TcpCaptureSource {
    type IO = tokio::net::TcpStream;

    fn accept(&mut self) -> Pin<Box<dyn Future<Output = crate::error::Result<IncomingConnection<Self::IO>>> + Send + '_>> {
        Box::pin(async move {
            let (stream, client_addr) = self.listener.accept().await?;
            Ok(IncomingConnection {
                stream,
                client_addr,
                target_addr: None, // Standard Proxy doesn't know target until parsing HTTP
            })
        })
    }

    fn listen_addrs(&self) -> Vec<SocketAddr> {
        if let Ok(addr) = self.listener.local_addr() {
            vec![addr]
        } else {
            vec![]
        }
    }
}
