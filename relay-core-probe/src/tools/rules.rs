use super::{make_tool, ok_json, ok_text, require_str};
use crate::server::ProbeContext;
use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch};
use relay_core_api::rule::Rule;
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::rule::MockResponseRuleConfig;
use rmcp::model::{Content, Tool};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub fn set_rule_schema() -> Tool {
    make_tool(
        "set_rule",
        "Add or replace a traffic rule. Accepts the full Rule JSON object. \
         If a rule with the same ID already exists, it is replaced.",
        json!({
            "type": "object",
            "required": ["rule"],
            "properties": {
                "rule": {
                    "type": "object",
                    "description": "Full Rule object (id, name, active, stage, filter, actions, priority, termination)"
                }
            }
        }),
    )
}

pub fn delete_rule_schema() -> Tool {
    make_tool(
        "delete_rule",
        "Delete a rule by ID.",
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Rule ID to delete" }
            }
        }),
    )
}

pub fn mock_url_schema() -> Tool {
    make_tool(
        "mock_url",
        "Quickly mock all requests matching a URL pattern to return a fixed response. \
         Creates a MockResponse rule with the given status, headers, and body.",
        json!({
            "type": "object",
            "required": ["url_pattern", "status"],
            "properties": {
                "url_pattern": { "type": "string", "description": "URL substring or regex to match" },
                "status":      { "type": "integer", "description": "HTTP status code to return" },
                "body":        { "type": "string",  "description": "Response body (default empty)" },
                "content_type":{ "type": "string",  "description": "Content-Type header (default application/json)" }
            }
        }),
    )
}

pub fn get_policy_schema() -> Tool {
    make_tool(
        "get_policy",
        "Get current proxy policy (including redaction settings).",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

pub fn update_policy_schema() -> Tool {
    make_tool(
        "update_policy",
        "Replace the current proxy policy with a full ProxyPolicy object.",
        json!({
            "type": "object",
            "required": ["policy"],
            "properties": {
                "policy": {
                    "type": "object",
                    "description": "Full ProxyPolicy object. Include redaction to enable/disable desensitization."
                }
            }
        }),
    )
}

pub fn patch_policy_schema() -> Tool {
    make_tool(
        "patch_policy",
        "Partially update proxy policy with merge-patch semantics.",
        json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "object",
                    "description": "ProxyPolicyPatch object. Example: {\"redaction\": {\"enabled\": true}}"
                }
            }
        }),
    )
}

pub async fn set_rule(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let rule_val = args.get("rule").ok_or("Missing 'rule' parameter")?;
    let rule: Rule = serde_json::from_value(rule_val.clone())
        .map_err(|e| format!("Invalid rule JSON: {}", e))?;

    let rule_id = rule.id.clone();
    ctx.rules
        .upsert_rule_from(
            AuditActor::Probe,
            "rule.upsert",
            rule_id.clone(),
            json!({ "tool": "set_rule" }),
            rule,
        )
        .await?;

    ok_text(format!("Rule '{}' set successfully.", rule_id))
}

pub async fn delete_rule(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let id = require_str(&args, "id")?.to_string();
    let deleted = ctx
        .rules
        .delete_rule_from(
            AuditActor::Probe,
            "rule.delete",
            id.clone(),
            json!({ "tool": "delete_rule" }),
            &id,
        )
        .await?;

    if deleted {
        ok_text(format!("Rule '{}' deleted.", id))
    } else {
        Err(format!("Rule '{}' not found.", id))
    }
}

pub async fn mock_url(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let url_pattern = require_str(&args, "url_pattern")?.to_string();
    let status = args
        .get("status")
        .and_then(Value::as_u64)
        .ok_or("Missing 'status' parameter")? as u16;
    let body = args
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let content_type = args
        .get("content_type")
        .and_then(Value::as_str)
        .unwrap_or("application/json")
        .to_string();

    let rule_id = format!("probe-mock-{}", Uuid::new_v4());
    ctx.rules
        .create_mock_response_rule_from(
            AuditActor::Probe,
            rule_id.clone(),
            json!({
                "tool": "mock_url",
                "url_pattern": url_pattern,
                "status": status
            }),
            MockResponseRuleConfig {
                rule_id: rule_id.clone(),
                url_pattern: url_pattern.clone(),
                name: format!("probe-mock:{}", url_pattern),
                status,
                content_type,
                body,
            },
        )
        .await?;

    ok_text(format!(
        "Mock created (rule_id: {}). All requests matching '{}' will return {}.",
        rule_id, url_pattern, status
    ))
}

pub async fn get_policy(ctx: &Arc<ProbeContext>) -> Result<Vec<Content>, String> {
    ok_json(&ctx.policy.policy_snapshot())
}

pub async fn update_policy(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let policy_val = args.get("policy").ok_or("Missing 'policy' parameter")?;
    let policy: ProxyPolicy = serde_json::from_value(policy_val.clone())
        .map_err(|e| format!("Invalid policy JSON: {}", e))?;

    ctx.policy
        .update_policy_from(AuditActor::Probe, "probe.policy".to_string(), policy);
    ok_text("Policy updated.")
}

pub async fn patch_policy(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let patch_val = args.get("patch").ok_or("Missing 'patch' parameter")?;
    let patch: ProxyPolicyPatch = serde_json::from_value(patch_val.clone())
        .map_err(|e| format!("Invalid patch JSON: {}", e))?;

    ctx.policy
        .patch_policy_from(AuditActor::Probe, "probe.policy.patch".to_string(), patch);
    ok_text("Policy patched.")
}
