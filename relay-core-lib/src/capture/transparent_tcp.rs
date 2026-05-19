use crate::capture::original_dst::OriginalDstProvider;
use crate::capture::source::{CaptureSource, IncomingConnection};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

fn is_recoverable_original_dst_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::Unsupported
            | io::ErrorKind::NotFound
            | io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::InvalidInput
    )
}

pub struct TransparentTcpCaptureSource {
    listener: TcpListener,
    original_dst_provider: Arc<dyn OriginalDstProvider>,
}

impl TransparentTcpCaptureSource {
    pub fn new(listener: TcpListener, original_dst_provider: Arc<dyn OriginalDstProvider>) -> Self {
        Self {
            listener,
            original_dst_provider,
        }
    }
}

impl CaptureSource for TransparentTcpCaptureSource {
    type IO = TcpStream;

    fn accept(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<IncomingConnection<Self::IO>>> + Send + '_>>
    {
        Box::pin(async move {
            let (stream, client_addr) = self.listener.accept().await?;

            // Try to get original destination.
            // Some platforms/modes may return recoverable errors (e.g. unsupported IPv6),
            // in which case we gracefully degrade to "unknown original dst".
            let target_addr = match self.original_dst_provider.get_original_dst(&stream) {
                Ok(target_addr) => target_addr,
                Err(e) if is_recoverable_original_dst_error(&e) => {
                    tracing::debug!("Original destination unavailable, falling back: {}", e);
                    None
                }
                Err(e) => return Err(e.into()),
            };

            Ok(IncomingConnection {
                stream,
                client_addr,
                target_addr,
            })
        })
    }

    fn listen_addrs(&self) -> Vec<SocketAddr> {
        // Return listen addresses from OriginalDstProvider (which knows iptables/nftables targets)
        // or just the listener's local address.
        // Usually transparent proxy listens on 0.0.0.0, but we want to avoid loops to *that* address.
        // But loop detection logic also checks local IPs.

        let mut addrs = self
            .original_dst_provider
            .get_listen_addrs()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if let Ok(addr) = self.listener.local_addr() {
            addrs.insert(addr);
        }
        addrs.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::io;

    struct MockOriginalDstProvider {
        listen_addrs: BTreeSet<SocketAddr>,
        dst_result: io::Result<Option<SocketAddr>>,
    }

    #[async_trait]
    impl OriginalDstProvider for MockOriginalDstProvider {
        fn get_original_dst(&self, _stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
            match &self.dst_result {
                Ok(v) => Ok(*v),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }

        fn get_listen_addrs(&self) -> BTreeSet<SocketAddr> {
            self.listen_addrs.clone()
        }
    }

    #[tokio::test]
    async fn test_listen_addrs_contains_listener_and_is_deduplicated() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listener_addr = listener.local_addr().expect("local_addr");
        let provider_addrs = BTreeSet::from([listener_addr]);
        let provider = Arc::new(MockOriginalDstProvider {
            listen_addrs: provider_addrs,
            dst_result: Ok(None),
        });

        let source = TransparentTcpCaptureSource::new(listener, provider);
        let addrs = source.listen_addrs();

        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0], listener_addr);
    }

    #[tokio::test]
    async fn test_accept_recovers_when_original_dst_is_unsupported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listener_addr = listener.local_addr().expect("local_addr");
        let provider = Arc::new(MockOriginalDstProvider {
            listen_addrs: BTreeSet::new(),
            dst_result: Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ipv6 transparent proxy not implemented",
            )),
        });
        let mut source = TransparentTcpCaptureSource::new(listener, provider);

        let connect_task = tokio::spawn(async move {
            let _ = TcpStream::connect(listener_addr).await;
        });

        let conn = source.accept().await.expect("accept should recover");
        assert!(
            conn.target_addr.is_none(),
            "target_addr should fallback to None"
        );
        let _ = connect_task.await;
    }

    #[tokio::test]
    async fn test_accept_propagates_non_recoverable_original_dst_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listener_addr = listener.local_addr().expect("local_addr");
        let provider = Arc::new(MockOriginalDstProvider {
            listen_addrs: BTreeSet::new(),
            dst_result: Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "permission denied",
            )),
        });
        let mut source = TransparentTcpCaptureSource::new(listener, provider);

        let connect_task = tokio::spawn(async move {
            let _ = TcpStream::connect(listener_addr).await;
        });

        let result = source.accept().await;
        assert!(result.is_err(), "non-recoverable errors must propagate");
        let _ = connect_task.await;
    }
}
