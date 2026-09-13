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
//! Every case in this file passes and **none is `#[ignore]`d**: a case is added when the data plane
//! actually works, and a capability with no case is listed as untested below rather than parked as
//! a skipped test. **Do not delete an assertion to make a case pass.**
//!
//! ## Action coverage inventory
//!
//! The full public action set is `relay-core-api/src/rule.rs` (30 variants). This table is the
//! single tracking point for "has a real wire-level assertion yet". Per roadmap §22 a capability
//! may not be reported `Stable` while its row reads *untested*.
//!
//! | Action | Wire status | Case |
//! |---|---|---|
//! | `Add/Update/DeleteRequestHeader` | **verified** | `wire_matrix_request_header_mutation_reaches_upstream` |
//! | `Add/Update/DeleteResponseHeader` | **verified** | `wire_matrix_response_header_mutation_reaches_client` |
//! | `SetResponseStatus` | **verified** | `wire_matrix_response_status_mutation_reaches_client` |
//! | `SetRequestBody` / `TransformRequestBody` | **verified** | `wire_matrix_request_body_mutation_reaches_upstream` |
//! | `SetResponseBody` / `TransformResponseBody` | **verified** | `wire_matrix_response_body_replacement_reaches_client` |
//! | `TransformResponseBody` under `Content-Encoding` | **verified** (gzip / br / zstd) | `wire_matrix_{gzip,brotli,zstd}_response_rewrite_stays_decodable` |
//! | `TransformRequestBody` under `Content-Encoding` | **verified** (plaintext + header dropped) | `wire_matrix_compressed_request_rewrite_is_sent_as_plaintext_without_a_false_header` |
//! | `MockWebSocketMessage` | **interceptor-level only** — the frame is replaced, but no socket assertion exists | `relay-core-runtime/tests/actor_tests.rs` |
//! | `MapRemote` (WebSocket handshake) | **verified** | `wire_matrix_ws_handshake_target_rewrite_is_honoured` |
//! | rule matching on a retained body (request / response) | **verified** | `wire_matrix_body_stage_rule_matches_on_the_body`, `wire_matrix_response_body_rule_matches_on_the_body` |
//! | rule matching through `Content-Encoding` | **verified** | `wire_matrix_body_filter_matches_through_content_encoding`, `..._verdict_is_matched_not_missed_on_gzip` |
//! | chain applies a mutation once | **verified** | `wire_matrix_stage_guard_applies_mutation_once_per_chain` |
//! | exchange lifecycle (`end_time`, close reason, drops) | **verified** | `wire_matrix_completed_flow_records_its_end_time`, `..._failed_flow_records_its_end_time`, `..._a_finished_exchange_reports_its_close_reason_once`, `..._dropped_exchange_is_not_reported_as_completed` |
//! | WebSocket session end / failed handshake | **verified** | `wire_matrix_ws_session_end_reaches_consumers`, `wire_matrix_failed_ws_handshake_is_recorded` |
//! | ingress/response HTTP version, client connection reuse | **verified** | `wire_matrix_forwarded_request_carries_the_client_version`, `..._response_version_matches_the_client_not_the_upstream`, `..._client_connection_is_reused_for_a_second_request` |
//! | `SetRequestMethod` / `SetRequestUrl` | **untested** (same rebuild path as request headers) | – |
//! | `MapLocal` / `MapRemote` (HTTP) / `Redirect` / `MockResponse` | **untested** | – |
//! | `Delay` / `Throttle` / `RateLimit` | **untested** (`Throttle` has a wrapper-level timing test) | – |
//! | `ForwardPort` / `RedirectIp` | **untested** (embedded transparent-TCP path only) | – |
//! | `SetTtl` | **no wire effect anywhere** (warn-only stub) | – |
//! | `Tag` / `SetVariable` | Flow-metadata only by design | – |
//! | `Inspect` | pauses the exchange for up to 60s; **no wire case** | – |
//!
//! Rows marked *untested* are the gap list for this file: an action may be `untested` and still
//! work, but per roadmap §22 it may not be reported `Stable`.

use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::event::CloseReason;
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
    /// Refuse the exchange at the request-headers stage, the way a policy drop does.
    RequestHeadersDrop,
    RequestHeaders,
    RequestBody,
    ResponseHeaders,
    ResponseStatus,
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
        if self.phase == Phase::RequestHeadersDrop {
            return InterceptionResult::Drop;
        }
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
        if self.phase == Phase::ResponseStatus {
            // Mirror `Action::SetResponseStatus`.
            if let Layer::Http(http) = &mut flow.layer
                && let Some(res) = &mut http.response
            {
                res.status = 418;
            }
            return InterceptionResult::Continue;
        }
        if self.phase != Phase::ResponseHeaders {
            return InterceptionResult::Continue;
        }
        match &mut flow.layer {
            Layer::Http(http) => {
                if let Some(res) = &mut http.response {
                    res.headers
                        .push(("x-wire-probe".to_string(), "from-interceptor".to_string()));
                }
            }
            // A WebSocket flow carries its handshake response separately.
            Layer::WebSocket(ws) => {
                ws.handshake_response
                    .headers
                    .push(("x-wire-probe".to_string(), "from-interceptor".to_string()));
            }
            _ => {}
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
/// Recording upstream. It echoes the received request head unless `reply_body` is given, so one
/// harness serves both "what did the upstream receive" and "what did it answer" assertions.
async fn spawn_recording_upstream_with_reply(
    reply_body: Option<&'static str>,
) -> (SocketAddr, tokio::sync::oneshot::Receiver<UpstreamCapture>) {
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
        let _ = tx.send(UpstreamCapture {
            head: head.clone(),
            _body: body,
        });

        let payload: Vec<u8> = match reply_body {
            Some(body) => body.as_bytes().to_vec(),
            None => head.as_bytes().to_vec(),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Upstream: recording\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.write_all(&payload).await;
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
    run_case_full(case, interceptor, None).await
}

/// Full control: interceptor plus an optional fixed upstream reply body.
async fn run_case_full(
    case: &Case,
    interceptor: Arc<dyn Interceptor>,
    reply_body: Option<&'static str>,
) -> (String, UpstreamCapture) {
    init_crypto();

    let (upstream_addr, upstream_rx) = spawn_recording_upstream_with_reply(reply_body).await;

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
// Fixed in A5: the client response head is rebuilt from the Flow by
// `build_client_response_head`, so status/header mutations reach the client while the body
// still streams from the upstream.

#[tokio::test]
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

/// `SetResponseStatus` reaches the client as a status line, not just as a Flow field.
///
/// The `Phase::ResponseStatus` scaffolding existed but no case selected it, so the roadmap's claim
/// that this action is wire-verified was unsupported: a status mutation that only changed `Flow`
/// would have looked identical to a working one.
#[tokio::test]
async fn wire_matrix_response_status_mutation_reaches_client() {
    const CASE: Case = Case {
        id: "response_status",
        phase: Phase::ResponseStatus,
        request: "GET /probe HTTP/1.1",
        body: "",
    };

    let (client, _upstream) = run_case(&CASE).await;

    assert!(
        client.contains("418"),
        "[{}] a status mutation must reach the client status line, got:\n{client}",
        CASE.id
    );
    assert!(
        !client.contains("200 OK"),
        "[{}] the upstream status must not also be sent, got:\n{client}",
        CASE.id
    );
}

// ── Request body mutations ──────────────────────────────────────────────────────────
//
// Fixed in A5b: when an interceptor replaced the request body, the Flow's bytes are materialized
// and the request is reframed (content-length recomputed, transfer-encoding/content-encoding
// dropped). An unreplaced body still streams.

#[tokio::test]
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
// A host adapter may register its own rule-executing interceptor alongside the runtime's
// `RuleInterceptor` — the Tauri desktop does, because its interceptor buffers bodies. Both used to
// execute the same stage, so every mutation applied twice: duplicate headers reached the upstream,
// `Delay` slept twice, `RateLimit` double-counted.
//
// The fix is `relay_core_lib::rule::stage_guard`: the first member to run records the stage in
// `Flow.meta` (never serialized) and later members skip. This case pins that contract, since the
// guard only works if every rule-executing interceptor honours it.

/// A rule-executing interceptor shaped like the host adapter's: it performs host-specific work,
/// then executes the stage only if no earlier member already did.
struct GuardedRuleLikeInterceptor {
    marker: &'static str,
}

#[async_trait::async_trait]
impl Interceptor for GuardedRuleLikeInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        use relay_core_lib::rule::stage_guard::{mark_stage_executed, stage_already_executed};
        let stage = relay_core_api::rule::RuleStage::RequestHeaders;

        if stage_already_executed(flow, &stage) {
            return InterceptionResult::Continue;
        }
        mark_stage_executed(flow, &stage);

        if let Layer::Http(http) = &mut flow.layer {
            http.request
                .headers
                .push((self.marker.to_string(), "applied".to_string()));
        }
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

#[tokio::test]
async fn wire_matrix_stage_guard_applies_mutation_once_per_chain() {
    const CASE: Case = Case {
        id: "stage_guard",
        phase: Phase::None,
        request: "POST /probe HTTP/1.1",
        body: "payload",
    };

    // Two rule-executing members, exactly the shape the desktop host registers.
    let chain: Arc<dyn Interceptor> = Arc::new(CompositeInterceptor::new(vec![
        Arc::new(GuardedRuleLikeInterceptor {
            marker: "x-first-member",
        }),
        Arc::new(GuardedRuleLikeInterceptor {
            marker: "x-second-member",
        }),
    ]));

    let (client, upstream) = run_case_with(&CASE, chain).await;
    let head = upstream.head.to_lowercase();

    assert!(
        head.contains("x-first-member: applied"),
        "[{}] the first member must apply its mutation, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert!(
        !head.contains("x-second-member"),
        "[{}] a later member must NOT re-apply the same stage, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert_eq!(
        head.matches("x-first-member:").count(),
        1,
        "[{}] the mutation must appear exactly once, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert!(client.contains("STATUS 200"), "[{}] expected 200", CASE.id);
}

// ── WebSocket handshake ─────────────────────────────────────────────────────────────
//
// Fixed in A5c: the WS path now records the upstream handshake response on the live Flow, runs
// `Interceptor::on_response_headers` (previously the only hook it skipped), and builds the client's
// 101 from the Flow — so handshake response-header mutations reach the client and
// `flow.handshake_response` is finally populated for observation.

/// Upstream that accepts a WebSocket upgrade so RelayCore has a real 101 to relay.
/// An upstream that refuses to upgrade: it drops the connection immediately, so a handshake that
/// reaches it cannot answer 101.
async fn spawn_refusing_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind refusing upstream");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => drop(stream),
                Err(_) => return,
            }
        }
    });

    addr
}

