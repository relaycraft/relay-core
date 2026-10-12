use crate::event::CloseReason;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

/// Whether this flow should be presented as failed.
///
/// The single definition of "an error" for every host and every derivation: the HTTP API's summary
/// (`FlowSummary::has_error`, which is also the persisted column behind the `?has_error=` filter),
/// the filter language's `err` token, the CLI and TUI, and the web UI's live rows — mirrored in
/// `webui/src/lib/flowSummary.ts`, which must keep the same rule.
///
/// It is written once because it used to be written four times, each slightly differently, and the
/// differences were visible in the interface rather than in a test: an upstream failure was an error
/// in the browser (which had just received the `Flow`) and not in the summary (which is what a
/// refresh reloads), so the error badge on a row disappeared when the page was reloaded.
///
/// An upstream failure counts even though it has no status code: the request never got an answer,
/// which is the most useful thing "error" can mean in a proxy.
pub fn flow_is_error(flow: &Flow) -> bool {
    if flow.tags.iter().any(|tag| tag == "error") {
        return true;
    }
    // A reset is the peer cancelling mid-exchange: the captured bytes are a prefix, not a result.
    if matches!(flow.close_reason, Some(CloseReason::Reset))
        || flow.tags.iter().any(|tag| tag == "stream-reset")
    {
        return true;
    }
    match &flow.layer {
        Layer::Http(http) => {
            if http.error.is_some() {
                return true;
            }
            let Some(response) = http.response.as_ref() else {
                return false;
            };
            if response.status >= 500 {
                return true;
            }
            let (grpc_status, _) = crate::grpc::outcome_from_trailers(&response.trailers);
            grpc_status.is_some_and(|code| code != 0)
        }
        Layer::WebSocket(ws) => ws.handshake_response.status >= 500,
        _ => false,
    }
}

/// Trait for custom protocol layers to implement
pub trait ProtocolLayer: Any + Send + Sync + std::fmt::Debug {
    fn protocol_name(&self) -> &str;
    fn as_any(&self) -> &dyn Any;
    fn to_json(&self) -> serde_json::Value;
}

// Custom serializer for Arc<dyn ProtocolLayer>
fn serialize_custom_layer<S>(
    layer: &Arc<dyn ProtocolLayer>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeStruct;
    let mut state = serializer.serialize_struct("CustomLayer", 2)?;
    state.serialize_field("protocol", layer.protocol_name())?;
    state.serialize_field("data", &layer.to_json())?;
    state.end()
}

// Custom deserializer for Arc<dyn ProtocolLayer>
fn deserialize_custom_layer<'de, D>(deserializer: D) -> Result<Arc<dyn ProtocolLayer>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;

    // Try to extract protocol and data
    let protocol = value
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let data = value
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    Ok(Arc::new(GenericProtocolLayer { protocol, data }))
}

#[derive(Debug)]
struct GenericProtocolLayer {
    protocol: String,
    data: serde_json::Value,
}

impl ProtocolLayer for GenericProtocolLayer {
    fn protocol_name(&self) -> &str {
        &self.protocol
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn to_json(&self) -> serde_json::Value {
        self.data.clone()
    }
}

fn u32_is_zero(v: &u32) -> bool {
    *v == 0
}

/// Structured trace of resilience-related events during a proxy request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResilienceTrace {
    /// Number of retry attempts for this request.
    /// Currently unused; reserved for future retry support.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub retry_count: u32,
    /// Whether the circuit breaker was open when this request was attempted.
    #[serde(default)]
    pub circuit_open: bool,
    /// Whether the request body budget was exceeded (rules skipped).
    #[serde(default)]
    pub budget_exceeded: bool,
    /// Upstream errors encountered (non-2xx, connection errors).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_errors: Vec<String>,
    /// P3: Timeout classification (e.g., "connect", "read", "total", "tls").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_type: Option<String>,
}

