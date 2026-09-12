//! Differential fixtures against mitmproxy (roadmap §15-3, §24.10).
//!
//! The roadmap asks to "compare wire behaviour, not just QPS". A throughput comparison cannot catch
//! a proxy that answers with the wrong `Content-Encoding`, drops a rewritten body, or leaks a
//! framing header — those only show up in the bytes a client receives.
//!
//! Each scenario is applied to the same upstream twice: once proxied by RelayCore and once by
//! mitmproxy, and the two client-visible results are compared. mitmproxy is treated as the reference
//! for *observable* behaviour, which is exactly what the benchmarking document established it does
//! well (see `docs/mitmproxy-policy-benchmark.md`).
//!
//! ## Requirements
//!
//! mitmproxy must be on `PATH`. When it is absent the tests **skip** with a clear message rather
//! than passing silently, and `REQUIRE_MITMPROXY=1` turns a missing mitmproxy into a failure so CI
//! can enforce it where the tool is installed.

use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::io::Read as _;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}

/// What a client observed, in terms that both proxies can be compared on.
#[derive(Debug, PartialEq, Eq)]
struct ClientObservation {
    status: u16,
    content_encoding: Option<String>,
    content_length_matches_body: bool,
    /// Body after applying the declared encoding, so a proxy that re-encodes differently still
    /// compares equal on meaning.
    decoded_body: String,
}

/// Kill the child on drop, including on a panicking assertion.
struct ProxyProcess {
    child: Child,
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn mitmproxy_available() -> Option<String> {
    let path = which("mitmdump")?;
    if std::env::var("REQUIRE_MITMPROXY").is_ok() {
        return Some(path);
    }
    Some(path)
}

fn which(name: &str) -> Option<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then_some(path)
}

/// Fail when mitmproxy is required but absent, otherwise report a skip.
fn skip_or_fail(reason: &str) {
    if std::env::var("REQUIRE_MITMPROXY").is_ok() {
        panic!("mitmproxy is required in this environment but {reason}");
    }
    eprintln!("Skipping differential fixture: {reason}");
}

/// An upstream that can serve gzip-encoded or plain responses on demand.
async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let mut received = Vec::new();
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            received.extend_from_slice(&buf[..n]);
                            if received.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let head = String::from_utf8_lossy(&received);
                let gzip_route = head.starts_with("GET /gzip");

                let plain = b"a-payload-with-plain-text";
                let body = if gzip_route {
                    let mut e =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    std::io::Write::write_all(&mut e, plain).expect("write");
                    e.finish().expect("finish")
                } else {
                    plain.to_vec()
                };

                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if gzip_route {
                    response.push_str("Content-Encoding: gzip\r\n");
                }
                response.push_str("\r\n");

                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
            });
        }
    });

    addr
}

/// Read one HTTP/1.1 response through `proxy_addr` for `path` on `upstream`.
async fn observe_through_proxy(
    proxy_addr: SocketAddr,
    upstream: SocketAddr,
    path: &str,
) -> ClientObservation {
    let mut stream = TcpStream::connect(proxy_addr).await.expect("connect proxy");
    let request = format!(
        "GET http://{upstream}{path} HTTP/1.1\r\nHost: {upstream}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let mut raw = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => raw.extend_from_slice(&buf[..n]),
        }
    }

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response must have a header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw[split + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .expect("status line");

    let header = |name: &str| -> Option<String> {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };

    let content_encoding = header("content-encoding").map(|v| v.to_ascii_lowercase());
    let declared_len = header("content-length").and_then(|v| v.parse::<usize>().ok());

    let decoded_body = match content_encoding.as_deref() {
        Some("gzip") => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&body[..])
                .read_to_end(&mut out)
                .map(|_| String::from_utf8_lossy(&out).to_string())
                .unwrap_or_else(|e| format!("<undecodable gzip: {e}>"))
        }
        _ => String::from_utf8_lossy(&body).to_string(),
    };

    ClientObservation {
        status,
        content_encoding,
        content_length_matches_body: declared_len.map(|l| l == body.len()).unwrap_or(true),
        decoded_body,
    }
}

/// Rewrites the response body as a rule would, so RelayCore and mitmproxy can be compared on the
/// same edit rather than each being checked separately.
struct RewriteResponseBody {
    replacement: &'static str,
}