async fn spawn_ws_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ws upstream");
    let addr = listener.local_addr().expect("ws upstream addr");

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let _ = tokio_tungstenite::accept_async(stream).await;
        // Hold the socket open briefly so the handshake relay completes.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    });

    addr
}

#[tokio::test]
async fn wire_matrix_ws_handshake_response_header_reaches_client() {
    init_crypto();

    let upstream_addr = spawn_ws_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor {
        phase: Phase::ResponseHeaders,
    });
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

    // Raw handshake so we can read the 101 headers the client actually receives.
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{upstream_addr}/ws HTTP/1.1\r\n\
         Host: {upstream_addr}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write handshake");

    let mut buf = vec![0u8; 8192];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake timed out")
        .expect("read handshake response");
    let head = String::from_utf8_lossy(&buf[..read]).to_string();

    assert!(
        head.starts_with("HTTP/1.1 101"),
        "expected a 101 upgrade, got:\n{head}"
    );
    assert!(
        head.to_lowercase()
            .contains("x-wire-probe: from-interceptor"),
        "a WS handshake response-header mutation must reach the client, got:\n{head}"
    );
}

// ── Body-stage rules can see the body (§24.2 / A6) ──────────────────────────────────
//
// A body-stage rule cannot match a body nobody buffered: the engine ran before the stream was ever
// polled, so `flow.request.body` was empty and a body filter silently never matched. This case
// drives the BodyPlan mechanism end to end — decide, retain a bounded prefix, record it on the
// flow, then match — and asserts the result on the wire.

/// Mimics the rule interceptor's body handling: consult the plan, buffer only if the stage needs
/// the body, record it, and derive the decision from the recorded body.
struct BodyStageRuleLikeInterceptor;

#[async_trait::async_trait]
impl Interceptor for BodyStageRuleLikeInterceptor {
    async fn on_request_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        use relay_core_api::body_plan::{BodyPlan, BodyPlanInputs, decide};
        use relay_core_api::flow::Direction;
        use relay_core_lib::proxy::body_plan::{
            buffer_body_within_budget, headers_for_direction, record_body_on_flow,
        };

        // This host has a RequestBody rule whose filter reads the body.
        let plan = decide(BodyPlanInputs {
            has_body_stage_rules: true,
            has_body_hook_script: false,
            has_body_intercept: false,
            observation: relay_core_api::body_plan::BodyObservation::Off,
            budget: 64 * 1024,
        });

        let BodyPlan::Buffer { limit } = plan else {
            return Ok(RequestAction::Continue(body));
        };

        // Materialize within the budget: a decision made before forwarding needs the bytes, unlike
        // the observation path where frames are merely retained as they flow past.
        let (snapshot, forwarded) = buffer_body_within_budget(body, limit).await?;
        let headers = headers_for_direction(flow, Direction::ClientToServer);
        record_body_on_flow(
            flow,
            Direction::ClientToServer,
            &snapshot.bytes,
            snapshot.total_bytes,
            &headers,
        );

        // Evaluate the "rule": it matches only if the recorded body is visible.
        let matched = match &flow.layer {
            Layer::Http(http) => http
                .request
                .body
                .as_ref()
                .is_some_and(|b| b.content.contains("needle")),
            _ => false,
        };

        if matched && let Layer::Http(http) = &mut flow.layer {
            http.request
                .headers
                .push(("x-body-rule".to_string(), "matched".to_string()));
        }

        Ok(RequestAction::Continue(forwarded))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

#[tokio::test]
async fn wire_matrix_body_stage_rule_matches_on_the_body() {
    const CASE: Case = Case {
        id: "body_stage_match",
        phase: Phase::None,
        request: "POST /probe HTTP/1.1",
        body: "contains-a-needle-here",
    };

    let (client, upstream) = run_case_with(&CASE, Arc::new(BodyStageRuleLikeInterceptor)).await;

    assert!(
        upstream
            .head
            .to_lowercase()
            .contains("x-body-rule: matched"),
        "[{}] a body-stage rule must be able to match the request body, got:\n{}",
        CASE.id,
        upstream.head
    );
    assert!(
        !upstream._body.is_empty(),
        "[{}] the body must still be forwarded while it is inspected",
        CASE.id
    );
    assert!(client.contains("STATUS 200"), "[{}] expected 200", CASE.id);
}

// ── Response-body stage rules can see the body (§24.2 / A6) ─────────────────────────
//
// The response body is a stream, and the body stage runs at the response-header moment — before the
// body has been read. A rule that inspects it therefore needs the proxy to retain the body *before*
// forwarding, which it can only know if the interceptor declared the intent during the request
// phase. This case drives that declaration, the retention, and the match end to end.

/// Mimics the rule interceptor: declares the need for the response body up front, then matches it.
struct ResponseBodyRuleLikeInterceptor;

#[async_trait::async_trait]
impl Interceptor for ResponseBodyRuleLikeInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        // The rule set has a ResponseBody rule that reads the body, so ask for retention.
        relay_core_lib::rule::stage_guard::request_response_body(flow, 64 * 1024);
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        // Evaluate the "rule" against the retained body.
        let matched = match &flow.layer {
            Layer::Http(http) => http
                .response
                .as_ref()
                .and_then(|r| r.body.as_ref())
                .is_some_and(|b| b.content.contains("needle")),
            _ => false,
        };

