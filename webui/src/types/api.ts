// Types mirroring relay-core-api Rust types

export interface FlowSummary {
  id: string;
  method: string;
  url: string;
  host: string;
  path: string;
  status: number | null;
  duration_ms: number | null;
  tags: string[];
  start_time_ms: number;
  has_error: boolean;
  is_websocket: boolean;
  /** gRPC status from response trailers. `0` is success. */
  grpc_status?: number | null;
  /** Decoded `grpc-message` trailer. */
  grpc_message?: string | null;
}

export interface NetworkInfo {
  client_ip: string;
  client_port: number;
  server_ip: string;
  server_port: number;
  /** Target of the request as `host:port`; absent when unknown. The peer address above is only set
   *  when this engine made the connection, so this is what a forward-proxy flow can report. */
  server_host?: string | null;
  protocol: string;
  tls: boolean;
  tls_version: string | null;
  sni: string | null;
}

export interface BodyData {
  encoding: string;
  content: string;
  size: number;
  /** Present when the body is gRPC, describing its message framing. */
  grpc?: GrpcBody | null;
}

/** One length-prefixed message inside a gRPC body. */
export interface GrpcMessage {
  index: number;
  length: number;
  compressed: boolean;
  /** The payload as text, when it is text (absent for binary or compressed payloads). */
  text?: string | null;
  /** Protobuf view added on a detail read. Absent on the stored capture. */
  decoded?: GrpcDecoded | null;
}

export interface GrpcDecoded {
  /** `schema` when a `.proto` matched the method, `wire` otherwise. */
  source: 'schema' | 'wire';
  message_type?: string | null;
  value?: unknown;
  error?: string | null;
}

export interface GrpcBody {
  messages: GrpcMessage[];
  /** Bytes that were not part of a complete message — a capture cut short. */
  unparsed_bytes: number;
  /** Why a schema was not applied, when the proto directory failed or no rpc matched. */
  schema_note?: string | null;
}

export interface Cookie {
  name: string;
  value: string;
  path?: string;
  domain?: string;
  expires?: string;
  http_only?: boolean;
  secure?: boolean;
}

export interface ResponseTiming {
  time_to_first_byte: number | null;
  time_to_last_byte: number | null;
  connect_time_ms?: number | null;
  ssl_time_ms?: number | null;
}

export interface HttpRequest {
  method: string;
  url: string;
  version: string;
  headers: [string, string][];
  cookies: Cookie[];
  query: [string, string][];
  body: BodyData | null;
}

export interface HttpResponse {
  status: number;
  status_text: string;
  version: string;
  headers: [string, string][];
  cookies: Cookie[];
  body: BodyData | null;
  timing: ResponseTiming;
  trailers?: [string, string][];
  /**
   * Events parsed out of this response when it was a `text/event-stream`.
   *
   * Recorded so a capture opened after the stream ended still reads as events; the live path pushes
   * each one as it is parsed, because a response that may never end cannot be waited for. Capped, and
   * the flow is tagged `sse-events-capped` when the cap was reached.
   */
  sse_events?: SseStreamEvent[];
}

export interface HttpLayer {
  type: 'Http';
  data: {
    request: HttpRequest;
    response: HttpResponse | null;
    error: string | null;
  };
}

export interface WebSocketMessage {
  id: string;
  timestamp: string;
  direction: 'ClientToServer' | 'ServerToClient';
  content: BodyData;
  opcode: string;
}

export interface WebSocketLayer {
  type: 'WebSocket';
  data: {
    handshake_request: HttpRequest;
    handshake_response: HttpResponse;
    messages: WebSocketMessage[];
    /** Whether the connection has closed. The Rust field is `closed`; this said `open`, which is the
     *  one thing the wire never carries — a reader of `.open` would get `undefined` and read it as
     *  `false`, i.e. "still open", for every socket that had already closed. */
    closed: boolean;
  };
}

export type Layer = HttpLayer | WebSocketLayer | { type: string; data?: unknown };

export interface ResilienceTrace {
  retry_count?: number;
  circuit_open?: boolean;
  budget_exceeded?: boolean;
  upstream_errors?: string[];
  timeout_type?: string | null;
}

export interface Flow {
  id: string;
  start_time: string;
  end_time: string | null;
  network: NetworkInfo;
  layer: Layer;
  tags: string[];
  /** Omitted from the wire when the flow met no resilience machinery. */
  resilience_trace?: ResilienceTrace | null;
  /**
   * Omitted from the wire when empty: the API skips empty collections (`skip_serializing_if`), so an
   * ordinary capture carries neither key. They are declared optional because that is what arrives —
   * typing them as always present is what let the Trace tab read `matched_rules.length` off a flow
   * that had no such field.
   */
  rule_variables?: Record<string, string>;
  matched_rules?: string[];
  /**
   * How the exchange ended, in the unified vocabulary of `relay-core-api/src/event.rs`.
   *
   * Internally tagged on the wire — `{"reason":"reset"}`, `{"reason":"timeout","kind":"idle"}` — and
   * omitted when the ending was not recorded, which is different from a confirmed normal completion.
   */
  close_reason?: CloseReason | null;
}

export type CloseReason =
  | { reason: 'client_closed' }
  | { reason: 'upstream_closed' }
  | { reason: 'timeout'; kind: string }
  | { reason: 'reset' }
  | { reason: 'policy_drop'; detail: string }
  | { reason: 'parser_error'; detail: string }
  | { reason: 'tls_error'; detail: string }
  | { reason: 'completed' };