/// The central data structure representing a captured traffic flow.
/// Designed to support L3/L4 layers initially, with L7 (HTTP) as a specialized layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub id: Uuid,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,

    /// L3/L4 Connection Info (IP, Port, Protocol)
    pub network: NetworkInfo,

    /// The application layer protocol detected or parsed
    pub layer: Layer,

    /// Analysis tags (e.g., "error", "large-body", "injected")
    pub tags: Vec<String>,

    /// Internal processing metadata (not necessarily for UI)
    #[serde(skip)]
    pub meta: HashMap<String, String>,

    /// P4: Resilience trace (circuit breaker, retries, budget exceeded)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resilience_trace: Option<ResilienceTrace>,

    /// S11: Rule engine variables (SetVariable action results) exposed to scripts.
    /// Cleared at end of flow lifecycle.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rule_variables: HashMap<String, String>,

    /// S10a: Rule IDs that matched and executed on this flow (exposed to scripts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_rules: Vec<String>,

    /// How the exchange ended, in the unified vocabulary of roadmap §4-3.
    ///
    /// `None` means "not recorded" — either the flow is still open, or it ended somewhere that does
    /// not yet classify its ending. It is deliberately not defaulted to `Completed`: an unrecorded
    /// ending and a confirmed normal completion are different facts, and reporting the second when
    /// only the first is known is what made errors indistinguishable from successes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<crate::event::CloseReason>,

    /// Which client connection this exchange ran on (roadmap §4-1).
    ///
    /// One id per accepted connection, shared by every exchange multiplexed over it — which is what
    /// makes HTTP/2 streams groupable and what lets a consumer answer "what else happened on this
    /// connection" after the fact. It is an id this engine assigns, not a wire value: the peers that
    /// model this (mitmproxy's `Connection.id`, HAR's `entry.connection`, Envoy's `%CONNECTION_ID%`)
    /// all do the same, and mitmproxy records the reason — object identity is enough while a flow is
    /// live, but a *recorded* flow still has to express it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,

    /// The CONNECT flow this exchange arrived inside, when it arrived through a tunnel (roadmap §4-1).
    ///
    /// A client that sends `CONNECT example.com:443` produces one flow for the instruction itself and
    /// then one flow per request inside the tunnel; without this pointer the two are only related by
    /// the connection id, and "which tunnel did this request come out of" cannot be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_flow_id: Option<Uuid>,

    /// The protocols this exchange actually travelled over, outermost first (roadmap §4-1).
    ///
    /// Observed facts, not configuration: e.g. `["tcp", "tls", "h2"]` for HTTP/2 inside a MITM'd
    /// tunnel, `["tcp", "h2c"]` for prior-knowledge HTTP/2 in the clear, `["udp"]` for a datagram
    /// session. mitmproxy keeps the same facts (`transport_protocol`, `tls`, `tls_version`, `alpn`)
    /// and Proxyman shows the negotiated protocol; recording them as one stack is what makes
    /// "gRPC spoke h2 over TLS here and h2c there" answerable from the capture alone.
    ///
    /// This is *not* ALPN: the application protocol below is the version hyper parsed, so a
    /// connection whose ALPN negotiated `h2` but which then spoke HTTP/1.1 is recorded as `h1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_stack: Vec<String>,
}

/// Tag on the CONNECT instruction flow itself.
///
/// A tunnel is a real exchange and is recorded as one, but it is not a request a person is
/// inspecting: the peers that record it treat it as something other than an ordinary transaction
/// (mitmproxy raises `http_connect` and documents that CONNECT produces none of the usual handler
/// events; Proxyman puts connection detail in a panel of its own). The tag is the key a host filters
/// on, so "we recorded it" and "we show it in the request list" stay independent decisions.
pub const CONNECT_TAG: &str = "connect";

