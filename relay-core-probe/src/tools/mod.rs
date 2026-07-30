use crate::server::ProbeContext;
use rmcp::model::{Content, Tool};
use serde_json::Value;
use std::sync::Arc;

pub mod intercept;
pub mod query;
pub mod rules;
pub mod script;

// Re-export all public tool functions for external testing
pub use intercept::{get_pending_intercepts, resume_flow, set_intercept};
pub use query::{export_har, get_flow, get_metrics, replay_flow, search_flows};
pub use rules::{delete_rule, get_policy, mock_url, patch_policy, set_rule, update_policy};
pub use script::set_script;

#[derive(Debug)]
pub enum ToolError {
    NotFound(String),
    InvalidArgument(String),
    Internal(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::NotFound(msg) => write!(f, "NotFound: {msg}"),
            ToolError::InvalidArgument(msg) => write!(f, "InvalidArgument: {msg}"),
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
}

/// 返回所有工具的 schema 声明（用于 list_tools 响应）
pub fn tool_list() -> Vec<Tool> {
    vec![
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
    ]
}

/// 按工具名分发调用
pub async fn dispatch(
    ctx: &Arc<ProbeContext>,
    name: &str,
    args: Value,
) -> Result<Vec<Content>, ToolError> {
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

/// 构造工具 schema 的辅助函数
pub(crate) fn make_tool(name: &str, description: &str, input_schema: Value) -> Tool {
    let schema = Arc::new(input_schema.as_object().cloned().unwrap_or_default());
    Tool::new(name.to_string(), description.to_string(), schema)
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

pub(crate) fn ok_json(value: &impl serde::Serialize) -> Result<Vec<Content>, ToolError> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(vec![Content::text(text)])
}

pub(crate) fn ok_text(text: impl Into<String>) -> Result<Vec<Content>, ToolError> {
    Ok(vec![Content::text(text.into())])
}
