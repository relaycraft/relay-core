use async_trait::async_trait;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::interceptor::HttpBody;
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use relay_core_api::flow::Flow;
use relay_core_api::policy::UpstreamProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

/// Error returned by an outbound connector.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream proxy unreachable: {0}")]
    Unreachable(String),
    #[error("upstream proxy refused CONNECT: status {status}")]
    ConnectRefused { status: u16 },
    #[error("upstream proxy authentication required")]
    AuthRequired,
    #[error("upstream TLS error: {0}")]
    Tls(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Outbound connector — sends a RelayCore-processed HTTP request to the target,
/// optionally through an upstream proxy.
#[async_trait]
pub trait OutboundConnector: Send + Sync {
    async fn send_request(
        &self,
        req: Request<HttpBody>,
        target_host: &str,
        target_port: u16,
        flow: &mut Flow,
    ) -> Result<Response<Incoming>, UpstreamError>;

    /// Returns the upstream proxy URL if this connector routes through one.
    /// Used for circuit breaker keying and status reporting.
    fn upstream_proxy_url(&self) -> Option<&str> {
        None
    }
}

// ── Bypass rule parsing / matching ──────────────────────

/// Pre-parsed bypass rule for CIDR / IP / glob hostname matching.
#[derive(Debug, Clone)]
pub enum BypassRule {
    Cidr(ipnetwork::IpNetwork),
    Ip(IpAddr),
    Glob(glob::Pattern),
}

impl BypassRule {
    /// Parse a raw bypass string.
    ///
    /// Supported formats:
    /// - `cidr:10.0.0.0/8`  → CIDR network
    /// - `127.0.0.1`        → literal IP
    /// - `*.internal.corp`  → glob hostname
    pub fn parse(raw: &str) -> Result<Self, String> {
        if let Some(cidr) = raw.strip_prefix("cidr:") {
            let net: ipnetwork::IpNetwork = cidr
                .parse()
                .map_err(|e| format!("invalid CIDR '{}': {}", cidr, e))?;
            return Ok(Self::Cidr(net));
        }
        if let Ok(ip) = raw.parse::<IpAddr>() {
            return Ok(Self::Ip(ip));
        }
        glob::Pattern::new(raw)
            .map(Self::Glob)
            .map_err(|e| format!("invalid glob '{}': {}", raw, e))
    }

    /// Returns true when `hostname` (a host name or IP string) should bypass upstream.
    pub fn matches_host(&self, hostname: &str) -> bool {
        match self {
            Self::Cidr(net) => hostname.parse::<IpAddr>().is_ok_and(|ip| net.contains(ip)),
            Self::Ip(ip) => hostname.parse::<IpAddr>().is_ok_and(|parsed| parsed == *ip),
            Self::Glob(p) => p.matches(hostname),
        }
    }

    /// Returns true when `addr` (a raw IP) should bypass upstream (transparent mode).
    pub fn matches_ip(&self, addr: &IpAddr) -> bool {
        match self {
            Self::Cidr(net) => net.contains(*addr),
            Self::Ip(ip) => ip == addr,
            Self::Glob(_) => false,
        }
    }
}

/// Computes the `Proxy-Authorization` header value from upstream auth config.
pub fn upstream_proxy_authorization(upstream: &UpstreamProxyConfig) -> Option<String> {
    upstream.auth.as_ref().map(|a| {
        let creds = format!(
            "{}:{}",
            a.username,
            secrecy::ExposeSecret::expose_secret(&a.password)
        );
        format!("Basic {}", data_encoding::BASE64.encode(creds.as_bytes()))
    })
}

/// Determines whether a given host should bypass the upstream proxy.
pub fn should_bypass(upstream: &UpstreamProxyConfig, host: &str, ip: Option<IpAddr>) -> bool {
    let rules: Vec<BypassRule> = upstream
        .bypass_hosts
        .iter()
        .filter_map(|r| match BypassRule::parse(r) {
            Ok(rule) => Some(rule),
            Err(e) => {
                tracing::warn!("invalid upstream bypass entry '{}': {}", r, e);
                None
            }
        })
        .collect();

    // Match by hostname first
    if rules.iter().any(|r| r.matches_host(host)) {
        return true;
    }

    // Transparent mode: also match by raw IP
    if let Some(addr) = ip
        && rules.iter().any(|r| r.matches_ip(&addr))
    {
        return true;
    }

    false
}

// ── Direct Connector ────────────────────────────────────

use crate::proxy::http_utils::HttpsClient;
use hyper_rustls::ConfigBuilderExt;

/// Direct outbound connector (no upstream proxy).
pub struct DirectConnector {
    client: Arc<HttpsClient>,
}

impl DirectConnector {
    pub fn new(client: Arc<HttpsClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl OutboundConnector for DirectConnector {
    async fn send_request(
        &self,
        req: Request<HttpBody>,
        _target_host: &str,
        _target_port: u16,
        _flow: &mut Flow,
    ) -> Result<Response<Incoming>, UpstreamError> {
        self.client
            .request(req)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))
    }
}

// ── HTTP Upstream Connector ─────────────────────────────

/// Connects through an HTTP upstream proxy (no TLS to the proxy itself).
pub struct HttpUpstreamConnector {
    proxy_url: String,
    proxy_addr: SocketAddr,
    proxy_authorization: Option<String>,
    tls_client_config: Arc<rustls::ClientConfig>,
}

impl HttpUpstreamConnector {
    pub async fn new(config: &UpstreamProxyConfig) -> Result<Self, UpstreamError> {
        let url = Url::parse(&config.proxy_url)
            .map_err(|e| UpstreamError::Unreachable(format!("invalid proxy URL: {}", e)))?;
        let host = url
            .host_str()
            .ok_or_else(|| UpstreamError::Unreachable("proxy URL missing host".into()))?;
        let port = url.port_or_known_default().unwrap_or(8080);

        let addr = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| UpstreamError::Unreachable(format!("DNS resolution failed: {}", e)))?
            .next()
            .ok_or_else(|| UpstreamError::Unreachable("no address resolved".into()))?;

        let proxy_auth = upstream_proxy_authorization(config);

        let tls_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_native_roots()
                .map_err(|e| UpstreamError::Tls(e.to_string()))?
                .with_no_client_auth(),
        );

        Ok(Self {
            proxy_url: config.proxy_url.clone(),
            proxy_addr: addr,
            proxy_authorization: proxy_auth,
            tls_client_config: tls_config,
        })
    }

    /// Send a CONNECT request through an async stream and return the HTTP response status.
    async fn send_connect_inner<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        stream: &mut S,
        host: &str,
        port: u16,
        proxy_auth: Option<&str>,
    ) -> Result<u16, UpstreamError> {
        let mut req = format!(
            "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n",
            host, port, host, port
        );
        if let Some(auth) = proxy_auth {
            req.push_str(&format!("Proxy-Authorization: {}\r\n", auth));
        }
        req.push_str("\r\n");

        stream
            .write_all(req.as_bytes())
            .await
            .map_err(UpstreamError::Io)?;
        stream.flush().await.map_err(UpstreamError::Io)?;

        // Read response line (buffered, up to 512 bytes)
        let mut buf = [0u8; 512];
        let mut pos = 0;
        loop {
            if pos >= buf.len() {
                return Err(UpstreamError::ConnectRefused { status: 0 });
            }
            let n = stream
                .read(&mut buf[pos..pos + 1])
                .await
                .map_err(UpstreamError::Io)?;
            if n == 0 {
                return Err(UpstreamError::Unreachable(
                    "connection closed during CONNECT handshake".into(),
                ));
            }
            pos += 1;
            if pos >= 2 && buf[pos - 2..pos] == [b'\r', b'\n'] {
                break;
            }
        }

        let status_line = String::from_utf8_lossy(&buf[..pos]).trim().to_string();
        let parts: Vec<&str> = status_line.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(UpstreamError::ConnectRefused { status: 0 });
        }
        let status: u16 = parts[1]
            .parse()
            .map_err(|_| UpstreamError::ConnectRefused { status: 0 })?;

        // Consume remaining headers until \r\n\r\n
        let mut header_buf = [0u8; 4096];
        let mut total = 0;
        loop {
            let n = stream
                .read(&mut header_buf[total..])
                .await
                .map_err(UpstreamError::Io)?;
            if n == 0 {
                return Err(UpstreamError::Unreachable(
                    "connection closed during CONNECT response".into(),
                ));
            }
            total += n;
            if total >= 4 && header_buf[total - 4..total] == [b'\r', b'\n', b'\r', b'\n'] {
                break;
            }
            if total >= header_buf.len() {
                break; // headers too large, assume end
            }
        }

        Ok(status)
    }

    /// Establish TLS to a target through the given stream.
    async fn tls_to_target(
        config: Arc<rustls::ClientConfig>,
        stream: TcpStream,
        target_host: &str,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>, UpstreamError> {
        let connector = tokio_rustls::TlsConnector::from(config);
        let server_name = rustls::pki_types::ServerName::try_from(target_host.to_string())
            .map_err(|e| UpstreamError::Tls(format!("invalid server name: {}", e)))?;
        connector
            .connect(server_name, stream)
            .await
            .map_err(|e| UpstreamError::Tls(e.to_string()))
    }
}