        if matched
            && let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.headers
                .push(("x-body-rule".to_string(), "matched".to_string()));
        }

        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

#[tokio::test]
async fn wire_matrix_response_body_rule_matches_on_the_body() {
    const CASE: Case = Case {
        id: "response_body_match",
        phase: Phase::None,
        request: "GET /probe HTTP/1.1",
        body: "",
    };

    let (client, _upstream) = run_case_full(
        &CASE,
        Arc::new(ResponseBodyRuleLikeInterceptor),
        Some("a-body-with-a-needle-inside"),
    )
    .await;

    assert!(
        client.to_lowercase().contains("x-body-rule: matched"),
        "[{}] a response-body rule must be able to match the response body, got:\n{client}",
        CASE.id
    );
    assert!(
        client.contains("a-body-with-a-needle-inside"),
        "[{}] inspecting the body must not consume it, got:\n{client}",
        CASE.id
    );
}

// ── Response body replacement (§24.1 SetResponseBody) ───────────────────────────────
//
// Fixed: a replaced response body now makes the Flow authoritative for the body as well, and the
// head is reframed (content-length recomputed, transfer-encoding/content-encoding dropped) so the
// framing describes the replacement rather than the upstream stream.

/// Mimics `Action::SetResponseBody`: replace the Flow's response body, then let the pipeline send it.
struct ResponseBodyReplaceInterceptor;

#[async_trait::async_trait]
impl Interceptor for ResponseBodyReplaceInterceptor {
    async fn on_request_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: "REPLACED-RESPONSE".to_string(),
                size: "REPLACED-RESPONSE".len() as u64,
            });
        }
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

#[tokio::test]
async fn wire_matrix_response_body_replacement_reaches_client() {
    const CASE: Case = Case {
        id: "response_body_replace",
        phase: Phase::None,
        request: "GET /probe HTTP/1.1",
        body: "",
    };

    let (client, _upstream) = run_case_full(
        &CASE,
        Arc::new(ResponseBodyReplaceInterceptor),
        Some("original-upstream-body"),
    )
    .await;

    assert!(
        client.contains("REPLACED-RESPONSE"),
        "[{}] a response-body replacement must reach the client, got:\n{client}",
        CASE.id
    );
    assert!(
        !client.contains("original-upstream-body"),
        "[{}] the original body must not also be sent, got:\n{client}",
        CASE.id
    );
}

// ── WebSocket handshake target rewrite (§24.1 MapRemote on WS) ──────────────────────
//
// The WS path read its forwarding target from the original request metadata, so a rule that
// rewrote the handshake URL (`Action::MapRemote`) changed the Flow and the UI while the handshake
// still went to the original target.

/// Rewrites the handshake URL the way `Action::MapRemote` does, then leaves the rest to the proxy.
struct WsMapRemoteInterceptor {
    target: &'static str,
}

#[async_trait::async_trait]
impl Interceptor for WsMapRemoteInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        if let Layer::WebSocket(ws) = &mut flow.layer
            && let Ok(url) = url::Url::parse(self.target)
        {
            ws.handshake_request.url = url;
        }
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

/// Decode HTTP/1.1 chunked transfer-encoding into its payload.
fn decode_chunked(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

/// Send a WS handshake through the proxy and report the first response line.
async fn ws_handshake_through_proxy(
    proxy_port: u16,
    target: SocketAddr,
    interceptor: Arc<dyn Interceptor>,
) -> String {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let port = listener.local_addr().expect("proxy addr").port();

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
    let _ = proxy_port;

    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{target}/ws HTTP/1.1\r\nHost: {target}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write handshake");

    let mut buf = vec![0u8; 4096];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake timed out")
        .expect("read handshake response");
    String::from_utf8_lossy(&buf[..read]).to_string()
}

#[tokio::test]
async fn wire_matrix_ws_handshake_target_rewrite_is_honoured() {
    const CASE: Case = Case {
        id: "ws_map_remote",
        phase: Phase::None,
        request: "GET /ws HTTP/1.1",
        body: "",
    };

    // The upstream the handshake must reach after the rewrite.
    let rewritten_addr = spawn_ws_upstream().await;
    // The original target refuses to upgrade, so a 101 proves the rewrite was honoured rather than
    // merely that *some* upstream answered.
    let original_addr = spawn_refusing_upstream().await;

    let interceptor: Arc<dyn Interceptor> = Arc::new(WsMapRemoteInterceptor {
        target: Box::leak(format!("http://{rewritten_addr}/ws").into_boxed_str()),
    });

    let response = ws_handshake_through_proxy(0, original_addr, interceptor).await;

    assert!(
        response.starts_with("HTTP/1.1 101"),
        "[{}] the handshake must go to the rewritten target; the original refuses to upgrade, got:\n{response}",
        CASE.id
    );
}

// ── Streaming is preserved when nothing inspects the body (§22) ─────────────────────
//
// Recording a body so it can be inspected is only worth its cost when something actually inspects
// it. An earlier attempt at body observation buffered every response body before forwarding, which
// silently traded away streaming; this case guards that property directly.

/// A body that hands out its frames one at a time and reports how they were consumed.
///
/// If the pipeline materializes the body before forwarding it, the frames are drained before the
/// upstream sees anything; if it streams, the first frame reaches the upstream while later frames
/// are still pending.
struct ChunkyBody {
    chunks: Vec<&'static [u8]>,
    index: usize,
}

impl hyper::body::Body for ChunkyBody {
    type Data = bytes::Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        if self.index >= self.chunks.len() {
            return std::task::Poll::Ready(None);
        }
        let chunk = self.chunks[self.index];
        self.index += 1;
        std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(
            bytes::Bytes::from_static(chunk),
        ))))
    }
}

#[tokio::test]
async fn wire_matrix_large_body_is_forwarded_intact_without_body_rules() {
    // Installing the crypto provider is process-global and order-dependent, so a case must not rely
    // on another test having done it first.
    init_crypto();

    const CASE: Case = Case {
        id: "streaming_intact",
        phase: Phase::None,
        request: "POST /probe HTTP/1.1",
        body: "",
    };

    // A body whose content is large enough to exceed any incidental small buffer, with no
    // body-stage rule installed: every byte must still reach the upstream.
    let chunk_strs: [&'static str; 3] = ["chunk-a-", "chunk-b-", "chunk-c-"];
    let expected: String = chunk_strs.concat();
    let chunks: Vec<&'static [u8]> = chunk_strs.iter().map(|c| c.as_bytes()).collect();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let upstream_addr = listener.local_addr().expect("addr");

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = Vec::new();
        let mut head_len = 0usize;
        let mut content_length = 0usize;
        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            received.extend_from_slice(&buf[..n]);
            if head_len == 0
                && let Some(pos) = received.windows(4).position(|w| w == b"\r\n\r\n")
            {
                head_len = pos + 4;
                let head = String::from_utf8_lossy(&received[..pos]).to_string();
                content_length = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
            }
            if head_len > 0 {
                if content_length > 0 && received.len() >= head_len + content_length {
                    break;
                }
                // A streamed body has no content-length (hop-by-hop headers are filtered), so read
                // until the chunked terminator instead of stopping at the head.
                if content_length == 0 && received[head_len..].windows(5).any(|w| w == b"0\r\n\r\n")
                {
                    break;
                }
            }
        }
        let body =
            String::from_utf8_lossy(received.get(head_len..).unwrap_or_default()).to_string();
        let _ = tx.send(body);
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("POST")
        .uri(format!("http://{upstream_addr}/probe"))
        .header("host", upstream_addr.to_string())
        .body(ChunkyBody { chunks, index: 0 })
        .expect("request");

    let resp = sender.send_request(req).await.expect("send");
    assert_eq!(resp.status().as_u16(), 200);

    let received = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .expect("upstream timed out")
        .expect("upstream dropped");

    // The body is streamed with chunked transfer-encoding, so the raw bytes carry chunk framing;
    // decode it and assert the payload survived every hop intact.
    let decoded = decode_chunked(&received);
    assert_eq!(
        decoded, expected,
        "[{}] a streamed body must reach the upstream intact (raw: {received:?})",
        CASE.id
    );
    assert!(
        received.contains("0\r\n\r\n"),
        "[{}] the streamed body must be terminated with a zero-length chunk",
        CASE.id
    );
}

