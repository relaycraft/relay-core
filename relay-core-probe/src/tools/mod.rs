use crate::server::ProbeContext;
use rmcp::model::{Tool, ToolAnnotations};
use serde_json::Value;
use std::sync::Arc;

pub mod intercept;
pub mod lifecycle;
pub mod query;
pub mod rules;
pub mod script;

// Re-export all public tool functions for external testing
pub use intercept::{get_pending_intercepts, resume_flow, set_intercept};
pub use lifecycle::{PROXY_CONTROL_HINT, lifecycle_tool_schemas};
pub use query::{export_har, get_flow, get_metrics, replay_flow, search_flows};
pub use rules::{delete_rule, get_policy, mock_url, patch_policy, set_rule, update_policy};
pub use script::set_script;

#[derive(Debug)]
pub enum ToolError {
    NotFound(String),
    InvalidArgument(String),
    /// The call is well-formed but the engine is not in a state where it can be answered — for
    /// example, no proxy is running, so reporting "no traffic" would be a lie.
    ///
    /// Carries a stable code because these are the errors an agent is expected to act on: prose
    /// would force it to guess whether to start a proxy, fix an argument, or give up.
    Unavailable {
        code: &'static str,
        message: String,
    },
    Internal(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::NotFound(msg) => write!(f, "NotFound: {msg}"),
            ToolError::InvalidArgument(msg) => write!(f, "InvalidArgument: {msg}"),
            ToolError::Unavailable { code, message } => write!(f, "{code}: {message}"),
            ToolError::Internal(msg) => write!(f, "Internal: {msg}"),
        }
    }
}

impl From<String> for ToolError {
    fn from(s: String) -> Self {
        ToolError::Internal(s)
    }
}

impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        ToolError::Internal(s.to_string())
    }
}

impl ToolError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        ToolError::NotFound(msg.into())
    }
    pub fn invalid_arg(msg: impl Into<String>) -> Self {
        ToolError::InvalidArgument(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        ToolError::Internal(msg.into())
    }
    pub fn unavailable(code: &'static str, msg: impl Into<String>) -> Self {
        ToolError::Unavailable {
            code,
            message: msg.into(),
        }
    }

    /// Machine-readable code, or `None` for errors the MCP error object already classifies.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { code, .. } => Some(code),
            _ => None,
        }
    }

    /// The error an agent should see when the proxy it asks about is not running.
    pub fn proxy_not_running(context: &str) -> Self {
        Self::unavailable(
            "proxy_not_running",
            format!(
                "{context} No proxy is running, so nothing is being captured and no history \
                 exists. Call proxy_start (or run `relay start`), then retry."
            ),
        )
    }
}

/// 返回所有工具的 schema 声明（用于 list_tools 响应）
pub fn tool_list() -> Vec<Tool> {
    let rest = vec![
        query::search_flows_schema(),
        query::get_flow_schema(),
        query::get_metrics_schema(),
        query::replay_flow_schema(),
        query::export_har_schema(),
        intercept::set_intercept_schema(),
        intercept::get_pending_intercepts_schema(),
        intercept::resume_flow_schema(),
        rules::set_rule_schema(),
        rules::delete_rule_schema(),
        rules::mock_url_schema(),
        rules::get_policy_schema(),
        rules::update_policy_schema(),
        rules::patch_policy_schema(),
        script::set_script_schema(),
    ];

    // Lifecycle tools come first: an agent that cannot see whether a proxy is running reads every
    // empty result as "no traffic".
    let mut tools = lifecycle::lifecycle_tool_schemas();
    tools.extend(rest);
    tools
}

/// 按工具名分发调用
pub async fn dispatch(
    ctx: &Arc<ProbeContext>,
    name: &str,
    args: Value,
) -> Result<ToolOutcome, ToolError> {
    if lifecycle::handles(name) {
        return lifecycle::dispatch(ctx, name, args).await;
    }

    match name {
        "search_flows" => query::search_flows(ctx, args).await,
        "get_flow" => query::get_flow(ctx, args).await,
        "get_metrics" => query::get_metrics(ctx).await,
        "replay_flow" => query::replay_flow(ctx, args).await,
        "export_har" => query::export_har(ctx, args).await,
        "set_intercept" => intercept::set_intercept(ctx, args).await,
        "get_pending_intercepts" => intercept::get_pending_intercepts(ctx).await,
        "resume_flow" => intercept::resume_flow(ctx, args).await,
        "set_rule" => rules::set_rule(ctx, args).await,
        "delete_rule" => rules::delete_rule(ctx, args).await,
        "mock_url" => rules::mock_url(ctx, args).await,
        "get_policy" => rules::get_policy(ctx).await,
        "update_policy" => rules::update_policy(ctx, args).await,
        "patch_policy" => rules::patch_policy(ctx, args).await,
        "set_script" => script::set_script(ctx, args).await,
        other => Err(ToolError::not_found(format!("Unknown tool: {other}"))),
    }
}

/// What a tool call returns.
///
/// Both halves are always present, and `text` is always the serialization of `structured`: the MCP
/// spec asks a tool that returns structured content to keep returning the JSON as text for clients
/// that predate `structuredContent`. Having exactly one shape also means an old client and a new one
/// can never disagree about what a call returned.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub structured: Value,
    pub text: String,
}

impl ToolOutcome {
    /// A structured result, with the same JSON serialized as the text fallback.
    pub fn json(structured: Value) -> Self {
        let text =
            serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());
        Self { structured, text }
    }

    /// A write result: a sentence for a human, typed fields for the agent.
    ///
    /// Every acknowledgement carries `ok`, so an agent can branch on success without matching
    /// message strings — and so the shared acknowledgement output schema is true by construction.
    pub fn ack(message: impl Into<String>, fields: Value) -> Self {
        let mut object = match fields {
            Value::Object(object) => object,
            _ => serde_json::Map::new(),
        };
        object.insert("ok".to_string(), Value::Bool(true));
        object.insert("message".to_string(), Value::String(message.into()));
        Self::json(Value::Object(object))
    }
}