#[async_trait]
impl OutboundConnector for HttpUpstreamConnector {
    async fn send_request(
        &self,
        req: Request<HttpBody>,
        target_host: &str,
        target_port: u16,
        _flow: &mut Flow,
    ) -> Result<Response<Incoming>, UpstreamError> {
        let uri_scheme = req.uri().scheme_str().unwrap_or("http");

        if uri_scheme == "https" {
            return self
                .send_request_connect(req, target_host, target_port)
                .await;
        }

        self.send_request_absolute_uri(req, target_host, target_port)
            .await
    }

    fn upstream_proxy_url(&self) -> Option<&str> {
        Some(&self.proxy_url)
    }
}

impl HttpUpstreamConnector {
    /// Send an HTTP request with absolute URI through the upstream proxy (HTTP target).
    async fn send_request_absolute_uri(
        &self,
        req: Request<HttpBody>,
        target_host: &str,
        target_port: u16,
    ) -> Result<Response<Incoming>, UpstreamError> {
        let (parts, body) = req.into_parts();
        let path = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let target_url = format!("http://{}:{}{}", target_host, target_port, path);
        let mut req_builder = Request::builder()
            .method(parts.method)
            .uri(&target_url)
            .version(parts.version);
        for (name, value) in &parts.headers {
            if crate::proxy::http_utils::is_hop_by_hop(name.as_str()) {
                continue;
            }
            req_builder = req_builder.header(name, value);
        }
        let req = req_builder
            .body(body)
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;

        let stream = TcpStream::connect(self.proxy_addr)
            .await
            .map_err(|e| UpstreamError::Unreachable(format!("TCP connect to upstream: {}", e)))?;
        let io = TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("upstream http1 connection error: {}", e);
            }
        });

        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        Ok(resp)
    }

    /// Send an HTTPS request through the upstream proxy via CONNECT tunnel.
    async fn send_request_connect(
        &self,
        req: Request<HttpBody>,
        target_host: &str,
        target_port: u16,
    ) -> Result<Response<Incoming>, UpstreamError> {
        let mut stream = TcpStream::connect(self.proxy_addr)
            .await
            .map_err(|e| UpstreamError::Unreachable(format!("TCP connect to upstream: {}", e)))?;

        // 1. Send CONNECT to upstream proxy
        let status = Self::send_connect_inner(
            &mut stream,
            target_host,
            target_port,
            self.proxy_authorization.as_deref(),
        )
        .await?;

        if !(200..300).contains(&status) {
            return Err(UpstreamError::ConnectRefused { status });
        }

        // 2. Establish TLS to target through the tunnel
        let tls_stream =
            Self::tls_to_target(self.tls_client_config.clone(), stream, target_host).await?;
        let io = TokioIo::new(tls_stream);

        // 3. Send the actual HTTP request over TLS
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("upstream tunnel http1 connection error: {}", e);
            }
        });

        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        Ok(resp)
    }
}