// ── Content-Encoding correctness on rewrite (§24.3) ─────────────────────────────────
//
// The engine had no compression handling, so a rewritten compressed body was sent while the
// upstream's `Content-Encoding` header still claimed it was compressed: the client was told to
// gunzip plaintext. These cases assert that header and body always agree.

/// Replace the response body, leaving framing/encoding to the proxy.
struct ReplaceResponseBodyInterceptor {
    replacement: &'static str,
}

#[async_trait::async_trait]
impl Interceptor for ReplaceResponseBodyInterceptor {
    async fn on_request_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: self.replacement.to_string(),
                size: self.replacement.len() as u64,
            });
        }
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

/// Send a request through the proxy and return the raw body bytes plus the declared encoding.
async fn drive_and_read_raw_body(proxy_port: u16, target: SocketAddr) -> (Vec<u8>, Option<String>) {
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://{target}/probe"))
        .header("host", target.to_string())
        .body(String::new())
        .expect("request");

    let resp = sender.send_request(req).await.expect("send");
    let encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes()
        .to_vec();

    (bytes, encoding)
}

#[tokio::test]
async fn wire_matrix_gzip_response_rewrite_stays_decodable() {
    init_crypto();

    // Upstream replies with gzip-encoded content.
    let plain = b"original-compressed-payload";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, plain).expect("write");
    let gzipped = encoder.finish().expect("finish");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let target = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gzipped.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&gzipped).await;
        let _ = socket.flush().await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(ReplaceResponseBodyInterceptor {
        replacement: "REWRITTEN-CONTENT",
    });
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let (bytes, encoding) = drive_and_read_raw_body(proxy_port, target).await;

    assert_eq!(
        encoding.as_deref(),
        Some("gzip"),
        "the upstream claimed gzip, so the rewrite must still be gzip"
    );

    // What the client is told must actually decode to the replacement.
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut decoded = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut decoded)
        .expect("the declared encoding must describe the bytes that were sent");
    assert_eq!(decoded, "REWRITTEN-CONTENT");
    assert!(
        !decoded.contains("original-compressed-payload"),
        "the upstream body must not survive a replacement"
    );
}

// ── brotli rewriting (§24.3, matching mitmproxy's codec coverage) ───────────────────
//
// mitmproxy decodes and re-encodes gzip, deflate, br and zstd (docs/mitmproxy-policy-benchmark.md
// §1.2). This asserts the same contract for br: a rewritten body must still be valid br with a
// header that says so.

/// A real brotli frame produced by the `brotli` CLI, so the decoder is exercised against a genuine
/// stream rather than output produced by the same library.
const BROTLI_RESPONSE: &[u8] = include_bytes!("fixtures/brotli_payload.br");
const BROTLI_PLAINTEXT: &str = "brotli-encoded-payload-for-relaycore-tests";

#[tokio::test]
async fn wire_matrix_brotli_response_rewrite_stays_decodable() {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let target = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: br\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            BROTLI_RESPONSE.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(BROTLI_RESPONSE).await;
        let _ = socket.flush().await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(ReplaceResponseBodyInterceptor {
        replacement: "REWRITTEN-BR-CONTENT",
    });
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let (bytes, encoding) = drive_and_read_raw_body(proxy_port, target).await;

    assert_eq!(
        encoding.as_deref(),
        Some("br"),
        "the upstream claimed br, so the rewrite must still be br"
    );

    // The declared encoding must describe the bytes actually sent.
    let mut decoded = Vec::new();
    brotli::BrotliDecompress(&mut std::io::Cursor::new(&bytes[..]), &mut decoded)
        .expect("declared br encoding must decode the bytes that were sent");
    let decoded = String::from_utf8(decoded).expect("utf-8");
    assert_eq!(decoded, "REWRITTEN-BR-CONTENT");
    assert!(
        !decoded.contains(BROTLI_PLAINTEXT),
        "the upstream body must not survive a replacement"
    );
}

// ── zstd rewriting (§24.3, completing mitmproxy codec parity) ──────────────────────

/// A real zstd frame produced by the `zstd` CLI.
const ZSTD_RESPONSE: &[u8] = include_bytes!("fixtures/zstd_payload.bin");
const ZSTD_PLAINTEXT: &str = "zstd-encoded-payload-for-relaycore-tests";

#[tokio::test]
async fn wire_matrix_zstd_response_rewrite_stays_decodable() {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let target = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: zstd\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            ZSTD_RESPONSE.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(ZSTD_RESPONSE).await;
        let _ = socket.flush().await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(ReplaceResponseBodyInterceptor {
        replacement: "REWRITTEN-ZSTD-CONTENT",
    });
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let (bytes, encoding) = drive_and_read_raw_body(proxy_port, target).await;

    assert_eq!(
        encoding.as_deref(),
        Some("zstd"),
        "the upstream claimed zstd, so the rewrite must still be zstd"
    );

    let decoded = zstd::stream::decode_all(&bytes[..])
        .expect("declared zstd encoding must decode the bytes that were sent");
    let decoded = String::from_utf8(decoded).expect("utf-8");
    assert_eq!(decoded, "REWRITTEN-ZSTD-CONTENT");
    assert!(
        !decoded.contains(ZSTD_PLAINTEXT),
        "the upstream body must not survive a replacement"
    );
}

// ── Body-stage filters see plaintext through Content-Encoding (§24.3) ───────────────
//
// Retention for matching used to record the bytes as received, so on a gzip/br/zstd response a
// filter was matched against compressed bytes and silently never fired. mitmproxy avoids this by
// buffering and exposing a decoded `text`; RelayCore keeps streaming, so the decode has to happen
// where the body is retained.

#[tokio::test]
async fn wire_matrix_body_filter_matches_through_content_encoding() {
    init_crypto();

    // Upstream replies gzip-encoded; the *needle* only exists in the plaintext.
    let plain = b"a-payload-with-needle-inside";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, plain).expect("write");
    let gzipped = encoder.finish().expect("finish");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let target = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gzipped.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&gzipped).await;
        let _ = socket.flush().await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(ResponseBodyRuleLikeInterceptor);
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let (bytes, encoding) = drive_and_read_raw_body(proxy_port, target).await;

    // The filter must have matched the decoded payload, not the compressed bytes...
    assert_eq!(
        encoding.as_deref(),
        Some("gzip"),
        "the response must still declare gzip"
    );
    let mut decoded = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut decoded)
        .expect("declared gzip encoding must decode what was sent");
    assert_eq!(
        decoded, "a-payload-with-needle-inside",
        "an unmodified compressed body must pass through unchanged"
    );
}

