use crate::interceptor::Interceptor;
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::http::handle_http_request;
use crate::proxy::http_utils::ExchangeOrigin;
use crate::proxy::outbound::OutboundConnector;
use crate::tls::CertificateAuthority;
use hyper::service::service_fn;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc::Sender, watch};
use tokio_rustls::TlsAcceptor;
use tracing::{error, info};
use url::Url;

use crate::capture::loop_detection::LoopDetector;

/// What the client is speaking inside a CONNECT tunnel.
///
/// A CONNECT tunnel is not necessarily TLS. gRPC over plaintext (`h2c`) reaches an HTTP proxy as
/// CONNECT followed by an HTTP/2 preface, which is how clients configured with `HTTP_PROXY` behave
/// for an insecure target. The old tunnel assumed TLS unconditionally, so such a session failed its
/// handshake and was never captured — the same blind spot mitmproxy has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunnelProtocol {
    /// A TLS record; terminate it and serve decrypted HTTP inside.
    Tls,
    /// Plaintext HTTP/1.x.
    PlaintextHttp1,
    /// Plaintext HTTP/2 by prior knowledge (the connection preface).
    PlaintextH2,
}

/// Longest prefix needed to classify: the HTTP/2 client preface.
const SNIFF_LEN: usize = 24;

/// Classify the first bytes the client sends. Anything that is not TLS or an HTTP/2 preface is
/// treated as plaintext HTTP/1.x, because a tunnel to a plaintext origin carries that too.
fn classify_tunnel_preface(prefix: &[u8]) -> TunnelProtocol {
    // A TLS record always starts with the handshake content type and a TLS major version.
    if prefix.len() >= 2 && prefix[0] == 0x16 && prefix[1] == 0x03 {
        return TunnelProtocol::Tls;
    }
    if prefix.starts_with(b"PRI * HTTP/2.0") {
        return TunnelProtocol::PlaintextH2;
    }
    TunnelProtocol::PlaintextHttp1
}

/// Can a decision be made yet? TLS needs two bytes; the H2 preface needs all of it.
fn preface_is_decidable(prefix: &[u8]) -> bool {
    if prefix.len() >= 2 && prefix[0] == 0x16 {
        return true;
    }
    // Not TLS as far as we can tell: either we have the whole HTML/2 preface, or enough bytes to
    // rule it out, so HTTP/1.x is decidable from its first byte.
    !b"PRI * HTTP/2.0".starts_with(prefix) || prefix.len() >= SNIFF_LEN
}

/// A stream that yields already-read bytes before the underlying stream.
///
/// The tunnel has to read the client's first bytes to decide what it is speaking, but those bytes
/// belong to the protocol, so they cannot be dropped: this hands them back in order to whichever
/// server implementation takes over.
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    offset: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.offset < self.prefix.len() {
            let remaining = self.prefix[self.offset..].to_vec();
            let take = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..take]);
            self.offset += take;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_tunnel(
    upgraded: Upgraded,
    host: String,
    client_addr: SocketAddr,
    origin: ExchangeOrigin,
    ca: Arc<CertificateAuthority>,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
) -> crate::error::Result<()> {
    // Read the client's first bytes before deciding anything: a CONNECT tunnel is not necessarily
    // TLS, and assuming it was meant an h2c session (CONNECT followed by an HTTP/2 preface, which is
    // what a client configured with HTTP_PROXY does for a plaintext gRPC target) failed its
    // handshake and was never captured.
    let (prefix, io) = match read_tunnel_preface(TokioIo::new(upgraded)).await {
        Ok(result) => result,
        Err(e) => {
            error!("Tunnel: could not read the client preface: {}", e);
            return Ok(());
        }
    };

    // The bytes already read belong to the protocol, so whichever path takes over gets them back
    // first — including TLS, whose ClientHello was partially consumed by the sniff.
    let stream = PrefixedStream::new(prefix.clone(), io);

    match classify_tunnel_preface(&prefix) {
        TunnelProtocol::Tls => {
            info!("Starting MITM tunnel for {}", host);
            serve_tls_tunnel(
                stream,
                host,
                client_addr,
                origin,
                ca,
                on_flow,
                connector,
                interceptor,
                policy_rx,
                target_addr,
                loop_detector,
                circuit_breaker,
            )
            .await
        }
        protocol => {
            let scheme = "http";
            info!(
                "Starting plaintext tunnel for {} ({:?} inside CONNECT)",
                host, protocol
            );
            serve_plaintext_tunnel(
                stream,
                host,
                scheme,
                client_addr,
                origin,
                on_flow,
                connector,
                interceptor,
                policy_rx,
                target_addr,
                loop_detector,
                circuit_breaker,
            )
            .await
        }
    }
}