// ── HTTPS Upstream Connector ────────────────────────────

/// Connects through an HTTPS upstream proxy (TLS to the proxy first).
pub struct HttpsUpstreamConnector {
    proxy_url: String,
    proxy_addr: SocketAddr,
    proxy_host: String,
    proxy_authorization: Option<String>,
    tls_client_config: Arc<rustls::ClientConfig>,
}

impl HttpsUpstreamConnector {
    pub async fn new(config: &UpstreamProxyConfig) -> Result<Self, UpstreamError> {
        let url = Url::parse(&config.proxy_url)
            .map_err(|e| UpstreamError::Unreachable(format!("invalid proxy URL: {}", e)))?;
        let host = url
            .host_str()
            .ok_or_else(|| UpstreamError::Unreachable("proxy URL missing host".into()))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(443);

        let addr = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| UpstreamError::Unreachable(format!("DNS resolution failed: {}", e)))?
            .next()
            .ok_or_else(|| UpstreamError::Unreachable("no address resolved".into()))?;

        let proxy_auth = upstream_proxy_authorization(config);

        let tls_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_native_roots()
                .map_err(|e| UpstreamError::Tls(e.to_string()))?
                .with_no_client_auth(),
        );

        Ok(Self {
            proxy_url: config.proxy_url.clone(),
            proxy_addr: addr,
            proxy_host: host,
            proxy_authorization: proxy_auth,
            tls_client_config: tls_config,
        })
    }
}