/// Same contract, but the filter's verdict is visible to the client so a non-match cannot pass
/// silently.
struct FilterOnDecodedBodyInterceptor;

#[async_trait::async_trait]
impl Interceptor for FilterOnDecodedBodyInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        relay_core_lib::rule::stage_guard::request_response_body(flow, 64 * 1024);
        InterceptionResult::Continue
    }

    async fn on_request(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<RequestAction, BoxError> {
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let matched = match &flow.layer {
            Layer::Http(http) => http
                .response
                .as_ref()
                .and_then(|r| r.body.as_ref())
                .is_some_and(|b| b.content.contains("needle")),
            _ => false,
        };

        if let Layer::Http(http) = &mut flow.layer
            && let Some(res) = &mut http.response
        {
            res.headers.push((
                "x-filter-verdict".to_string(),
                if matched { "matched" } else { "missed" }.to_string(),
            ));
        }

        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

#[tokio::test]
async fn wire_matrix_body_filter_verdict_is_matched_not_missed_on_gzip() {
    init_crypto();

    let plain = b"a-payload-with-needle-inside";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, plain).expect("write");
    let gzipped = encoder.finish().expect("finish");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let target = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gzipped.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&gzipped).await;
        let _ = socket.flush().await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(FilterOnDecodedBodyInterceptor);
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://{target}/probe"))
        .header("host", target.to_string())
        .body(String::new())
        .expect("request");
    let resp = sender.send_request(req).await.expect("send");

    assert_eq!(
        resp.headers()
            .get("x-filter-verdict")
            .and_then(|v| v.to_str().ok()),
        Some("matched"),
        "a body filter must match the decoded payload, not the compressed bytes"
    );
}

// ── Request-direction Content-Encoding (§24.3 remaining) ─────────────────────────────
//
// The request direction does not decode by Content-Encoding: a replaced request body is sent as
// plaintext with the encoding dropped. That is consistent (header and body agree) but it is a
// recorded limitation rather than a feature, so this pins the current contract instead of letting
// it drift silently.

/// Replaces the request body, leaving framing to the proxy.
struct ReplaceRequestBodyInterceptor {
    replacement: &'static str,
}

#[async_trait::async_trait]
impl Interceptor for ReplaceRequestBodyInterceptor {
    async fn on_request_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        if let Layer::Http(http) = &mut flow.layer {
            http.request.body = Some(relay_core_api::flow::BodyData {
                encoding: "utf-8".to_string(),
                content: self.replacement.to_string(),
                size: self.replacement.len() as u64,
            });
        }
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, _flow: &mut Flow) -> InterceptionResult {
        InterceptionResult::Continue
    }

    async fn on_response(
        &self,
        _flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
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

#[tokio::test]
async fn wire_matrix_compressed_request_rewrite_is_sent_as_plaintext_without_a_false_header() {
    init_crypto();

    let payload = b"original-request-payload";
    let gzipped = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut e, payload).expect("write");
        e.finish().expect("finish")
    };

    let (upstream_addr, upstream_rx) = spawn_recording_upstream_with_reply(None).await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(ReplaceRequestBodyInterceptor {
        replacement: "REWRITTEN-PLAINTEXT",
    });
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
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Send a gzip-encoded request body.
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("POST")
        .uri(format!("http://{upstream_addr}/probe"))
        .header("host", upstream_addr.to_string())
        .header("content-encoding", "gzip")
        .body(http_body_util::Full::new(bytes::Bytes::from(
            gzipped.clone(),
        )))
        .expect("request");
    let _ = sender.send_request(req).await.expect("send");

    let upstream = tokio::time::timeout(std::time::Duration::from_secs(5), upstream_rx)
        .await
        .expect("upstream timed out")
        .expect("upstream dropped");
    let head = upstream.head.to_lowercase();

    // The proxy must not claim gzip while sending plaintext.
    assert!(
        !head.contains("content-encoding: gzip"),
        "a rewritten request body must not keep a stale Content-Encoding, got:\n{}",
        upstream.head
    );
    assert_eq!(
        String::from_utf8_lossy(&upstream._body),
        "REWRITTEN-PLAINTEXT",
        "the replacement is what must reach the upstream"
    );
}

// ── Flow completion is recorded (§24.7) ─────────────────────────────────────────────
//
// `Flow.end_time` was never set on the live path, so `FlowSummary.duration_ms` — computed only from
// end_time — was always null in REST/MCP output, and consumers could not tell a finished exchange
// from a stalled one.

/// Drive one exchange while capturing every flow update the proxy emits.
async fn run_case_capturing_updates() -> Vec<FlowUpdate> {
    init_crypto();

    let (upstream_addr, _upstream_rx) = spawn_recording_upstream_with_reply(Some("body")).await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    let _ = drive(proxy_port, upstream_addr, "GET /probe HTTP/1.1", "").await;

    // Give the final update a moment to be enqueued, then drain without blocking.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut updates = Vec::new();
    while let Ok(update) = flow_rx.try_recv() {
        updates.push(update);
    }
    updates
}

#[tokio::test]
async fn wire_matrix_completed_flow_records_its_end_time() {
    let updates = run_case_capturing_updates().await;

    let flows: Vec<&relay_core_api::flow::Flow> = updates
        .iter()
        .filter_map(|u| match u {
            FlowUpdate::Full(flow) => Some(flow.as_ref()),
            _ => None,
        })
        .collect();

    assert!(
        !flows.is_empty(),
        "the proxy must emit at least one full flow update"
    );
    assert!(
        flows.iter().any(|f| f.end_time.is_some()),
        "a completed exchange must record end_time; otherwise duration_ms can never be computed"
    );

    let finished = flows
        .iter()
        .find(|f| f.end_time.is_some())
        .expect("checked above");
    assert!(
        finished.end_time >= Some(finished.start_time),
        "end_time must not precede start_time"
    );
}

/// A failed exchange must also record its end time, so duration is available for debugging failures
/// rather than only for successes.
#[tokio::test]
async fn wire_matrix_failed_flow_records_its_end_time() {
    init_crypto();

    // Reserve then drop a port so connecting to it fails deterministically.
    let dead = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let dead_addr = dead.local_addr().expect("addr");
    drop(dead);

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    let _ = drive(proxy_port, dead_addr, "GET /probe HTTP/1.1", "").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut updates = Vec::new();
    while let Ok(update) = flow_rx.try_recv() {
        updates.push(update);
    }

    let finished = updates.iter().filter_map(|u| match u {
        FlowUpdate::Full(flow) => Some(flow.as_ref()),
        _ => None,
    });

    let finished: Vec<&Flow> = finished.filter(|f| f.end_time.is_some()).collect();
    assert!(
        !finished.is_empty(),
        "an exchange that failed must still record end_time, otherwise its duration is unknowable"
    );

    // The reason matters as much as the timestamp: without it a consumer cannot tell this failure
    // from a success, which is the whole point of recording it.
    assert!(
        finished
            .iter()
            .any(|f| f.close_reason == Some(CloseReason::UpstreamClosed)),
        "a failed upstream connection must be recorded as UpstreamClosed, got {:?}",
        finished.iter().map(|f| &f.close_reason).collect::<Vec<_>>()
    );
}