#[async_trait::async_trait]
impl relay_core_lib::interceptor::Interceptor for RewriteResponseBody {
    async fn on_request_headers(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
    ) -> relay_core_lib::interceptor::InterceptionResult {
        relay_core_lib::interceptor::InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
        body: relay_core_lib::interceptor::HttpBody,
    ) -> Result<relay_core_lib::interceptor::RequestAction, relay_core_lib::interceptor::BoxError>
    {
        Ok(relay_core_lib::interceptor::RequestAction::Continue(body))
    }

    async fn on_response_headers(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
    ) -> relay_core_lib::interceptor::InterceptionResult {
        relay_core_lib::interceptor::InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut relay_core_api::flow::Flow,
        body: relay_core_lib::interceptor::HttpBody,
    ) -> Result<relay_core_lib::interceptor::ResponseAction, relay_core_lib::interceptor::BoxError>
    {
        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: self.replacement.to_string(),
                size: self.replacement.len() as u64,
            });
        }
        Ok(relay_core_lib::interceptor::ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut relay_core_api::flow::Flow,
        message: relay_core_api::flow::WebSocketMessage,
    ) -> Result<
        relay_core_lib::interceptor::WebSocketMessageAction,
        relay_core_lib::interceptor::BoxError,
    > {
        Ok(relay_core_lib::interceptor::WebSocketMessageAction::Continue(message))
    }
}

async fn spawn_relay_core(
    upstream: SocketAddr,
    interceptor: Arc<dyn relay_core_lib::interceptor::Interceptor>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let _ = upstream;
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind relay-core");
    let addr = listener.local_addr().expect("addr");

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, _flow_rx) = tokio::sync::mpsc::channel(64);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    let handle = tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    (addr, handle)
}

/// Start mitmproxy, optionally with an addon.
///
/// An empty `addon` means "no addon": writing a zero-byte file and pointing `--scripts` at it makes
/// mitmproxy fail to load and serve nothing, which previously made the baseline fixtures fail rather
/// than test anything.
fn spawn_mitmproxy(addon: &str) -> Option<(SocketAddr, ProxyProcess)> {
    let port = free_port()?;

    let mut command = Command::new("mitmdump");
    command
        .arg("--quiet")
        .arg("--listen-host")
        .arg("127.0.0.1")
        .arg("--listen-port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if !addon.is_empty() {
        let script = write_addon(addon)?;
        command.arg("--scripts").arg(&script);
    }

    let child = command.spawn().ok()?;

    Some((
        SocketAddr::from(([127, 0, 0, 1], port)),
        ProxyProcess { child },
    ))
}

fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

