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
            wants_observation: false,
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