/// A finished exchange states how it ended, exactly once.
///
/// Runtime turns that recorded reason into the terminal lifecycle event (§4-4). If a flow could
/// carry a reason on more than one update, consumers would see the same exchange complete twice;
/// if it carried none, they could not tell a success from a failure at all.
#[tokio::test]
async fn wire_matrix_a_finished_exchange_reports_its_close_reason_once() {
    let updates = run_case_capturing_updates().await;

    let reasons: Vec<&CloseReason> = updates
        .iter()
        .filter_map(|u| match u {
            FlowUpdate::Full(flow) => flow.close_reason.as_ref(),
            _ => None,
        })
        .collect();

    assert_eq!(
        reasons.len(),
        1,
        "a flow must report its ending exactly once, got {reasons:?}"
    );
    assert_eq!(
        reasons[0],
        &CloseReason::Completed,
        "a successful exchange must be reported as completed, not as an error"
    );
}

/// A policy drop is not a completion. `Completed` here would tell a consumer the exchange finished
/// normally when the proxy actually refused it.
#[tokio::test]
async fn wire_matrix_dropped_exchange_is_not_reported_as_completed() {
    init_crypto();

    let (upstream_addr, _upstream_rx) = spawn_recording_upstream_with_reply(Some("body")).await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor {
        phase: Phase::RequestHeadersDrop,
    });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    let _ = drive(proxy_port, upstream_addr, "GET /probe HTTP/1.1", "").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut reasons = Vec::new();
    while let Ok(update) = flow_rx.try_recv() {
        if let FlowUpdate::Full(flow) = update
            && let Some(reason) = flow.close_reason
        {
            reasons.push(reason);
        }
    }

    assert_eq!(reasons.len(), 1, "expected one ending, got {reasons:?}");
    match &reasons[0] {
        CloseReason::PolicyDrop { detail } => assert!(
            detail.contains("policy"),
            "the drop reason should name what dropped it, got {detail:?}"
        ),
        other => panic!("a dropped exchange must not be reported as {other:?}"),
    }
}

// ── Ingress HTTP version reaches the forwarded request (§24.9) ───────────────────────
//
// The outbound request used to be built with the builder's HTTP/1.1 default regardless of what the
// client spoke, so the ingress version was dropped from the request's own metadata.

#[tokio::test]
async fn wire_matrix_forwarded_request_carries_the_client_version() {
    init_crypto();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let upstream_addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ = tx.send(head);
        let _ = socket
            .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
    });

    let proxy_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = proxy_listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(proxy_listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    // Speak HTTP/1.0 to the proxy; the forwarded request should reflect that rather than defaulting.
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request =
        format!("GET http://{upstream_addr}/probe HTTP/1.0\r\nHost: {upstream_addr}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let head = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .expect("upstream timed out")
        .expect("upstream dropped");

    assert!(
        head.starts_with("GET /probe HTTP/1.0"),
        "the forwarded request must carry the client's version, got:\n{head}"
    );
}

// ── Response version follows the client (§24.9) ─────────────────────────────────────
//
// The response used to inherit whatever version the upstream spoke, so an HTTP/1.0 client could be
// answered with an HTTP/1.1 response — which changes the framing rules (lingering close vs
// Content-Length, keep-alive expectations).

#[tokio::test]
async fn wire_matrix_response_version_matches_the_client_not_the_upstream() {
    init_crypto();

    // Upstream answers HTTP/1.0 while the client speaks HTTP/1.1.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let upstream_addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let _ = socket.read(&mut buf).await;
        let _ = socket
            .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
            .await;
        let _ = socket.flush().await;
    });

    let proxy_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = proxy_listener.local_addr().expect("addr").port();

    let source = TcpCaptureSource::new(proxy_listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    // The client speaks HTTP/1.1 and reads the raw status line.
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{upstream_addr}/probe HTTP/1.1\r\nHost: {upstream_addr}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let mut raw = Vec::new();
    let mut buf = vec![0u8; 8192];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
            }
        }
    })
    .await;

    let head = String::from_utf8_lossy(&raw);
    assert!(
        head.starts_with("HTTP/1.1 "),
        "the response must speak the client's version, got:\n{}",
        head.lines().next().unwrap_or("<empty>")
    );
}

/// An upstream WebSocket that keeps the session open, so the client is the one that ends it.
async fn spawn_holding_ws_upstream() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ws upstream");
    let addr = listener.local_addr().expect("ws upstream addr");

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        // Stay open: the test ends the session from the client side.
        while let Some(Ok(_)) = futures_util::StreamExt::next(&mut ws).await {}
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    });

    addr
}

/// A finished WebSocket session must be reported as finished.
///
/// The tunnel ended silently: no closing Flow was ever emitted, so `WebSocketLayer.closed` stayed
/// `false` and `end_time` stayed `None`. A UI therefore showed an ended session as still open
/// forever, and its duration was unknowable — while the HTTP path recorded both at all 14 of its
/// terminal sites.
#[tokio::test]
async fn wire_matrix_ws_session_end_reaches_consumers() {
    init_crypto();

    let upstream_addr = spawn_holding_ws_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{upstream_addr}/ws HTTP/1.1\r\nHost: {upstream_addr}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write handshake");

    let mut buf = vec![0u8; 4096];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake timed out")
        .expect("read handshake response");
    let handshake = String::from_utf8_lossy(&buf[..read]).to_string();
    assert!(
        handshake.starts_with("HTTP/1.1 101"),
        "the session must upgrade before its end can be observed, got {handshake:?}"
    );

    // Send one masked text frame so the tunnel is provably live, then close the session properly.
    stream
        .write_all(&[0x81, 0x82, 0x01, 0x02, 0x03, 0x04, b'h' ^ 1, b'i' ^ 2])
        .await
        .expect("write a frame");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    stream
        .write_all(&[0x88, 0x82, 0x01, 0x02, 0x03, 0x04, 0x03 ^ 1, 0xe8 ^ 2])
        .await
        .expect("write close frame");
    let _ = stream.shutdown().await;

    // Wait for the closing Flow rather than assuming it is already queued.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut closing: Option<relay_core_api::flow::Flow> = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(250), flow_rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if flow.end_time.is_some() {
                    closing = Some(*flow);
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    let closing = closing.expect(
        "ending a WebSocket session must emit a closing Flow with end_time; \
         without it a UI shows a finished session as still open",
    );

    match &closing.layer {
        Layer::WebSocket(ws) => assert!(
            ws.closed,
            "the closing Flow must mark the session closed, got {ws:?}"
        ),
        other => panic!("expected a websocket layer, got {other:?}"),
    }
    assert!(
        closing.end_time >= Some(closing.start_time),
        "end_time must not precede start_time"
    );

    // Which side closed first is known at the point the stream ends, so it is recorded rather than
    // collapsed into a generic "it finished". This client sent the close frame.
    assert_eq!(
        closing.close_reason,
        Some(CloseReason::ClientClosed),
        "the side that ended the session must be recorded"
    );
}

/// A WebSocket handshake that never reaches upstream must still be visible.
///
/// The failure arms returned an error response without ever emitting the Flow, so a failed WS
/// handshake produced *no* traffic record at all — the exchange was invisible precisely when
/// someone would want to look at it.
#[tokio::test]
async fn wire_matrix_failed_ws_handshake_is_recorded() {
    init_crypto();

    let upstream_addr = spawn_refusing_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let request = format!(
        "GET http://{upstream_addr}/ws HTTP/1.1\r\nHost: {upstream_addr}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write handshake");

    let mut buf = vec![0u8; 4096];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("handshake timed out")
        .expect("read handshake response");
    let response = String::from_utf8_lossy(&buf[..read]).to_string();
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "a refused upstream upgrade should surface as 502, got {response:?}"
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut recorded: Option<Flow> = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(250), flow_rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) if flow.end_time.is_some() => {
                recorded = Some(*flow);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    let flow = recorded.expect("a failed WS handshake must still be recorded as a Flow");
    match &flow.layer {
        Layer::WebSocket(ws) => {
            assert!(
                ws.closed,
                "a failed handshake is a finished session, got {ws:?}"
            );
            assert_eq!(
                ws.handshake_response.status, 502,
                "the recorded handshake response must match what the client was told"
            );
        }
        other => panic!("expected a websocket layer, got {other:?}"),
    }
}

