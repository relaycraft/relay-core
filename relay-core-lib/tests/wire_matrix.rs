//! Wire-level Action matrix.
//!
//! Purpose (roadmap §3 "建立真实线路 Action Matrix" / §22 "DoD"):
//! assert what the **upstream** and the **client** actually receive on the socket after an
//! interception mutates a flow — not merely that a `Flow` struct field changed.
//!
//! Every case runs the real data plane:
//!
//! ```text
//!   test client ──HTTP/1.1 absolute-URI──▶ RelayCore proxy ──▶ recording upstream
//!        ▲                                                          │
//!        └──────────────── response (echo of what upstream got) ────┘
//! ```
//!
//! The upstream records the exact request head/body bytes it received and replies with them
//! echoed back, so a single round trip yields both halves of the assertion:
//!
//! * `Case.upstream` — what the upstream actually received.
//! * bytes the client actually received — visible to the test as the response body.
//!
//! A capability whose `Flow` mutation does not show up in either assertion is *not* wired,
//! and per roadmap §22 must not be reported as Stable.
//!
//! ## Status of cases
//!
//! Cases that expose a known gap (roadmap §24) are marked `#[ignore]` with the §24 reference so
//! the suite stays green while the gap is open. **Do not delete an assertion to make a case
//! pass** — remove the `#[ignore]` only when the data plane is actually fixed.
//!
//! ## Action coverage inventory
//!
//! The full public action set is `relay-core-api/src/rule.rs` (30 variants). This table is the
//! single tracking point for "has a real wire-level assertion yet". Per roadmap §22 a capability
//! may not be reported `Stable` while its row is `none` or `gap`.
//!
//! | Action | Wire status | Case |
//! |---|---|---|
//! | `Add/Update/DeleteRequestHeader` | **pass** | `wire_matrix_request_header_mutation_reaches_upstream` |
//! | `SetRequestMethod` / `SetRequestUrl` | pass (same rebuild path as request headers) | – |
//! | `SetRequestBody` / `TransformRequestBody` | **gap — §24.1** | `wire_matrix_request_body_mutation_reaches_upstream` |
//! | `Add/Update/DeleteResponseHeader` | **gap — §24.1** | `wire_matrix_response_header_mutation_reaches_client` |
//! | `SetResponseStatus` | gap — §24.1 (same `res_parts` path) | – |
//! | `SetResponseBody` / `TransformResponseBody` | gap — §24.1 | – |
//! | `MockWebSocketMessage` | gap — §24.1 (frame is dropped) | – |
//! | `SetTtl` | gap — unimplemented by design | – |
//! | `MapRemote` (WebSocket handshake) | gap — §24.1 | – |
//! | `ForwardPort` (`target_host`) | gap — §24.1 | – |
//! | `Drop` / `Abort` | covered elsewhere (`udp_integration_test`) | – |
//! | `MapLocal` / `MapRemote` (HTTP) / `Redirect` / `MockResponse` | none yet | – |
//! | `Delay` / `Throttle` / `RateLimit` | none yet | – |
//! | `Tag` / `SetVariable` / `Inspect` | Flow-metadata only, no wire effect by design | – |

use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::flow::{Flow, FlowUpdate, Layer};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::{
    BoxError, CompositeInterceptor, HttpBody, InterceptionResult, Interceptor, RequestAction,
    ResponseAction,
};
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}

/// Which stage a case mutates. Kept explicit so each case states the contract it exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // remaining phases are added as the data plane is fixed.
enum Phase {
    None,
    RequestHeaders,
    RequestBody,
    ResponseHeaders,
    ResponseBody,
}

struct Case {
    /// Stable id used in assertion messages and in the coverage table below.
    id: &'static str,
    phase: Phase,
    request: &'static str,
    body: &'static str,
}

/// Applies a fixed mutation per phase, mirroring the shape of the corresponding rule actions.
struct MutateInterceptor {
    phase: Phase,
}

#[async_trait::async_trait]
impl Interceptor for MutateInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        if self.phase != Phase::RequestHeaders {
            return InterceptionResult::Continue;
        }
        if let Layer::Http(http) = &mut flow.layer {
            http.request
                .headers
                .push(("x-wire-probe".to_string(), "from-interceptor".to_string()));
        }
        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        if self.phase != Phase::RequestBody {
            return Ok(RequestAction::Continue(body));
        }
        // Mirror `Action::SetRequestBody { body: Text(..) }`, which writes
        // `flow.layer.http.request.body` and nothing else.
        if let Layer::Http(http) = &mut flow.layer {
            http.request.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: "REPLACED".to_string(),
                size: "REPLACED".len() as u64,
            });
        }
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        if self.phase != Phase::ResponseHeaders {
            return InterceptionResult::Continue;
        }
        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.headers
                .push(("x-wire-probe".to_string(), "from-interceptor".to_string()));
        }
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let _ = flow;
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        message: relay_core_api::flow::WebSocketMessage,
    ) -> Result<relay_core_lib::interceptor::WebSocketMessageAction, BoxError> {
        Ok(relay_core_lib::interceptor::WebSocketMessageAction::Continue(message))
    }
}