/**
 * One page of stored flow summaries, as `GET /api/v1/flows` answers it.
 *
 * There is no `total`: the route returns `FlowSearchResponse { items, returned, limit, offset }` and
 * never counted the matches. The `total` this interface used to require was `undefined` at runtime,
 * so `page.total > page.items.length` would have been `false` for a truncated page — a lie that only
 * shows up as a missing control, never as an error.
 */
export interface FlowPage {
  items: FlowSummary[];
  returned: number;
  limit: number;
  offset: number;
}

// Rules
export type RuleStage =
  | 'Connect'
  | 'RequestHeaders'
  | 'RequestBody'
  | 'ResponseHeaders'
  | 'ResponseBody'
  | 'WebSocketMessage';

export type RuleTermination = 'Continue' | 'Stop';

export interface StringMatcher {
  mode: 'Exact' | 'Contains' | 'Prefix' | 'Suffix' | 'Regex' | 'Glob';
  value: string;
}

export interface Filter {
  type: string;
  config?: unknown;
}

export interface RuleAction {
  type: string;
  config?: unknown;
}

export interface Rule {
  id: string;
  name: string;
  active: boolean;
  stage: RuleStage;
  priority: number;
  termination: RuleTermination;
  filter: Filter;
  actions: RuleAction[];
}

// Intercept
export interface FlowModification {
  method?: string;
  url?: string;
  request_headers?: Record<string, string>;
  request_body?: string;
  status_code?: number;
  response_headers?: Record<string, string>;
  response_body?: string;
  message_content?: string;
}

export interface InterceptItem {
  key: string;
  flow_id: string;
  phase: string;
  url: string;
  method: string;
  created_at_ms: number;
}

export interface CoreInterceptSnapshot {
  pending_count: number;
  ws_pending_count: number;
  items?: InterceptItem[];
}

// Metrics
export interface CoreMetrics {
  flows_total: number;
  flows_in_memory: number;
  flows_dropped: number;
  intercepts_pending: number;
  ws_pending_messages: number;
  oldest_intercept_age_ms: number | null;
  oldest_ws_message_age_ms: number | null;
  rule_exec_errors: number;
  audit_events_total: number;
  audit_events_failed: number;
  flow_events_lagged_total: number;
  audit_events_lagged_total: number;
  proxy_body_degraded_total: number;
  proxy_http_request_total: number;
  proxy_sandbox_reject_total: number;
  proxy_invalid_method_total: number;
  proxy_invalid_status_total: number;
  proxy_retry_total: number;
  proxy_stream_mode_tap_total: number;
  proxy_stream_mode_degrade_total: number;
}

// Status
export type LifecyclePhase = 'Created' | 'Starting' | 'Running' | 'Stopping' | 'Stopped' | 'Failed';

export interface CoreStatusSnapshot {
  phase: LifecyclePhase;
  running: boolean;
  port: number | null;
  uptime: number | null;
  last_error: string | null;
}

// SSE Events
export interface FlowUpdateFull {
  type: 'Full';
  data: Flow;
}

/**
 * One event parsed out of a *proxied* `text/event-stream`, as `event: sse-event` carries it.
 *
 * `event` is the SSE field the app set (`undefined` means the spec's unnamed `message` event), and
 * `data` is the payload with the trailing newline stripped. This mirrors the engine's
 * `SseEventFrame`; the fields are optional on the wire because the engine skips them when unset.
 */
export interface SseStreamEvent {
  /** The `event:` field, absent for an unnamed event. */
  event?: string;
  /** The collected `data:` payload. */
  data: string;
  /** The last-event-id in effect when the event was dispatched. */
  id?: string;
  /** The reconnection time in effect, in milliseconds. */
  retry?: number;
}

export interface FlowUpdateSseEvent {
  type: 'SseEvent';
  data: {
    flow_id: string;
    event: SseStreamEvent;
  };
}

export interface FlowUpdateWsMsg {
  type: 'WebSocketMessage';
  data: {
    flow_id: string;
    message: WebSocketMessage;
  };
}

export interface FlowUpdateHttpBody {
  type: 'HttpBody';
  data: {
    flow_id: string;
    direction: 'ClientToServer' | 'ServerToClient';
    body: BodyData;
  };
}

export interface FlowUpdateBudgetExceeded {
  type: 'BodyBudgetExceeded';
  data: {
    flow_id: string;
    direction: 'ClientToServer' | 'ServerToClient';
  };
}

export type FlowUpdate = FlowUpdateFull | FlowUpdateWsMsg | FlowUpdateHttpBody | FlowUpdateBudgetExceeded;

// Policy
export interface RedactionPolicy {
  enabled: boolean;
  sensitive_header_names: string[];
  sensitive_query_keys: string[];
  redact_bodies: boolean;
}

export interface UpstreamProxyConfig {
  proxy_url: string;
  auth?: { username: string; password?: string };
  /** Omitted when empty, like every other `Vec` in the API (`skip_serializing_if`). */
  bypass_hosts?: string[];
  fail_open: boolean;
}

export interface ProxyPolicy {
  strict_http_semantics: boolean;
  transparent_enabled: boolean;
  quic_mode: string;
  redaction: RedactionPolicy;
  /** Omitted — not `null` — when no upstream proxy is configured, so read it with `?.`. */
  upstream?: UpstreamProxyConfig;
  max_body_size: number;
  rule_body_inspect_budget: number;
  request_timeout_ms: number;
}

export interface ProxyPolicyPatch {
  redaction?: Partial<RedactionPolicy>;
  upstream?: UpstreamProxyConfig;
}

// Audit
export interface AuditEvent {
  id: string;
  timestamp_ms: number;
  actor: string;
  kind: string;
  target: string;
  outcome: 'Success' | 'Failed';
  details: unknown;
}

export interface CoreAuditSnapshot {
  events: AuditEvent[];
}