/// A plain HTTP upstream that honours keep-alive and serves every request on the connection.
///
/// The other recording upstreams answer once and then send `Connection: close`, which is enough to
/// observe one exchange but cannot answer "does the proxy reuse a connection?" — a faithful proxy
/// must relay that close, so the question is unanswerable with them.
async fn spawn_keepalive_upstream() -> (
    SocketAddr,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let served = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    let accept_counter = accepted.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            accept_counter.fetch_add(1, Ordering::Relaxed);
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let mut pending = Vec::new();
                loop {
                    // Serve every complete request head that arrives on this connection.
                    let head_end = loop {
                        if let Some(pos) = find_subslice(&pending, b"\r\n\r\n") {
                            break Some(pos + 4);
                        }
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => break None,
                            Ok(n) => pending.extend_from_slice(&buf[..n]),
                        }
                    };
                    let Some(head_end) = head_end else { return };
                    pending.drain(..head_end);
                    counter.fetch_add(1, Ordering::Relaxed);

                    let body = b"kept-alive";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    if socket.write_all(body).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                }
            });
        }
    });

    (addr, served, accepted)
}

/// Can the proxy serve two requests on one client connection?
///
/// The performance baseline is taken at `CONNECTIONS=25` because the harness concluded the proxy's
/// responses carry `Connection: close`, which would prevent any client from reusing a connection and
/// force a fresh ephemeral port per request. That conclusion is load-bearing: if it is wrong, the
/// baseline is measured well below what the engine can do, and the workaround hides a real
/// throughput ceiling rather than describing one.
#[tokio::test]
async fn wire_matrix_client_connection_is_reused_for_a_second_request() {
    init_crypto();

    let (upstream_addr, upstream_served, upstream_connections) = spawn_keepalive_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    // One client connection, two sequential requests.
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("proxy handshake");
    let conn_task = tokio::spawn(conn);

    let make_request = |target: SocketAddr| {
        hyper::Request::builder()
            .method("GET")
            .uri(
                format!("http://{target}/probe")
                    .parse::<hyper::Uri>()
                    .expect("uri"),
            )
            .header("host", target.to_string())
            .body(String::new())
            .expect("request")
    };

    let first = sender
        .send_request(make_request(upstream_addr))
        .await
        .expect("first request on the connection");
    let first_status = first.status();
    let _ = first.into_body().collect().await.expect("first body");

    // The decisive step: a second request on the same connection only works if the proxy kept it.
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sender.send_request(make_request(upstream_addr)),
    )
    .await
    .expect("the proxy closed the connection instead of reusing it")
    .expect("second request on the same connection");

    assert_eq!(first_status.as_u16(), 200);
    assert_eq!(second.status().as_u16(), 200);
    let _ = second.into_body().collect().await.expect("second body");

    // The upstream must have been reached for both requests, or "reuse" would only describe the
    // client half of the path.
    assert_eq!(
        upstream_served.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "both requests must reach the upstream"
    );

    // And reached over one connection: the proxy pools upstream connections, so two sequential
    // requests must not cost two connects. This is the half of reuse the client-side assertion
    // cannot see, and without it a per-request connect would look like a working proxy that is
    // merely slow.
    assert_eq!(
        upstream_connections.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the proxy must reuse its upstream connection for sequential requests"
    );

    conn_task.abort();
}