/// The protocol stack of an HTTP exchange: the outermost layer first.
///
/// Derived from facts this engine observed — the transport it accepted, whether it terminated TLS,
/// and the HTTP version the request was parsed as. It is deliberately not the ALPN result: ALPN is
/// what was *offered and negotiated*, which is a different fact, so a connection that negotiated
/// `h2` and then spoke HTTP/1.1 is recorded as `h1` rather than as `h2`.
pub fn http_protocol_stack(tls_terminated: bool, http_version: &str) -> Vec<String> {
    let mut stack = vec!["tcp".to_string()];
    if tls_terminated {
        stack.push("tls".to_string());
    }
    // `HTTP/2` and `HTTP/2.0` both appear: the version is carried as a string in the capture, and
    // hyper's `{:?}` and `Display` forms differ.
    let application = if http_version.starts_with("HTTP/2") {
        // `h2c` is HTTP/2 over cleartext, and it is reachable here without any TLS at all — through
        // the plaintext listener's preface sniff or a CONNECT tunnel to a plaintext gRPC target.
        Some(if tls_terminated { "h2" } else { "h2c" })
    } else if http_version.starts_with("HTTP/3") {
        Some("h3")
    } else if http_version.starts_with("HTTP/1") {
        Some("h1")
    } else {
        // An unrecognised version is not worth guessing at: a stack that stops after the layers we
        // actually observed is more useful than one that names a protocol we inferred.
        None
    };
    if let Some(application) = application {
        stack.push(application.to_string());
    }
    stack
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub client_ip: String,
    pub client_port: u16,
    /// The peer that was actually connected to, when this engine made the connection.
    ///
    /// A forward-proxy request never connects to anything itself: it hands the URL to the HTTP client,
    /// so the resolved address is not knowable here and these stay at their placeholder. Reporting
    /// the placeholder as an address would be read as a real destination — use [`Self::server_host`]
    /// for what the request was aiming at.
    pub server_ip: String,
    pub server_port: u16,
    /// The target as requested, `host:port`, with the port filled in from the scheme when the URL
    /// omits it.
    ///
    /// This is what a person inspecting a forward-proxy flow wants to see, and unlike the resolved
    /// address it is known without a DNS lookup and cannot be wrong because of a round-robin answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_host: Option<String>,
    pub protocol: TransportProtocol,
    pub tls: bool,
    pub tls_version: Option<String>,
    pub sni: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransportProtocol {
    TCP,
    UDP,
    ICMP,
    Unknown,
}

/// The specific application layer data.
/// Using an enum allows us to support HTTP now, but easily add DNS, WebSocket,
/// or raw TCP streams later without breaking the top-level Flow structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Layer {
    Http(HttpLayer),
    WebSocket(WebSocketLayer),
    Tcp(TcpLayer),   // For raw TCP flows without L7 parsing
    Udp(UdpLayer),   // For raw UDP flows
    Quic(QuicLayer), // For QUIC flows
    #[serde(
        serialize_with = "serialize_custom_layer",
        deserialize_with = "deserialize_custom_layer"
    )]
    Custom(Arc<dyn ProtocolLayer>),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpLayer {
    pub payload_size: usize,
    pub packet_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicLayer {
    pub sni: Option<String>,
    pub alpn: Option<String>,
    pub version: Option<String>,
}

// --- HTTP Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpLayer {
    pub request: HttpRequest,
    pub response: Option<HttpResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub version: String,                // HTTP/1.1, HTTP/2
    pub headers: Vec<(String, String)>, // Vec preserves order, unlike HashMap
    pub cookies: Vec<Cookie>,
    pub query: Vec<(String, String)>,
    pub body: Option<BodyData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<Cookie>,
    pub body: Option<BodyData>,
    pub timing: ResponseTiming,
    /// HTTP trailers that arrived after the body.
    ///
    /// They are part of the response, not decoration: gRPC carries the outcome of a call in
    /// `grpc-status` / `grpc-message` trailers, so a capture without them can say a call happened
    /// but not whether it succeeded. Empty means the response had none, which is the common case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trailers: Vec<(String, String)>,

    /// Events parsed out of this response when it was a `text/event-stream` (roadmap §7.2).
    ///
    /// Two paths fill this and they answer different questions. While the stream is open, the events
    /// are pushed one at a time (`FlowUpdate::SseEvent`) because the response may never end, so a
    /// reader can watch it. Here they are recorded, so a capture opened *afterwards* still reads as
    /// events — a pane that showed nothing for a finished stream could not tell "no events" from "not
    /// recorded", which is the distinction this field exists to remove.
    ///
    /// Capped (see [`MAX_RECORDED_SSE_EVENTS`]); a stream longer than the cap says so on the flow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sse_events: Vec<crate::sse_stream::SseEvent>,
}

/// Most events recorded per response.
///
/// The body they come from is already bounded, but the event count is not the body size: a stream of
/// tiny ticks is many events in few bytes. Bounded because a capture is kept for days and an
/// unbounded per-response list is how one chatty stream becomes a storage problem.
pub const MAX_RECORDED_SSE_EVENTS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub path: Option<String>,
    pub domain: Option<String>,
    pub expires: Option<String>,
    pub http_only: Option<bool>,
    pub secure: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTiming {
    pub time_to_first_byte: Option<u64>, // ms
    pub time_to_last_byte: Option<u64>,  // ms
    /// TCP connect time in milliseconds for the upstream connection (new connections only).
    pub connect_time_ms: Option<u64>,
    /// TLS handshake time in milliseconds for the upstream connection (new connections only).
    pub ssl_time_ms: Option<u64>,
}

