use super::ToolError;
use super::{ToolOutcome, ToolSpec, ack_output_schema, ok_ack, ok_json, require_str, tool};
use crate::server::ProbeContext;
use relay_core_api::modification::FlowModification;
use relay_core_api::rule::RuleTermination;
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::rule::InterceptRuleConfig;
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn set_intercept_schema() -> Tool {
    tool(
        ToolSpec::write(
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
        false,
        true,
    )
        .with_output(ack_output_schema(json!({
            "rule_id": { "type": "string" },
            "url_pattern": { "type": "string" },
            "phase": { "type": "string" },
        }))),
    )
}

pub fn get_pending_intercepts_schema() -> Tool {
    tool(
        ToolSpec::read_only(
            "get_pending_intercepts",
            "List all flows currently paused waiting for an intercept decision.",
            json!({ "type": "object", "properties": {} }),
        )
        .with_output(json!({
            "type": "object",
            "properties": {
                "pending_count": { "type": "integer" },
                "ws_pending_count": { "type": "integer" },
                "items": { "type": "array", "items": { "type": "object" } }
            },
            "required": ["pending_count", "ws_pending_count"],
        })),
    )
}

pub fn resume_flow_schema() -> Tool {
    tool(
        ToolSpec::write(
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
                "request_headers": {
                    "type": "object",
                    "description": "Replaces the entire request header map. Omit the field to leave headers unchanged. To add or overwrite headers without deleting the rest, use request_header_upserts."
                },
                "request_header_upserts": {
                    "type": "object",
                    "description": "Adds or overwrites request headers by name (case-insensitive, first match). Other headers are kept. Does not delete headers."
                },
                "request_body":     { "type": "string" },
                "status_code":      { "type": "integer" },
                "response_headers": {
                    "type": "object",
                    "description": "Replaces the entire response header map. Omit the field to leave headers unchanged. To add or overwrite headers without deleting the rest, use response_header_upserts."
                },
                "response_header_upserts": {
                    "type": "object",
                    "description": "Adds or overwrites response headers by name (case-insensitive, first match). Other headers are kept. Does not delete headers."
                },
                "response_body":    { "type": "string" },
                "message_content":  { "type": "string" }
            }
        }),
        false,
        false,
    )
        .with_output(ack_output_schema(json!({
            "key": { "type": "string" },
            "action": { "type": "string" },
        }))),
    )
}

pub async fn set_intercept(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let url_pattern = require_str(&args, "url_pattern")?.to_string();
    let phase = args
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("request");

    // Derived from the call, so retrying the same request targets the same rule instead of adding
    // another one.
    let rule_id = super::stable_rule_id("probe-intercept", &[&url_pattern, phase]);
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
    ok_ack(
        format!(
            "Intercept breakpoint set (rule_id: {}). Waiting for a matching request.",
            rule_id
        ),
        json!({
            "rule_id": rule_id,
            "url_pattern": url_pattern,
            "phase": phase,
        }),
    )
}

pub async fn get_pending_intercepts(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    ok_json(&ctx.intercepts.intercept_snapshot().await)
}

pub async fn resume_flow(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
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
    ok_ack(
        format!("Flow {} resumed with action '{}'", key, action),
        json!({ "key": key, "action": action }),
    )
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
        let outcome = get_pending_intercepts(&ctx)
            .await
            .expect("tool should succeed");

        assert_eq!(outcome.structured["pending_count"], 0);
        assert_eq!(outcome.structured["ws_pending_count"], 0);

        // The text half is the serialized structured half, so a client that only reads text sees
        // exactly the same facts.
        let from_text: serde_json::Value =
            serde_json::from_str(&outcome.text).expect("text fallback should be valid json");
        assert_eq!(from_text, outcome.structured);
    }
}