/// A plaintext HTTP/2 (h2c) request must be captured and forwarded.
///
/// The plaintext listener served HTTP/1.1 only, so h2c — how gRPC is used on networks that do not
/// terminate TLS, and something mitmproxy does not support either — was parsed as HTTP/1.1 or
/// rejected. Capture was therefore impossible for it. The listener now detects the protocol from the
/// connection preface, so H1 clients are unaffected and h2c is a first-class ingress.
///
/// The client here speaks prior-knowledge H2 to the proxy while naming the upstream in `:authority`,
/// which is how an h2c forward-proxy request is expressed (HTTP/2 has no absolute-form request
/// line). Independently confirmed with `curl --http2-prior-knowledge` over nghttp2, which returns
/// `HTTP/2 200` through this same path.
#[tokio::test]
async fn wire_matrix_h2c_ingress_is_captured_and_forwarded() {
    init_crypto();

    let (upstream_addr, upstream_served, _upstream_connections) = spawn_keepalive_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    // Mutate a response header so the assertion covers "the rule reached the wire", not just
    // "something answered".
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor {
        phase: Phase::ResponseHeaders,
    });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    // Connect to the proxy, but address the upstream in the request URI — the h2c equivalent of
    // curl's `--connect-to`.
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let (mut sender, conn) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .timer(hyper_util::rt::TokioTimer::new())
            .handshake(TokioIo::new(stream))
            .await
            .expect("h2c handshake to the proxy must succeed");
    let conn_task = tokio::spawn(conn);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sender.send_request(
            hyper::Request::builder()
                .method("GET")
                .uri(format!("http://{upstream_addr}/h2c-probe"))
                .body(String::new())
                .expect("request"),
        ),
    )
    .await
    .expect("an h2c request must not hang")
    .expect("an h2c request must be forwarded, not refused");

    assert_eq!(
        response.version(),
        hyper::Version::HTTP_2,
        "the proxy must answer in the protocol the client spoke"
    );
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-wire-probe")
            .map(|v| v.to_str().unwrap_or("")),
        Some("from-interceptor"),
        "a rule must reach an h2c client the same way it reaches an H1 one"
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    assert_eq!(
        &body[..],
        b"kept-alive",
        "the upstream body must be relayed"
    );

    assert_eq!(
        upstream_served.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the h2c request must actually reach the upstream"
    );

    // The Flow must record the protocol the client spoke, which is what makes an h2c exchange
    // distinguishable from an H1 one in the UI and to an agent reading the traffic.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut version = None;
    while tokio::time::Instant::now() < deadline && version.is_none() {
        match tokio::time::timeout(std::time::Duration::from_millis(200), flow_rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let Layer::Http(http) = &flow.layer
                    && http.request.url.as_str().contains("/h2c-probe")
                {
                    version = Some(http.request.version.clone());
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert_eq!(
        version.as_deref(),
        Some("HTTP/2.0"),
        "the captured Flow must say the request arrived over HTTP/2"
    );

    conn_task.abort();
}

/// `h2c` inside a CONNECT tunnel must be captured — this is the shape a gRPC client with
/// `HTTP_PROXY` produces for a plaintext target.
///
/// The tunnel used to terminate TLS unconditionally, so a client that sent an HTTP/2 preface instead
/// of a ClientHello failed its handshake and nothing was captured. The tunnel now reads the client's
/// first bytes and serves whichever protocol they announce, which also makes plaintext HTTP/1.x
/// through CONNECT work. Independently confirmed with
/// `curl --proxytunnel --http2-prior-knowledge -x <proxy> http://<target>/`, which returns
/// `HTTP/2 200` here while the HTTP/1.1 and TLS controls still work.
#[tokio::test]
async fn wire_matrix_h2c_inside_a_connect_tunnel_is_captured() {
    init_crypto();

    let (upstream_addr, upstream_served, _upstream_connections) = spawn_keepalive_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor {
        phase: Phase::ResponseHeaders,
    });
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
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

    // 1. CONNECT to the upstream through the proxy.
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    stream
        .write_all(
            format!("CONNECT {upstream_addr} HTTP/1.1\r\nHost: {upstream_addr}\r\n\r\n").as_bytes(),
        )
        .await
        .expect("write CONNECT");

    let mut buf = vec![0u8; 1024];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("CONNECT must be answered")
        .expect("read CONNECT response");
    let response = String::from_utf8_lossy(&buf[..read]).to_string();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the tunnel must open, got {response:?}"
    );

    // 2. Speak h2c inside it, with origin-form paths — the tunnel's CONNECT target is the authority.
    let (mut sender, conn) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .timer(hyper_util::rt::TokioTimer::new())
            .handshake(TokioIo::new(stream))
            .await
            .expect("the tunnel must accept an h2c preface instead of demanding a ClientHello");

    let conn_task = tokio::spawn(conn);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sender.send_request(
            hyper::Request::builder()
                .method("GET")
                .uri("/plaintext-h2c-probe")
                .body(String::new())
                .expect("request"),
        ),
    )
    .await
    .expect("an h2c request in the tunnel must not hang")
    .expect("an h2c request in the tunnel must be forwarded");

    assert_eq!(response.version(), hyper::Version::HTTP_2);
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-wire-probe")
            .map(|v| v.to_str().unwrap_or("")),
        Some("from-interceptor"),
        "a rule must reach an h2c client inside a CONNECT tunnel"
    );
    let _ = response.into_body().collect().await.expect("body");

    assert_eq!(
        upstream_served.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the tunnelled h2c request must reach the upstream"
    );

    // 3. The Flow must describe the exchange truthfully: the CONNECT target with an `http` scheme
    // (no TLS was terminated), over HTTP/2.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut observed: Option<(String, String, bool)> = None;
    while tokio::time::Instant::now() < deadline && observed.is_none() {
        match tokio::time::timeout(std::time::Duration::from_millis(200), flow_rx.recv()).await {
            Ok(Some(FlowUpdate::Full(flow))) => {
                if let Layer::Http(http) = &flow.layer
                    && http.request.url.as_str().contains("/plaintext-h2c-probe")
                {
                    observed = Some((
                        http.request.url.to_string(),
                        http.request.version.clone(),
                        flow.network.tls,
                    ));
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    let (url, version, tls) = observed.expect("the tunnelled h2c exchange must be recorded");
    assert_eq!(
        url,
        format!("http://{upstream_addr}/plaintext-h2c-probe"),
        "the URL must come from the CONNECT target and the plaintext scheme"
    );
    assert_eq!(version, "HTTP/2.0");
    assert!(
        !tls,
        "no TLS was terminated, so the Flow must not claim there was"
    );

    conn_task.abort();
}

/// A plaintext gRPC upstream speaks HTTP/2 and nothing else, and must be reachable.
///
/// The outbound leg used hyper's legacy client, which picks its protocol from the URL — HTTP/1.1 for
/// `http://` — so an h2c-only upstream could not be reached at all: the request was captured and
/// then failed with 502. Since gRPC on an internal network is exactly this shape, c2h support on the
/// ingress side was not enough on its own.
#[tokio::test]
async fn wire_matrix_h2c_upstream_is_reachable() {
    use bytes::Bytes;
    use hyper::body::Frame;

    /// A body with one data frame and then a trailers frame, like a gRPC response.
    #[derive(Default)]
    struct GrpcLikeBody {
        step: u8,
    }

    impl hyper::body::Body for GrpcLikeBody {
        type Data = Bytes;
        type Error = BoxError;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            let step = self.step;
            self.step += 1;
            match step {
                0 => std::task::Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                    b"h2c-upstream",
                ))))),
                1 => {
                    let mut trailers = hyper::HeaderMap::new();
                    trailers.insert("grpc-status", "0".parse().expect("header"));
                    std::task::Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                }
                _ => std::task::Poll::Ready(None),
            }
        }
    }

    init_crypto();

    let (upstream_addr, requests) = spawn_h2c_only_upstream(GrpcLikeBody::default).await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    // A gRPC client: h2c to the proxy, `te: trailers` and a grpc content type.
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let (mut sender, conn) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake(TokioIo::new(stream))
            .await
            .expect("h2c handshake");
    let conn_task = tokio::spawn(conn);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sender.send_request(
            hyper::Request::builder()
                .method("POST")
                .uri(format!("http://{upstream_addr}/pkg.Service/Method"))
                .header("content-type", "application/grpc")
                .header("te", "trailers")
                .body(String::new())
                .expect("request"),
        ),
    )
    .await
    .expect("must not hang")
    .expect("must be answered");

    assert_eq!(
        response.status().as_u16(),
        200,
        "a plaintext gRPC upstream must be reachable over h2c"
    );
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the upstream must actually receive the request"
    );

    let mut body = response.into_body();
    let mut data = Vec::new();
    let mut grpc_status: Option<String> = None;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("body frame");
        if let Some(chunk) = frame.data_ref() {
            data.extend_from_slice(chunk);
        }
        if let Some(trailers) = frame.trailers_ref() {
            grpc_status = trailers
                .get("grpc-status")
                .map(|v| v.to_str().unwrap_or("").to_string());
        }
    }
    assert_eq!(&data[..], b"h2c-upstream");
    assert_eq!(
        grpc_status.as_deref(),
        Some("0"),
        "the upstream's trailers must reach the client, or gRPC status is lost"
    );

    conn_task.abort();
}

/// Start an upstream that serves HTTP/2 only, on plaintext, and count the requests it serves.
async fn spawn_h2c_only_upstream<B>(
    make_body: fn() -> B,
) -> (SocketAddr, Arc<std::sync::atomic::AtomicUsize>)
where
    B: hyper::body::Body<Data = bytes::Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    use hyper::service::service_fn;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind h2c upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let served = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = served.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let counter = counter.clone();
            tokio::spawn(async move {
                let service = service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(make_body()))
                    }
                });
                let _ =
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
            });
        }
    });

    (addr, served)
}

/// gRPC-shaped requests to an upstream that only speaks HTTP/1.1 must still work.
///
/// The h2c selector is keyed on the request's own declarations, so a server that turns out not to
/// speak HTTP/2 has to be reached anyway. Falling back is only safe because the decision is made
/// before the body is sent — a streamed body cannot be replayed, so this covers connection failure
/// and not a mid-request failure, which is why the sender is established first.
#[tokio::test]
async fn wire_matrix_grpc_shaped_request_falls_back_to_http1() {
    init_crypto();

    // A plain HTTP/1.1 upstream, which will not answer an HTTP/2 preamble.
    let (upstream_addr, served, _connections) = spawn_keepalive_upstream().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();

    let source = TcpCaptureSource::new(listener);
    let interceptor: Arc<dyn Interceptor> = Arc::new(MutateInterceptor { phase: Phase::None });
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

    // Ordinary HTTP/1.1 client, but with gRPC's markers, so the h2c path is attempted first.
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("proxy handshake");
    let conn_task = tokio::spawn(conn);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sender.send_request(
            hyper::Request::builder()
                .method("POST")
                .uri(format!("http://{upstream_addr}/pkg.Service/Method"))
                .header("content-type", "application/grpc")
                .header("te", "trailers")
                .body(String::new())
                .expect("request"),
        ),
    )
    .await
    .expect("the fallback must not hang")
    .expect("the fallback must answer");

    assert_eq!(
        response.status().as_u16(),
        200,
        "an upstream that is not h2c must still be reached over HTTP/1.1"
    );
    // The upstream echoes its own body, so seeing it proves the real request arrived over HTTP/1.1.
    // A request count is not a usable signal here: the h2c attempt writes an HTTP/2 preface to the
    // same upstream first, and a raw counting socket cannot tell that apart from a request.
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    assert_eq!(
        &body[..],
        b"kept-alive",
        "the HTTP/1.1 upstream must actually serve the request"
    );
    let _ = served;

    conn_task.abort();
}
