use crate::server::ProbeContext;
use rmcp::model::{Content, Tool};
use serde_json::Value;
use std::sync::Arc;

mod intercept;
mod query;
mod rules;
mod script;

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
) -> Result<Vec<Content>, String> {
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
        other => Err(format!("Unknown tool: {}", other)),
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
pub(crate) fn require_str(args: &Value, key: &str) -> Result<String, String> {
    get_str(args, key)
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Missing required parameter: {}", key))
}

/// 把结果序列化为 JSON 文本 Content
pub(crate) fn ok_json(value: &impl serde::Serialize) -> Result<Vec<Content>, String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    Ok(vec![Content::text(text)])
}

pub(crate) fn ok_text(text: impl Into<String>) -> Result<Vec<Content>, String> {
    Ok(vec![Content::text(text.into())])
}