fn write_addon(body: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("relay-core-differential");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("addon-{}.py", std::process::id()));
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Is the proxy accepting connections?
async fn wait_for_proxy(addr: SocketAddr) -> bool {
    for _ in 0..60 {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Wait until the proxy *behaves* as the fixture expects, not merely until its port is open.
///
/// mitmproxy accepts connections before it has finished loading addons, so a request sent as soon as
/// the port opens can pass through unrewritten. Probing for the expected effect removes that race
/// instead of relying on a sleep long enough to usually win it.
async fn wait_until_rewrite_is_active(addr: SocketAddr, upstream: SocketAddr) -> bool {
    for _ in 0..60 {
        let observed = observe_through_proxy(addr, upstream, "/plain").await;
        if observed.decoded_body == REWRITE_TARGET {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Sanity check for the harness itself: with no rewriting at all, both proxies must agree on a plain
/// response. If this fails, no other comparison in this file means anything.
#[tokio::test]
async fn differential_harness_agrees_on_a_plain_response() {
    init_crypto();

    if mitmproxy_available().is_none() {
        skip_or_fail("mitmdump is not on PATH");
        return;
    }

    let upstream = spawn_upstream().await;
    let (relay_addr, relay_handle) = spawn_relay_core(upstream, Arc::new(NoOpInterceptor {})).await;

    let Some((mitm_addr, _mitm)) = spawn_mitmproxy("") else {
        skip_or_fail("mitmdump could not be started");
        return;
    };
    assert!(
        wait_for_proxy(mitm_addr).await,
        "mitmproxy did not start listening"
    );

    let relay = observe_through_proxy(relay_addr, upstream, "/plain").await;
    let mitm = observe_through_proxy(mitm_addr, upstream, "/plain").await;

    assert_eq!(
        relay.status, 200,
        "relay-core must pass a plain response through"
    );
    assert_eq!(
        relay.decoded_body, mitm.decoded_body,
        "both proxies must deliver the same body"
    );
    assert_eq!(
        relay.content_encoding, mitm.content_encoding,
        "both proxies must report the same encoding for an untouched response"
    );

    relay_handle.abort();
}

/// A gzip response that nobody rewrites must arrive intact through both proxies, with the encoding
/// header still describing the bytes that were sent.
#[tokio::test]
async fn differential_untouched_gzip_matches_mitmproxy() {
    init_crypto();

    if mitmproxy_available().is_none() {
        skip_or_fail("mitmdump is not on PATH");
        return;
    }

    let upstream = spawn_upstream().await;
    let (relay_addr, relay_handle) = spawn_relay_core(upstream, Arc::new(NoOpInterceptor {})).await;

    let Some((mitm_addr, _mitm)) = spawn_mitmproxy("") else {
        skip_or_fail("mitmdump could not be started");
        return;
    };
    assert!(
        wait_for_proxy(mitm_addr).await,
        "mitmproxy did not start listening"
    );

    let relay = observe_through_proxy(relay_addr, upstream, "/gzip").await;
    let mitm = observe_through_proxy(mitm_addr, upstream, "/gzip").await;

    assert_eq!(
        relay.content_encoding.as_deref(),
        Some("gzip"),
        "an untouched gzip response must keep its encoding"
    );
    assert_eq!(
        relay.decoded_body, mitm.decoded_body,
        "both proxies must deliver the same decoded payload"
    );
    assert!(
        relay.content_length_matches_body,
        "a declared content-length must describe the bytes sent"
    );
    assert_eq!(
        relay.content_encoding, mitm.content_encoding,
        "both proxies must agree on the encoding they send"
    );

    relay_handle.abort();
}

/// Addon that rewrites the response body the same way the RelayCore fixture does, so the two
/// implementations can be compared on the bytes a client receives.
/// What the rewriting addon and the RelayCore interceptor both produce.
const REWRITE_TARGET: &str = "REWRITTEN-BY-PROXY";

const REWRITE_ADDON: &str = r#"
from mitmproxy import http

class Rewrite:
    def response(self, flow: http.HTTPFlow) -> None:
        flow.response.text = "REWRITTEN-BY-PROXY"

addons = [Rewrite()]
"#;

/// Both proxies rewriting the same gzip response must produce a client-decodable reply whose header
/// matches the bytes. This is the comparison the roadmap asks for: behaviour, not throughput.
#[tokio::test]
async fn differential_rewritten_gzip_matches_mitmproxy() {
    init_crypto();

    if mitmproxy_available().is_none() {
        skip_or_fail("mitmdump is not on PATH");
        return;
    }

    let upstream = spawn_upstream().await;

    // mitmproxy rewrites through the addon.
    let Some((mitm_addr, _mitm)) = spawn_mitmproxy(REWRITE_ADDON) else {
        skip_or_fail("mitmdump could not be started");
        return;
    };
    assert!(
        wait_for_proxy(mitm_addr).await,
        "mitmproxy did not start listening"
    );
    assert!(
        wait_until_rewrite_is_active(mitm_addr, upstream).await,
        "mitmproxy never applied its addon, so there is nothing to compare against"
    );
    let mitm = observe_through_proxy(mitm_addr, upstream, "/gzip").await;

    // RelayCore performs the same edit through an interceptor.
    let (relay_addr, relay_handle) = spawn_relay_core(
        upstream,
        Arc::new(RewriteResponseBody {
            replacement: "REWRITTEN-BY-PROXY",
        }),
    )
    .await;
    let relay = observe_through_proxy(relay_addr, upstream, "/gzip").await;

    // The two must agree on everything a client can observe.
    assert_eq!(
        relay.status, mitm.status,
        "both proxies must answer with the same status"
    );
    assert_eq!(
        relay.decoded_body, mitm.decoded_body,
        "both proxies must deliver the same rewritten body"
    );
    assert_eq!(
        relay.content_encoding, mitm.content_encoding,
        "both proxies must declare the same encoding after a rewrite"
    );
    assert_eq!(
        relay.content_length_matches_body, mitm.content_length_matches_body,
        "both proxies must keep content-length consistent with the body"
    );
    assert!(
        relay.content_length_matches_body,
        "relay-core must not declare a length that contradicts its body"
    );

    relay_handle.abort();
}
