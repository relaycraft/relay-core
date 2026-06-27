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
}

export interface NetworkInfo {
  client_ip: string;
  client_port: number;
  server_ip: string;
  server_port: number;
  protocol: string;
  tls: boolean;
  tls_version: string | null;
  sni: string | null;
}

export interface BodyData {
  encoding: string;
  content: string;
  size: number;
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
    open: boolean;
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
  resilience_trace: ResilienceTrace | null;
  rule_variables: Record<string, string>;
  matched_rules: string[];
}

export interface FlowPage {
  items: FlowSummary[];
  total: number;
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
  bypass_hosts: string[];
  fail_open: boolean;
}

export interface ProxyPolicy {
  strict_http_semantics: boolean;
  transparent_enabled: boolean;
  quic_mode: string;
  redaction: RedactionPolicy;
  upstream: UpstreamProxyConfig | null;
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