/// What the upstream socket actually received for one case.
struct UpstreamCapture {
    head: String,
    _body: Vec<u8>,
}

/// Start a recording upstream. It reads one request head (+ declared body), then replies with a
/// `200` whose body echoes the received head so the test can observe both directions.
async fn spawn_recording_upstream() -> (SocketAddr, tokio::sync::oneshot::Receiver<UpstreamCapture>)
{
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = Vec::new();

        // Read until we have the full head, then the declared body length if present.
        let (head_end, content_length) = loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break (received.len(), 0usize),
                Ok(n) => n,
            };
            received.extend_from_slice(&buf[..n]);

            if let Some(pos) = find_subslice(&received, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..pos]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                break (pos + 4, len);
            }
        };

        let mut body = received.get(head_end..).unwrap_or_default().to_vec();
        while body.len() < content_length {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
            }
        }

        let head = String::from_utf8_lossy(&received[..head_end.min(received.len())]).to_string();
        let head_len = head.len();
        let _ = tx.send(UpstreamCapture {
            head: head.clone(),
            _body: body,
        });

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Upstream: recording\r\nConnection: close\r\n\r\n",
            head_len
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.flush().await;
    });

    (addr, rx)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Send one request through the proxy and return the raw response received by the client.
async fn drive(proxy_port: u16, target: SocketAddr, request_head: &str, body: &str) -> String {
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("proxy handshake");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let uri = format!("http://{}{}", target, "/probe")
        .parse::<hyper::Uri>()
        .expect("uri");
    let req = hyper::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", target.to_string())
        .header("content-type", "text/plain")
        .body(String::from(body))
        .expect("request");

    let _ = request_head;

    let resp = sender.send_request(req).await.expect("send via proxy");
    let status = resp.status();
    let mut resp_headers = String::new();
    for (k, v) in resp.headers() {
        resp_headers.push_str(&format!("{}: {}\n", k, v.to_str().unwrap_or("<binary>")));
    }
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect response")
        .to_bytes();
    let echoed = String::from_utf8_lossy(&bytes).to_string();

    format!(
        "STATUS {}\nHEADERS\n{resp_headers}ECHOED-UPSTREAM-REQUEST\n{echoed}\n",
        status.as_u16()
    )
}

/// Run one case end to end: returns (what the client observed, what the upstream received).
async fn run_case(case: &Case) -> (String, UpstreamCapture) {
    run_case_with(case, Arc::new(MutateInterceptor { phase: case.phase })).await
}

