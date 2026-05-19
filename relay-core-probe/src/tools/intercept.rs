use super::ToolError;
use super::{make_tool, ok_json, ok_text, require_str};
use crate::server::ProbeContext;
use relay_core_api::modification::FlowModification;
use relay_core_api::rule::RuleTermination;
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::rule::InterceptRuleConfig;
use rmcp::model::{Content, Tool};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub fn set_intercept_schema() -> Tool {
    make_tool(
        "set_intercept",
        "Set up a one-shot intercept breakpoint. The next request matching the URL pattern \
         will be paused. Use get_pending_intercepts to see it, resume_flow to release it.",
        json!({
            "type": "object",
            "required": ["url_pattern"],
            "properties": {
                "url_pattern": {
                    "type": "string",
                    "description": "URL substring or regex pattern to match (e.g. '/api/login', 'example.com')"
                },
                "phase": {
                    "type": "string",
                    "enum": ["request", "response", "both"],
                    "description": "Which phase to intercept (default: request)"
                }
            }
        }),
    )
}

pub fn get_pending_intercepts_schema() -> Tool {
    make_tool(
        "get_pending_intercepts",
        "List all flows currently paused waiting for an intercept decision.",
        json!({ "type": "object", "properties": {} }),
    )
}

pub fn resume_flow_schema() -> Tool {
    make_tool(
        "resume_flow",
        "Resume a paused (intercepted) flow. Optionally apply modifications before releasing.",
        json!({
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Intercept key from get_pending_intercepts (format: '<flow_id>:<phase>')"
                },
                "action": {
                    "type": "string",
                    "enum": ["continue", "drop"],
                    "description": "Whether to forward or drop the request (default: continue)"
                },
                "method":           { "type": "string" },
                "url":              { "type": "string" },
                "request_headers":  { "type": "object" },
                "request_body":     { "type": "string" },
                "status_code":      { "type": "integer" },
                "response_headers": { "type": "object" },
                "response_body":    { "type": "string" },
                "message_content":  { "type": "string" }
            }
        }),
    )
}

pub async fn set_intercept(
    ctx: &Arc<ProbeContext>,
    args: Value,
) -> Result<Vec<Content>, ToolError> {
    let url_pattern = require_str(&args, "url_pattern")?.to_string();
    let phase = args
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("request");

    let rule_id = Uuid::new_v4().to_string();
    ctx.rules
        .create_intercept_rule_from(
            AuditActor::Probe,
            rule_id.clone(),
            json!({
                "tool": "set_intercept",
                "url_pattern": url_pattern,
                "phase": phase
            }),
            InterceptRuleConfig {
                rule_id: rule_id.clone(),
                active: true,
                url_pattern: url_pattern.clone(),
                method: None,
                phase: phase.to_string(),
                name: format!("probe-intercept:{}", url_pattern),
                priority: 100,
                termination: RuleTermination::Stop,
            },
        )
        .await?;
    ok_text(format!(
        "Intercept breakpoint set (rule_id: {}). Waiting for matching request…",
        rule_id
    ))
}

pub async fn get_pending_intercepts(ctx: &Arc<ProbeContext>) -> Result<Vec<Content>, ToolError> {
    ok_json(&ctx.intercepts.intercept_snapshot().await)
}

pub async fn resume_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, ToolError> {
    let key = require_str(&args, "key")?.to_string();
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("continue")
        .to_string();

    let mods = FlowModification::from_json_value(&args).into_option();
    ctx.intercepts
        .resolve_intercept_with_modifications_from(AuditActor::Probe, key.clone(), &action, mods)
        .await?;
    ok_text(format!("Flow {} resumed with action '{}'", key, action))
}

#[cfg(test)]
mod tests {
    use super::get_pending_intercepts;
    use crate::server::ProbeContext;
    use relay_core_runtime::CoreState;
    use std::sync::Arc;

    #[tokio::test]
    async fn pending_intercepts_tool_returns_shared_snapshot_shape() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(ProbeContext::new(state));
        let contents = get_pending_intercepts(&ctx)
            .await
            .expect("tool should succeed");

        let serialized = serde_json::to_value(&contents[0]).expect("content should serialize");
        let text = serialized["text"]
            .as_str()
            .expect("tool content should contain text");
        let json: serde_json::Value =
            serde_json::from_str(text).expect("tool output should be valid json");
        assert_eq!(json["pending_count"], 0);
        assert_eq!(json["ws_pending_count"], 0);
    }
}