/// Declarative description of a tool: its schema and how a client should reason about calling it.
///
/// Annotations are part of the contract, not decoration: an agent that cannot tell a read-only
/// query from one that stops the proxy has to guess whether a tool is safe to call speculatively.
pub(crate) struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input: Value,
    pub output: Option<Value>,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    /// Whether the tool reaches outside the local engine (network, filesystem). Defaults to
    /// false, which tells a client the tool's domain is the captured traffic it can already see.
    pub open_world: bool,
}

impl ToolSpec {
    /// A tool that only observes. Read-only tools are idempotent and never destructive.
    pub(crate) fn read_only(name: &'static str, description: &'static str, input: Value) -> Self {
        Self {
            name,
            description,
            input,
            output: None,
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        }
    }

    /// A tool that changes state. `destructive` means it can remove or interrupt something;
    /// `idempotent` means calling it twice with the same arguments is the same as calling it once.
    pub(crate) fn write(
        name: &'static str,
        description: &'static str,
        input: Value,
        destructive: bool,
        idempotent: bool,
    ) -> Self {
        Self {
            name,
            description,
            input,
            output: None,
            read_only: false,
            destructive,
            idempotent,
            open_world: false,
        }
    }

    /// Declare the shape of the structured result.
    ///
    /// Only set where the shape is stable, because a declared schema that lies is worse than none:
    /// clients validate against it.
    /// Mark a tool that reaches outside the local engine.
    pub(crate) fn open_world(mut self) -> Self {
        self.open_world = true;
        self
    }

    pub(crate) fn with_output(mut self, output: Value) -> Self {
        self.output = Some(output);
        self
    }

    pub(crate) fn build(self) -> Tool {
        let schema = Arc::new(self.input.as_object().cloned().unwrap_or_default());
        let mut tool = Tool::new(self.name.to_string(), self.description.to_string(), schema);
        tool.output_schema = self
            .output
            .and_then(|output| output.as_object().cloned())
            .map(Arc::new);
        tool.annotations = Some(ToolAnnotations::from_raw(
            None,
            Some(self.read_only),
            Some(self.destructive),
            Some(self.idempotent),
            Some(self.open_world),
        ));
        tool
    }
}

/// Build a tool from its spec.
pub(crate) fn tool(spec: ToolSpec) -> Tool {
    spec.build()
}

/// The structured result every write tool returns: `ok` plus a human-readable `message`.
pub(crate) fn ack_output_schema(extra_properties: Value) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("ok".to_string(), serde_json::json!({ "type": "boolean" }));
    properties.insert(
        "message".to_string(),
        serde_json::json!({ "type": "string" }),
    );
    if let Value::Object(extra) = extra_properties {
        properties.extend(extra);
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": ["ok", "message"],
    })
}

/// 从 args 中取 string 字段
pub(crate) fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// 从 args 中取 string 字段，缺失时返回错误
pub(crate) fn require_str(args: &Value, key: &str) -> Result<String, ToolError> {
    get_str(args, key)
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::invalid_arg(format!("Missing required parameter: {key}")))
}

pub(crate) fn ok_json(value: &impl serde::Serialize) -> Result<ToolOutcome, ToolError> {
    let structured = serde_json::to_value(value).map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(ToolOutcome::json(structured))
}

/// A write result: a sentence plus the typed fields behind it.
pub(crate) fn ok_ack(message: impl Into<String>, fields: Value) -> Result<ToolOutcome, ToolError> {
    Ok(ToolOutcome::ack(message, fields))
}

/// A rule id derived from the call that asked for the rule.
///
/// Idempotency needs identity: an agent that retries `set_intercept` or `mock_url` with the same
/// arguments is asking for the same rule, and a fresh UUID per call turned every retry into an extra
/// rule. The hash is FNV-1a rather than a cryptographic digest because this is identity, not
/// security, and unlike `DefaultHasher` it is reproducible across builds and Rust versions — a rule
/// id that changed when the toolchain did would silently stop matching the rule it was stored under.
pub(crate) fn stable_rule_id(prefix: &str, parts: &[&str]) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // A separator so ["ab", "c"] and ["a", "bc"] cannot collide.
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    format!("{prefix}-{hash:016x}")
}

#[cfg(test)]
mod stable_rule_id_tests {
    use super::stable_rule_id;

    /// The same call must produce the same id, or a retried tool call accumulates duplicates.
    #[test]
    fn the_same_call_produces_the_same_id() {
        let first = stable_rule_id("probe-intercept", &["example.com/api", "request"]);
        let second = stable_rule_id("probe-intercept", &["example.com/api", "request"]);

        assert_eq!(first, second);
        assert!(first.starts_with("probe-intercept-"));
    }

    /// Different calls must not collide, including when the boundary between parts moves.
    #[test]
    fn different_calls_produce_different_ids() {
        let ids = [
            stable_rule_id("probe-intercept", &["example.com/api", "request"]),
            stable_rule_id("probe-intercept", &["example.com/api", "response"]),
            stable_rule_id("probe-intercept", &["example.com/other", "request"]),
            stable_rule_id("probe-mock", &["example.com/api", "request"]),
            stable_rule_id("probe-intercept", &["ab", "c"]),
            stable_rule_id("probe-intercept", &["a", "bc"]),
        ];

        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "ids must distinguish these calls: {ids:?}"
        );
    }
}