/// Splice a CONNECT tunnel to its target without terminating TLS.
///
/// Nothing inside is parsed, recorded, or rewritten: the client's handshake reaches the upstream and
/// the client validates the upstream certificate. That is the only mode in which a certificate-pinned
/// client can work, and the reason the policy exists (roadmap §8).
///
/// The CONNECT flow itself is still recorded by the caller, tagged `tls-passthrough`, so the capture
/// can answer "this tunnel existed and was not inspected" instead of showing nothing at all.
pub async fn handle_passthrough_tunnel(
    upgraded: Upgraded,
    host: String,
    connector: Arc<dyn OutboundConnector>,
) -> crate::error::Result<()> {
    let (hostname, port) = crate::proxy::tls::parse_authority(&host).ok_or_else(|| {
        crate::error::RelayError::Proxy(format!("invalid CONNECT authority '{host}'"))
    })?;

    let mut upstream = connector
        .open_tunnel(&hostname, port)
        .await
        .map_err(|e| crate::error::RelayError::Proxy(format!("passthrough {host}: {e}")))?;
    let mut client = TokioIo::new(upgraded);

    // `copy_bidirectional` returns when both directions are done, including the half-close case
    // where one peer shuts down its write side but still expects to read. It is the whole job: no
    // sniffing, no framing, no idle timeout — an idle tunnel is a legitimate tunnel.
    let (client_to_upstream, upstream_to_client) =
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .map_err(crate::error::RelayError::Io)?;

    tracing::debug!(
        "passthrough tunnel {host} closed after {client_to_upstream} bytes up / {upstream_to_client} bytes down"
    );
    Ok(())
}

/// Read the client's first bytes without losing them.
///
/// Returns the bytes read so far and the stream, so the caller can hand both to whichever server
/// implementation the preface selects.
async fn read_tunnel_preface(
    mut io: TokioIo<Upgraded>,
) -> io::Result<(Vec<u8>, TokioIo<Upgraded>)> {
    use tokio::io::AsyncReadExt;

    let mut prefix = Vec::with_capacity(SNIFF_LEN);
    let mut buf = [0u8; SNIFF_LEN];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while !preface_is_decidable(&prefix) && prefix.len() < SNIFF_LEN {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no preface arrived within the tunnel timeout",
            ));
        }
        match tokio::time::timeout(remaining, io.read(&mut buf[prefix.len()..])).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => prefix.extend_from_slice(&buf[prefix.len()..prefix.len() + n]),
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no preface arrived within the tunnel timeout",
                ));
            }
        }
    }

    Ok((prefix, io))
}

/// Terminate TLS and serve the decrypted stream as HTTP.
#[allow(clippy::too_many_arguments)]
async fn serve_tls_tunnel(
    stream: PrefixedStream<TokioIo<Upgraded>>,
    host: String,
    client_addr: SocketAddr,
    origin: ExchangeOrigin,
    ca: Arc<CertificateAuthority>,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
) -> crate::error::Result<()> {
    // 1. Get cached server config for `host` (generates on miss)
    // Strip the port for certificate generation (SNI / CN). The authority parser handles the
    // bracketed IPv6 form; splitting on ':' used to turn `[::1]:443` into `[`.
    let hostname = crate::proxy::tls::parse_authority(&host)
        .map(|(host, _)| host)
        .unwrap_or_else(|| host.clone());
    let server_config = ca.gen_server_config(&hostname).await?;

    // 2. Configure Server TLS
    // ALPN protocols (h2, http/1.1) are already configured in gen_server_config
    let tls_acceptor = TlsAcceptor::from(server_config);

    // 3. Accept TLS connection from client
    let tls_stream =
        match tokio::time::timeout(Duration::from_secs(10), tls_acceptor.accept(stream)).await {
            Ok(res) => res?,
            Err(_) => {
                return Err(crate::error::RelayError::Proxy(
                    "TLS handshake timeout".into(),
                ));
            }
        };

    // The handshake itself is the only honest source for these three facts. They are copied onto
    // every flow the tunnel produces, because a flow is what a consumer reads: recording them only
    // in a log would leave the API unable to answer "which SNI did this client send?".
    let handshake = {
        let (_, connection) = tls_stream.get_ref();
        crate::proxy::tls::TlsHandshake {
            sni: connection.server_name().map(str::to_string),
            alpn: connection
                .alpn_protocol()
                .map(|protocol| String::from_utf8_lossy(protocol).into_owned()),
            version: connection
                .protocol_version()
                .map(crate::proxy::tls::protocol_version_name),
        }
    };
    info!(
        "MITM tunnel for {} negotiated {:?} / {:?} (SNI {:?})",
        host, handshake.version, handshake.alpn, handshake.sni
    );

    let tls_io = TokioIo::new(tls_stream);

    if let Err(err) = tunnel_builder()
        .serve_connection(
            tls_io,
            service_fn(request_service(
                host,
                "https",
                true,
                client_addr,
                origin,
                on_flow,
                connector,
                interceptor,
                policy_rx,
                target_addr,
                loop_detector,
                circuit_breaker,
                Some(handshake),
            )),
        )
        .await
    {
        error!("MITM Tunnel Error: {:?}", err);
    }

    Ok(())
}