// --- WebSocket Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketLayer {
    pub handshake_request: HttpRequest,
    pub handshake_response: HttpResponse,
    pub messages: Vec<WebSocketMessage>,
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSocketMessage {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub direction: Direction, // ClientToServer, ServerToClient
    pub content: BodyData,
    pub opcode: String, // Text, Binary, Ping, Pong
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

// --- Raw TCP Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpLayer {
    pub bytes_up: u64,
    pub bytes_down: u64,
    // Future: packets or data chunks
}

// --- Shared ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BodyData {
    /// Encoding (e.g., "utf-8", "base64")
    pub encoding: String,
    /// The actual content. If binary, it should be base64 encoded string.
    pub content: String,
    /// Original size in bytes before any decoding/unzipping
    pub size: u64,
    /// Set when the body is gRPC, describing its message framing.
    ///
    /// A gRPC body is a sequence of length-prefixed messages, which is invisible in `content`: as
    /// base64 it is opaque, so without this a consumer can see that a call carried something but not
    /// how many messages or how large. The payloads themselves stay encoded — decoding them needs the
    /// service's protobuf schema, which the engine does not have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<crate::grpc::GrpcBody>,
}

/// A point in an exchange's life that the proxy declares (roadmap §4-4).
///
/// Three stages rather than one "started" flag because the facts differ in *when* they become true:
/// the first two are both established once the request head has been parsed, and the third only when
/// an upstream has answered. A consumer that only wants "is this flow alive" reads `Started`; one
/// that wants to time header arrival reads the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeStage {
    /// The exchange exists and its request metadata is known.
    Started,
    /// The client-direction headers are known.
    RequestHeaders,
    /// The upstream-direction headers are known.
    ResponseHeaders,
}

