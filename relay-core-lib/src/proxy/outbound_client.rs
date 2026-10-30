//! The outbound HTTP client's transport: TCP + TLS with measurement and certificate capture.
//!
//! hyper-rustls owns both steps internally, so nothing outside it can say how long the connect or
//! the handshake took, nor which chain the peer presented. This module replaces that connector with
//! an equivalent one that measures both phases and carries the facts on the connection metadata,
//! which hyper copies into every response's extensions.
//!
//! Scope: the direct path (target over TLS). The parent-proxy connectors build their own TLS
//! sessions; their timing is not attached to a target flow, because the handshake belongs to the
//! proxy, not the destination.

use crate::interceptor::HttpBody;
use data_encoding::BASE64;
use hyper::Uri;
use hyper::rt::{Read, Write};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tower_service::Service;

/// The client type every outbound HTTP exchange uses. Its connector is [`TlsTransport`].
pub type HttpsClient = Client<TlsTransport, HttpBody>;

/// What a completed outbound handshake established, attached to the connection.
///
/// `connected_at` is what makes the metadata honest under connection reuse: hyper copies the
/// connection's `Connected` into **every** response, so a pooled request would otherwise inherit the
/// timing and chain of a handshake that happened long before it. A response whose connection was
/// established before the request started must report nothing.
#[derive(Debug, Clone)]
pub struct HandshakeMeta {
    /// When the connection finished establishing (TCP + TLS).
    pub connected_at: Instant,
    /// TCP connect duration.
    pub connect_ms: u64,
    /// TLS handshake duration; `None` when the target was plaintext.
    pub ssl_ms: Option<u64>,
    /// The peer's certificate chain, leaf first, base64-encoded DER.
    pub certificates: Vec<String>,
}

/// A connector that speaks TCP and (for `https`) TLS, recording how long each step took.
#[derive(Clone)]
pub struct TlsTransport {
    config: Arc<ClientConfig>,
}

impl TlsTransport {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl Service<Uri> for TlsTransport {
    type Response = OutboundConnection;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let config = self.config.clone();
        Box::pin(async move { open(uri, config).await })
    }
}

async fn open(uri: Uri, config: Arc<ClientConfig>) -> Result<OutboundConnection, BoxError> {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = uri
        .host()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "URI has no host"))?
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });

    let connect_start = Instant::now();
    let tcp = dial(&host, port).await?;
    let connect_ms = connect_start.elapsed().as_millis() as u64;

    if scheme != "https" {
        return Ok(OutboundConnection {
            io: TokioIo::new(Box::new(tcp) as Box<dyn AsyncReadWrite>),
            h2: false,
            meta: HandshakeMeta {
                connected_at: Instant::now(),
                connect_ms,
                ssl_ms: None,
                certificates: Vec::new(),
            },
        });
    }

    let server_name = ServerName::try_from(host)?;
    let ssl_start = Instant::now();
    let stream = TlsConnector::from(config)
        .connect(server_name, tcp)
        .await
        .map_err(|e| Box::new(e) as BoxError)?;
    let ssl_ms = ssl_start.elapsed().as_millis() as u64;

    let (_, connection) = stream.get_ref();
    let h2 = connection.alpn_protocol() == Some(b"h2");
    let certificates = connection
        .peer_certificates()
        .map(|chain| {
            chain
                .iter()
                .map(|certificate| BASE64.encode(certificate.as_ref()))
                .collect()
        })
        .unwrap_or_default();

    Ok(OutboundConnection {
        io: TokioIo::new(Box::new(stream) as Box<dyn AsyncReadWrite>),
        h2,
        meta: HandshakeMeta {
            connected_at: Instant::now(),
            connect_ms,
            ssl_ms: Some(ssl_ms),
            certificates,
        },
    })
}

/// Try every resolved address, not only the first: `localhost` often resolves to `::1` before
/// `127.0.0.1`, and a test or a dual-stack host may only listen on one of them.
async fn dial(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("no address resolved for {host}:{port}"),
        ));
    }

    let mut last_error = None;
    for address in addresses {
        match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("connect to {address} timed out"),
                ))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("no address accepted a connection for {host}:{port}"),
        )
    }))
}

/// A connected transport, plus the facts hyper will copy into the response.
pub struct OutboundConnection {
    io: TokioIo<Box<dyn AsyncReadWrite>>,
    h2: bool,
    meta: HandshakeMeta,
}

impl Read for OutboundConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl Write for OutboundConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }
}

impl Connection for OutboundConnection {
    fn connected(&self) -> Connected {
        let connected = Connected::new();
        let connected = if self.h2 {
            connected.negotiated_h2()
        } else {
            connected
        };
        connected.extra(self.meta.clone())
    }
}

/// Both traits are needed because the TLS and plaintext transports are stored behind one box.
trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Build a config that trusts exactly `roots` and advertises both HTTP versions over ALPN.
pub fn config_with_roots(roots: RootCertStore) -> ClientConfig {
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}