/// Serve a CONNECT tunnel whose contents are plaintext, without terminating anything.
///
/// This covers both plaintext HTTP/1.x through CONNECT and `h2c` (prior-knowledge HTTP/2), which is
/// how a gRPC client with `HTTP_PROXY` reaches an insecure target.
#[allow(clippy::too_many_arguments)]
async fn serve_plaintext_tunnel(
    io: PrefixedStream<TokioIo<Upgraded>>,
    host: String,
    scheme: &'static str,
    client_addr: SocketAddr,
    origin: ExchangeOrigin,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
) -> crate::error::Result<()> {
    // hyper's server wants its own IO traits; `TokioIo` bridges our tokio-based stream back.
    if let Err(err) = tunnel_builder()
        .serve_connection(
            TokioIo::new(io),
            service_fn(request_service(
                host,
                scheme,
                // No TLS was terminated, so the Flow must not claim there was any.
                false,
                client_addr,
                origin,
                on_flow,
                connector,
                interceptor,
                policy_rx,
                target_addr,
                loop_detector,
                circuit_breaker,
                None,
            )),
        )
        .await
    {
        error!("Plaintext Tunnel Error: {:?}", err);
    }

    Ok(())
}

/// The HTTP server configuration shared by both tunnel flavours.
fn tunnel_builder() -> auto::Builder<hyper_util::rt::TokioExecutor> {
    let mut builder = auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10));
    builder
        .http2()
        .timer(hyper_util::rt::TokioTimer::new())
        .initial_stream_window_size(2 * 1024 * 1024) // 2MB
        .initial_connection_window_size(4 * 1024 * 1024) // 4MB
        .max_concurrent_streams(200)
        .max_header_list_size(65536) // 64KB
        .keep_alive_interval(std::time::Duration::from_secs(20))
        .keep_alive_timeout(std::time::Duration::from_secs(10));
    builder
}

/// The boxed future one tunnelled request resolves to.
type TunnelFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    hyper::Response<crate::interceptor::HttpBody>,
                    std::convert::Infallible,
                >,
            > + Send,
    >,
>;

/// One request inside a tunnel, with an absolute URL reconstructed from the CONNECT target.
///
/// Inside a tunnel the request target is origin-form (`/path`), so the authority has to come from
/// the CONNECT line. `scheme` and `is_mitm` are separate because a plaintext tunnel has an
/// `http://` target and no TLS to report.
#[allow(clippy::too_many_arguments)]
fn request_service(
    host: String,
    scheme: &'static str,
    is_mitm: bool,
    client_addr: SocketAddr,
    origin: ExchangeOrigin,
    on_flow: Sender<FlowUpdate>,
    connector: Arc<dyn OutboundConnector>,
    interceptor: Arc<dyn Interceptor>,
    policy_rx: watch::Receiver<ProxyPolicy>,
    target_addr: Option<SocketAddr>,
    loop_detector: Arc<LoopDetector>,
    circuit_breaker: Arc<CircuitBreaker>,
    tls: Option<crate::proxy::tls::TlsHandshake>,
) -> impl Fn(hyper::Request<hyper::body::Incoming>) -> TunnelFuture + Clone + Send + 'static {
    move |req| {
        let host = host.clone();
        let origin = origin.clone();
        let on_flow = on_flow.clone();
        let connector = connector.clone();
        let interceptor = interceptor.clone();
        let policy_rx = policy_rx.clone();
        let loop_detector = loop_detector.clone();
        let circuit_breaker = circuit_breaker.clone();
        let tls = tls.clone();

        Box::pin(async move {
            // Inside the tunnel the request target is origin-form (`GET /`), so the authority has to
            // come from the CONNECT line. `scheme` is the tunnel's, not the client's: a plaintext
            // tunnel carries `http://` and terminated nothing.
            let mut req = req;
            let path = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/");
            let uri_string = format!("{}://{}{}", scheme, host, path);
            if let Ok(new_uri) = Url::parse(&uri_string)
                && let Ok(uri) = new_uri.as_str().parse()
            {
                *req.uri_mut() = uri;
            }

            handle_http_request(
                req,
                client_addr,
                origin,
                on_flow,
                connector,
                interceptor,
                is_mitm,
                policy_rx,
                target_addr,
                loop_detector,
                circuit_breaker,
                tls,
            )
            .await
        })
    }
}
