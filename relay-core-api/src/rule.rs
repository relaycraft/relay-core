use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A self-contained unit of logic with explicit stage, priority, and flow control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Whether the rule is enabled
    pub active: bool,

    /// [P0] Execution Stage: Determines *when* this rule is evaluated.
    pub stage: RuleStage,

    /// Execution priority (Higher value = Earlier execution)
    /// Default: 0
    #[serde(default)]
    pub priority: i32,

    /// [P0] Flow Control: Replaces `stop_on_match`.
    /// Determines if subsequent rules in the same stage should be skipped.
    pub termination: RuleTermination,

    /// The condition to match traffic
    pub filter: Filter,

    /// The actions to execute when matched
    pub actions: Vec<Action>,

    /// [P1] Performance Constraints
    pub constraints: Option<RuleConstraints>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleGroup {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleStage {
    /// L3/L4: Connection establishment (IP/Port filtering)
    Connect,
    /// L7: HTTP Request Headers available
    RequestHeaders,
    /// L7: HTTP Request Body available (Streaming or Buffered)
    RequestBody,
    /// L7: HTTP Response Headers available
    ResponseHeaders,
    /// L7: HTTP Response Body available (Streaming or Buffered)
    ResponseBody,
    /// WebSocket Message frame
    WebSocketMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleTermination {
    /// Continue to the next rule in this stage (Default)
    Continue,
    /// Stop processing subsequent rules in this stage
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConstraints {
    /// Max execution time in ms (soft limit)
    pub timeout_ms: Option<u64>,
}

// --- Filters ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum Filter {
    /// Matches everything (useful for global rules)
    All,

    // --- L3/L4 Network Filters (Valid in Connect stage) ---
    /// Matches by Source IP (CIDR support)
    SrcIp(String), // e.g., "192.168.1.0/24"
    /// Matches by Destination Port
    DstPort(u16),
    /// Matches by Protocol (TCP/UDP)
    Protocol(String),
    /// Matches if connection is in transparent proxy mode
    TransparentMode(bool),

    // --- L7 HTTP Filters (Valid in Request/Response stages) ---
    /// Matches by Full URL
    Url(StringMatcher),
    /// Matches by Host/Domain
    Host(StringMatcher),
    /// Matches by URL Path
    Path(StringMatcher),
    /// Matches by HTTP Method (GET, POST, etc.)
    Method(StringMatcher),
    /// Matches by Request Header presence or value
    RequestHeader {
        name: String,
        value: Option<StringMatcher>,
    },
    /// Matches by Response Header presence or value
    ResponseHeader {
        name: String,
        value: Option<StringMatcher>,
    },
    /// Matches by Response Status Code
    StatusCode(u16),
    /// Matches by Response Body Content (Valid only in ResponseBody stage)
    ResponseBody(StringMatcher),
    /// Matches by WebSocket Message Content (Valid only in WebSocketMessage stage)
    WebSocketMessage(StringMatcher),

    // --- Logical Compositors ---
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value")]
pub enum StringMatcher {
    Exact(String),
    Contains(String),
    Prefix(String),
    Suffix(String),
    Regex(String),
    Glob(String),
}

// --- Actions ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum Action {
    // === Tier 1: Universal Control Actions ===
    /// Drop the connection immediately
    Drop,
    /// Abort the connection (RST)
    Abort,
    /// Delay execution (Latency Simulation)
    Delay {
        ms: u64,
    },
    /// Throttle bandwidth
    Throttle {
        kbps: u64,
    },
    /// Tag for subsequent processing/stats
    Tag {
        key: String,
        value: String,
    },
    /// Pause the flow for manual inspection/intervention (e.g., in GUI)
    Inspect,
    /// Set a variable for use in subsequent actions (Fluxzy-style)
    SetVariable {
        name: String,
        value: String,
    },
    /// Rate limit traffic based on a key
    RateLimit {
        /// The key to limit by (e.g., "ip", "host", "path", or a template like "{{ip}}:{{path}}")
        key: String,
        /// Maximum number of requests allowed in the window
        limit: u32,
        /// Time window in milliseconds
        window_ms: u64,
    },

    // === Tier 2: L3/L4 Network Actions (Future) ===
    RedirectIp {
        target: String,
    },
    SetTtl {
        ttl: u8,
    },
    ForwardPort {
        target_host: String,
        target_port: u16,
    },

    // === Tier 3: L7 HTTP Actions ===
    // --- Terminal Actions ---
    MockResponse {
        status: u16,
        headers: HashMap<String, String>,
        body: Option<BodySource>,
    },
    MapLocal {
        path: String,
        content_type: Option<String>,
    },
    MapRemote {
        url: String,
        preserve_host: bool,
    },
    Redirect {
        location: String,
        status: u16,
    },

    // --- Modification Actions (Headers) ---
    /// Add a header (Appends if exists, useful for multi-value headers like Set-Cookie)
    AddRequestHeader {
        name: String,
        value: String,
    },
    /// Update an existing header (Supports {{previous}} variable)
    /// If add_if_missing is true, creates it if not found.
    UpdateRequestHeader {
        name: String,
        value: String,
        add_if_missing: bool,
    },
    /// Delete a header
    DeleteRequestHeader {
        name: String,
    },

    AddResponseHeader {
        name: String,
        value: String,
    },
    UpdateResponseHeader {
        name: String,
        value: String,
        add_if_missing: bool,
    },
    DeleteResponseHeader {
        name: String,
    },

    // --- Modification Actions (Request/Response) ---
    SetRequestMethod {
        method: String,
    },
    SetRequestUrl {
        url: String,
    },
    SetRequestBody {
        body: BodySource,
    },
    SetResponseStatus {
        status: u16,
    },
    SetResponseBody {
        body: BodySource,
    },

    // --- Transformation Actions ---
    TransformRequestBody {
        transform: BodyTransform,
    },
    TransformResponseBody {
        transform: BodyTransform,
    },

    // === Tier 3: WebSocket Actions ===
    MockWebSocketMessage {
        direction: WebSocketDirection,
        message: String,
    },
    DropWebSocketMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum BodySource {
    Text(String),
    File(String),
    Base64(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum BodyTransform {
    RegexReplace {
        pattern: String,
        replacement: String,
    },
    JsonPathSet {
        path: String,
        value: String,
    },
    JsonPathDelete {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSocketDirection {
    Incoming,
    Outgoing,
}

// --- Tracing ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleTrace {
    pub flow_id: String,
    /// Summary for list view icons
    pub summary: RuleTraceSummary,
    /// Detailed execution log (Lazy loaded)
    pub events: Vec<RuleExecutionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleTraceSummary {
    NoMatch,
    /// Modified by one or more rules
    Modified {
        rule_ids: Vec<String>,
    },
    /// Terminated by a rule (Drop/Mock)
    Terminated {
        rule_id: String,
        reason: TerminalReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalReason {
    Drop,
    Abort,
    Mock,
    Redirect,
    Inspect,
    RateLimited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleExecutionEvent {
    pub rule_id: String,
    pub stage: RuleStage,
    pub matched: bool,
    pub duration_us: u64,
    pub outcome: RuleOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleOutcome {
    Skipped,
    MatchedAndExecuted,
    MatchedAndTerminated,
    Failed(String),
}

// ── S10a: Rule↔Script bridge registry ──────────────────────

/// Registry of script helper functions that can be called from rule templates
/// via {{script:fnName}}. Populated at script load time from globalThis.scriptHelpers.
/// This is a data-only struct in 1.0; runtime invocation deferred to 1.x.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleScriptRegistry {
    pub helpers: std::collections::HashMap<String, ScriptHelperEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptHelperEntry {
    pub name: String,
    pub description: Option<String>,
    pub arity: usize,
}