#[async_trait]
impl OutboundConnector for HttpsUpstreamConnector {
    async fn send_request(
        &self,
        req: Request<HttpBody>,
        target_host: &str,
        target_port: u16,
        _flow: &mut Flow,
    ) -> Result<Response<Incoming>, UpstreamError> {
        let connector = tokio_rustls::TlsConnector::from(self.tls_client_config.clone());
        let proxy_server_name = rustls::pki_types::ServerName::try_from(self.proxy_host.clone())
            .map_err(|e| UpstreamError::Tls(format!("invalid proxy server name: {}", e)))?;

        // 1. TCP connect to upstream proxy
        let stream = TcpStream::connect(self.proxy_addr)
            .await
            .map_err(|e| UpstreamError::Unreachable(format!("TCP connect to upstream: {}", e)))?;

        // 2. TLS handshake to upstream proxy
        let mut proxy_tls = connector
            .connect(proxy_server_name.clone(), stream)
            .await
            .map_err(|e| UpstreamError::Tls(e.to_string()))?;

        // 3. Send CONNECT inside the TLS tunnel
        let status = HttpUpstreamConnector::send_connect_inner(
            &mut proxy_tls,
            target_host,
            target_port,
            self.proxy_authorization.as_deref(),
        )
        .await?;

        if !(200..300).contains(&status) {
            return Err(UpstreamError::ConnectRefused { status });
        }

        // 4. TLS to target through the proxy's TLS tunnel (TLS-in-TLS)
        let target_server_name =
            rustls::pki_types::ServerName::try_from(target_host.to_string())
                .map_err(|e| UpstreamError::Tls(format!("invalid target server name: {}", e)))?;
        let target_tls = connector
            .connect(target_server_name, proxy_tls)
            .await
            .map_err(|e| UpstreamError::Tls(e.to_string()))?;
        let io = TokioIo::new(target_tls);

        // 5. Send the actual HTTP request
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("https-upstream tunnel http1 connection error: {}", e);
            }
        });

        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| UpstreamError::Io(std::io::Error::other(e)))?;
        Ok(resp)
    }

    fn upstream_proxy_url(&self) -> Option<&str> {
        Some(&self.proxy_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn bypass_rule_parse_cidr() {
        let rule = BypassRule::parse("cidr:10.0.0.0/8").unwrap();
        assert!(matches!(rule, BypassRule::Cidr(_)));
    }

    #[test]
    fn bypass_rule_parse_ip_literal() {
        let rule = BypassRule::parse("127.0.0.1").unwrap();
        assert!(matches!(rule, BypassRule::Ip(_)));
    }

    #[test]
    fn bypass_rule_parse_glob() {
        let rule = BypassRule::parse("*.internal.corp").unwrap();
        assert!(matches!(rule, BypassRule::Glob(_)));
    }

    #[test]
    fn bypass_rule_parse_invalid() {
        assert!(BypassRule::parse("cidr:not-a-cidr").is_err());
    }

    #[test]
    fn bypass_rule_cidr_matches_host() {
        let rule = BypassRule::parse("cidr:10.0.0.0/8").unwrap();
        assert!(rule.matches_host("10.1.2.3"));
        assert!(!rule.matches_host("192.168.1.1"));
        assert!(!rule.matches_host("example.com"));
    }

    #[test]
    fn bypass_rule_ip_matches_host() {
        let rule = BypassRule::parse("127.0.0.1").unwrap();
        assert!(rule.matches_host("127.0.0.1"));
        assert!(!rule.matches_host("127.0.0.2"));
    }

    #[test]
    fn bypass_rule_glob_matches_host() {
        let rule = BypassRule::parse("*.internal.corp").unwrap();
        assert!(rule.matches_host("svc.internal.corp"));
        assert!(rule.matches_host("foo.bar.internal.corp"));
        assert!(!rule.matches_host("external.corp"));
        assert!(!rule.matches_host("10.0.0.1"));
    }

    #[test]
    fn bypass_rule_cidr_matches_ip() {
        let rule = BypassRule::parse("cidr:10.0.0.0/8").unwrap();
        assert!(rule.matches_ip(&IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
        assert!(!rule.matches_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    }

    #[test]
    fn bypass_rule_glob_never_matches_ip() {
        let rule = BypassRule::parse("*.example.com").unwrap();
        assert!(!rule.matches_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn upstream_error_display() {
        let e = UpstreamError::ConnectRefused { status: 403 };
        assert!(e.to_string().contains("403"));
    }
}