/// Same, with an explicit interceptor (used to build multi-member chains).
async fn run_case_with(
    case: &Case,
    interceptor: Arc<dyn Interceptor>,
) -> (String, UpstreamCapture) {
    init_crypto();

    let (upstream_addr, upstream_rx) = spawn_recording_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, _flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());

    tokio::spawn(async move {
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

    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    let client_observed = drive(proxy_port, upstream_addr, case.request, case.body).await;
    let upstream = tokio::time::timeout(std::time::Duration::from_secs(5), upstream_rx)
        .await
        .expect("upstream capture timed out")
        .expect("upstream capture dropped");

    (client_observed, upstream)
}

// ── Baseline: the harness itself must be trustworthy ────────────────────────────────
//
// If this fails, every other case in this file is meaningless.

#[tokio::test]
async fn wire_matrix_baseline_round_trip_is_unmodified() {
    const CASE: Case = Case {
        id: "baseline",
        phase: Phase::None,
        request: "POST /probe HTTP/1.1",
        body: "payload",
    };

    let (client, upstream) = run_case(&CASE).await;

    assert!(
        upstream.head.starts_with("POST /probe HTTP/1.1"),
        "[{}] upstream must receive the forwarded request line, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert!(
        !upstream.head.to_lowercase().contains("x-wire-probe"),
        "[{}] no interceptor ran, so no probe header may appear upstream",
        CASE.id
    );
    assert!(
        client.contains("STATUS 200"),
        "[{}] client must receive the upstream response, got:\n{client}",
        CASE.id
    );
}

// ── Request-direction mutations ─────────────────────────────────────────────────────
//
// Request method/URI/headers are rebuilt from the Flow by `build_forward_request`,
// so these are expected to reach the upstream today.

#[tokio::test]
async fn wire_matrix_request_header_mutation_reaches_upstream() {
    const CASE: Case = Case {
        id: "request_headers",
        phase: Phase::RequestHeaders,
        request: "POST /probe HTTP/1.1",
        body: "payload",
    };

    let (client, upstream) = run_case(&CASE).await;

    assert!(
        upstream
            .head
            .to_lowercase()
            .contains("x-wire-probe: from-interceptor"),
        "[{}] a request-header mutation must reach the upstream socket, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert!(client.contains("STATUS 200"), "[{}] expected 200", CASE.id);
}

// ── Response-direction mutations ────────────────────────────────────────────────────
//
// Known gap (roadmap §24.1): the client-facing response is built from the upstream `res_parts`
// captured in `handle_http_request`, so response header/status mutations only change the Flow.
// Un-ignore when the data plane is fixed (A5: single source of truth).

#[tokio::test]
#[ignore = "roadmap §24.1: response header mutation does not reach the wire (res_parts wins)"]
async fn wire_matrix_response_header_mutation_reaches_client() {
    const CASE: Case = Case {
        id: "response_headers",
        phase: Phase::ResponseHeaders,
        request: "POST /probe HTTP/1.1",
        body: "payload",
    };

    let (client, _upstream) = run_case(&CASE).await;

    assert!(
        client
            .to_lowercase()
            .contains("x-wire-probe: from-interceptor"),
        "[{}] a response-header mutation must reach the client socket, got:\n{client}",
        CASE.id
    );
}

// ── Request body mutations ──────────────────────────────────────────────────────────
//
// Known gap (roadmap §24.1): `Action::SetRequestBody` writes `flow.layer.http.request.body`,
// but the forwarded body is the separate `current_body` stream argument, so the upstream still
// receives the original bytes. Un-ignore when the data plane is fixed (A5).

#[tokio::test]
#[ignore = "roadmap §24.1: SetRequestBody writes the Flow, the wire keeps the original stream"]
async fn wire_matrix_request_body_mutation_reaches_upstream() {
    const CASE: Case = Case {
        id: "request_body",
        phase: Phase::RequestBody,
        request: "POST /probe HTTP/1.1",
        body: "original-payload",
    };

    let (client, upstream) = run_case(&CASE).await;

    assert!(
        !upstream._body.is_empty(),
        "[{}] upstream must have received a body",
        CASE.id
    );
    assert_eq!(
        String::from_utf8_lossy(&upstream._body),
        "REPLACED",
        "[{}] a request-body mutation must reach the upstream socket",
        CASE.id
    );
    assert!(client.contains("STATUS 200"), "[{}] expected 200", CASE.id);
}

// ── Multi-interceptor chains ────────────────────────────────────────────────────────
//
// Known gap (roadmap §24.4): the Tauri host registers BOTH the runtime `RuleInterceptor` and its
// own `TauriInterceptor`, and `CompositeInterceptor` only short-circuits on `Drop`/`MockResponse`/
// `ModifiedResponse` — not on `ModifiedRequest`. The rule engine therefore runs twice per hook,
// and because `AddRequestHeader` appends unconditionally, duplicate header values reach the
// upstream. This case pins the contract; the fix belongs with the single-source-of-truth work
// (A5) because a naive "skip the second execution" changes which interceptor owns the wire result.

/// Appends one header per invocation, so the upstream transitively reports how many times an
/// interceptor in the chain ran.
#[tokio::test]
#[ignore = "roadmap §24.4: composite runs rule-mutating interceptors once per chain member"]
async fn wire_matrix_mutation_is_applied_once_per_chain() {
    const CASE: Case = Case {
        id: "no_double_apply",
        phase: Phase::RequestHeaders,
        request: "POST /probe HTTP/1.1",
        body: "payload",
    };

    // Exactly what the Tauri host registers: the runtime rule interceptor plus the host's own
    // rule-executing interceptor.
    let chain: Arc<dyn Interceptor> = Arc::new(CompositeInterceptor::new(vec![
        Arc::new(MutateInterceptor { phase: CASE.phase }),
        Arc::new(MutateInterceptor { phase: CASE.phase }),
    ]));

    let (client, upstream) = run_case_with(&CASE, chain).await;

    let occurrences = upstream
        .head
        .to_lowercase()
        .matches("x-wire-probe:")
        .count();
    assert_eq!(
        occurrences, 1,
        "[{}] a mutation must be applied exactly once, got {} occurrences in:\n{}",
        CASE.id, occurrences, upstream.head
    );
    assert!(client.contains("STATUS 200"), "[{}] expected 200", CASE.id);
}