/// Enum for Incremental Flow Updates to avoid cloning the entire Flow object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum FlowUpdate {
    /// Full Flow update (e.g., initial creation, request/response headers)
    Full(Box<Flow>),
    /// Incremental WebSocket Message
    WebSocketMessage {
        flow_id: String,
        message: WebSocketMessage,
    },
    /// Incremental HTTP Body Update (Request or Response)
    HttpBody {
        flow_id: String,
        direction: Direction,
        body: BodyData,
        /// Whether the recorded body is a bounded prefix rather than all of it.
        ///
        /// Declared by the observer, which is the only place that knows: `BodyData` carries the
        /// observed *transfer* size, so nothing downstream can tell a complete 4 KiB body from the
        /// first 4 KiB of a 4 MiB one. Inferring it would put a guess into the typed event that the
        /// model exists to keep guesses out of.
        #[serde(default)]
        truncated: bool,
    },
    /// P1: Body budget exceeded notification
    BodyBudgetExceeded {
        flow_id: String,
        direction: Direction,
    },
    /// Trailers that arrived after the body.
    ///
    /// Trailers are part of the response, not an afterthought: gRPC reports the outcome of a call in
    /// `grpc-status` / `grpc-message`, so without them a capture can show that a call happened but
    /// not whether it succeeded. Carried separately from the body because they arrive last, after the
    /// bytes have already streamed to the client.
    ResponseTrailers {
        flow_id: String,
        trailers: Vec<(String, String)>,
    },
    /// A stage of the exchange was reached, declared where it was reached.
    ///
    /// This exists because three typed events (roadmap §4-4) had no producer, and the roadmap is
    /// explicit about how they must get one: *on the proxy's own handshake path*, not by comparing
    /// successive snapshots — the whole point of the typed model is that a consumer stops inferring
    /// what happened. So the proxy says it: this is sent at the moment the request head has been
    /// parsed, and again when the response head has been established.
    Stage {
        flow_id: String,
        stage: ExchangeStage,
        /// When the proxy reached this stage — the fact's own time, not when a consumer read it.
        at: DateTime<Utc>,
    },
    /// One event parsed out of a `text/event-stream` response (roadmap §7.2 / §4-2 "Message").
    ///
    /// An SSE response is a stream of events, so it is reported the way a WebSocket's messages are:
    /// incrementally, as each one becomes complete, rather than making the reader wait for a body
    /// that may never end. The body itself is still recorded separately, so nothing is lost by
    /// reading events from here.
    SseEvent {
        flow_id: String,
        event: crate::sse_stream::SseEvent,
    },
    /// The HTTP/2 stream was reset.
    ///
    /// Distinct from a body that simply stopped: a reset is the peer cancelling the call, and the
    /// bytes already captured are a prefix, not a finished message sequence.
    StreamReset {
        flow_id: String,
        /// The reset reason, as reported by the HTTP/2 stack.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{ResponseTiming, TransportProtocol};

    fn base_flow(layer: Layer) -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".into(),
                client_port: 1,
                server_ip: "127.0.0.1".into(),
                server_port: 80,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer,
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: HashMap::new(),
            matched_rules: vec![],
            connection_id: None,
            parent_flow_id: None,
            protocol_stack: vec![],
        }
    }

    fn response(status: u16, trailers: Vec<(String, String)>) -> HttpResponse {
        HttpResponse {
            status,
            status_text: "OK".into(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            cookies: vec![],
            body: None,
            trailers,
            timing: ResponseTiming {
                time_to_first_byte: None,
                time_to_last_byte: None,
                connect_time_ms: None,
                ssl_time_ms: None,
            },
            sse_events: vec![],
        }
    }

    fn http_flow(response: Option<HttpResponse>, error: Option<&str>) -> Flow {
        base_flow(Layer::Http(HttpLayer {
            request: HttpRequest {
                method: "GET".into(),
                url: Url::parse("http://example.com/x").unwrap(),
                version: "HTTP/1.1".into(),
                headers: vec![],
                cookies: vec![],
                query: vec![],
                body: None,
            },
            response,
            error: error.map(str::to_string),
        }))
    }

    #[test]
    fn upstream_failure_is_an_error_even_though_it_has_no_status() {
        // The regression: the browser called this an error (it had the `Flow`) and the summary did
        // not (it only looked at the status), so the badge vanished on refresh.
        let flow = http_flow(None, Some("Upstream Error: connection refused"));
        assert!(flow_is_error(&flow));
    }

    #[test]
    fn status_and_tag_rules() {
        assert!(flow_is_error(&http_flow(Some(response(500, vec![])), None)));
        assert!(!flow_is_error(&http_flow(
            Some(response(404, vec![])),
            None
        )));
        assert!(!flow_is_error(&http_flow(
            Some(response(200, vec![])),
            None
        )));

        let mut tagged = http_flow(Some(response(200, vec![])), None);
        tagged.tags.push("error".into());
        assert!(flow_is_error(&tagged));
    }

    #[test]
    fn grpc_status_decides_a_call_that_returned_200() {
        let ok = http_flow(
            Some(response(200, vec![("grpc-status".into(), "0".into())])),
            None,
        );
        assert!(!flow_is_error(&ok));
        let failed = http_flow(
            Some(response(200, vec![("grpc-status".into(), "13".into())])),
            None,
        );
        assert!(flow_is_error(&failed));
    }

    #[test]
    fn a_reset_is_an_error_however_it_was_recorded() {
        let mut by_reason = http_flow(Some(response(200, vec![])), None);
        by_reason.close_reason = Some(CloseReason::Reset);
        assert!(flow_is_error(&by_reason));

        let mut by_tag = http_flow(Some(response(200, vec![])), None);
        by_tag.tags.push("stream-reset".into());
        assert!(flow_is_error(&by_tag));
    }

    #[test]
    fn websocket_handshake_failure_is_an_error() {
        let flow = |status: u16| {
            base_flow(Layer::WebSocket(WebSocketLayer {
                handshake_request: HttpRequest {
                    method: "GET".into(),
                    url: Url::parse("ws://example.com/socket").unwrap(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                handshake_response: response(status, vec![]),
                messages: vec![],
                closed: false,
            }))
        };
        assert!(flow_is_error(&flow(500)));
        assert!(!flow_is_error(&flow(101)));
    }

    #[test]
    fn a_flow_with_no_application_layer_is_not_an_error_by_default() {
        // Raw TCP has no status, no response and no trailers: "no answer" is not the same statement
        // as "the answer was a failure", and a capture tool that marks every opaque connection as an
        // error is one whose error filter nobody can use.
        let flow = base_flow(Layer::Tcp(TcpLayer {
            bytes_up: 0,
            bytes_down: 0,
        }));
        assert!(!flow_is_error(&flow));
    }
}
