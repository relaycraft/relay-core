use super::ToolError;
use super::{ToolOutcome, ToolSpec, ack_output_schema, ok_ack, ok_json, require_str, tool};
use crate::server::ProbeContext;
use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch};
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::rule::MockResponseRuleConfig;
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn set_rule_schema() -> Tool {
    tool(
        ToolSpec::write(
        "set_rule",
        "Add or replace one traffic rule. `actions` is an array of {type, config} objects; \
         a single action object is rejected. Filters use the same {type, config} shape, and a \
         string match is {mode, value}. A text body is {type: Text, value}. constraints may be null. \
         A rule with the same id is replaced.",
        json!({
            "type": "object",
            "required": ["rule"],
            "properties": {
                "rule": {
                    "type": "object",
                    "description": "One rule. actions is an array. Example: {\"id\":\"r1\",\"name\":\"r1\",\"active\":true,\"stage\":\"RequestHeaders\",\"priority\":200,\"termination\":\"Stop\",\"filter\":{\"type\":\"Url\",\"config\":{\"mode\":\"Contains\",\"value\":\"example\"}},\"actions\":[{\"type\":\"AddRequestHeader\",\"config\":{\"name\":\"X-Test\",\"value\":\"1\"}}],\"constraints\":null}",
                    "required": ["id", "name", "active", "stage", "termination", "filter", "actions"],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "active": { "type": "boolean" },
                        "stage": {
                            "type": "string",
                            "enum": ["Connect", "RequestHeaders", "RequestBody", "ResponseHeaders", "ResponseBody", "WebSocketMessage"]
                        },
                        "priority": { "type": "integer" },
                        "termination": { "type": "string", "enum": ["Continue", "Stop"] },
                        "filter": {
                            "type": "object",
                            "required": ["type"],
                            "description": "Adjacent tag. Url/Host/Path config is {mode, value}. And/Or config is an array of filters. All has no config.",
                            "properties": {
                                "type": { "type": "string" },
                                "config": { "description": "Shape depends on type: object, array, string, or number." }
                            }
                        },
                        "actions": {
                            "type": "array",
                            "description": "Each item is {type, config}. Unit actions such as Drop and Inspect omit config. MockResponse headers is an object, and its body is {type, value}.",
                            "items": {
                                "type": "object",
                                "required": ["type"],
                                "properties": {
                                    "type": { "type": "string" },
                                    "config": { "type": "object" }
                                }
                            }
                        },
                        "constraints": {
                            "type": ["object", "null"],
                            "properties": {
                                "timeout_ms": { "type": ["integer", "null"] }
                            }
                        }
                    }
                }
            }
        }),
        false,
        true,
    )
        .with_output(ack_output_schema(json!({
            "rule_id": { "type": "string" },
        }))),
    )
}

pub fn list_rules_schema() -> Tool {
    tool(
        ToolSpec::read_only(
            "list_rules",
            "List the rules currently loaded in the daemon. Use this before set_rule or delete_rule \
             so a change replaces or removes the rule that is actually active.",
            json!({ "type": "object", "properties": {} }),
        )
        .with_output(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" },
                "rules": { "type": "array", "items": { "type": "object" } }
            },
            "required": ["count", "rules"]
        })),
    )
}

pub fn delete_rule_schema() -> Tool {
    tool(
        ToolSpec::write(
            "delete_rule",
            "Delete a rule by ID.",
            json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "description": "Rule ID to delete" }
                }
            }),
            true,
            true,
        )
        .with_output(ack_output_schema(json!({
            "rule_id": { "type": "string" },
        }))),
    )
}

pub fn mock_url_schema() -> Tool {
    tool(
        ToolSpec::write(
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
        false,
        true,
    )
        .with_output(ack_output_schema(json!({
            "rule_id": { "type": "string" },
            "url_pattern": { "type": "string" },
        }))),
    )
}

pub fn get_policy_schema() -> Tool {
    tool(ToolSpec::read_only(
        "get_policy",
        "Get current proxy policy (including redaction settings).",
        json!({
            "type": "object",
            "properties": {}
        }),
    ))
}

pub fn update_policy_schema() -> Tool {
    tool(
        ToolSpec::write(
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
        true,
        true,
    )
        .with_output(ack_output_schema(json!({}))),
    )
}

pub fn patch_policy_schema() -> Tool {
    tool(
        ToolSpec::write(
        "patch_policy",
        "Partially update proxy policy. Accepted fields are `redaction`, `upstream`, and \
         `retention`. A retention field you omit stays as it is; `null` removes that bound. \
         The default keeps 5000 flows and drops anything older than 7 days. Any other field is \
         rejected and the current policy is left unchanged. To change fields such as \
         request_timeout_ms, send a full policy via update_policy.",
        json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "object",
                    "description": "ProxyPolicyPatch. Accepted: redaction, upstream, retention. Example: {\"retention\": {\"max_flows\": 5000, \"max_age_secs\": 604800}}"
                }
            }
        }),
        false,
        true,
    )
        .with_output(ack_output_schema(json!({}))),
    )
}

pub async fn list_rules(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    let rules = ctx.rules.get_rules().await;
    let count = rules.len();
    ok_json(&json!({ "count": count, "rules": rules }))
}

pub async fn set_rule(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let rule_val = args.get("rule").ok_or("Missing 'rule' parameter")?;
    let rule = relay_core_api::rule::parse_rule(rule_val)?;

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

    ok_ack(
        format!("Rule '{}' set successfully.", rule_id),
        json!({ "rule_id": rule_id }),
    )
}

pub async fn delete_rule(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
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
        ok_ack(format!("Rule '{}' deleted.", id), json!({ "rule_id": id }))
    } else {
        Err(format!("Rule '{}' not found.", id).into())
    }
}

pub async fn mock_url(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
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

    // Derived from the call: `mock_url` upserts by id, so a stable id is what makes a retry an
    // update rather than a duplicate.
    let rule_id = super::stable_rule_id("probe-mock", &[&url_pattern, &status.to_string()]);
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

    ok_ack(
        format!(
            "Mock created (rule_id: {}). All requests matching '{}' will return {}.",
            rule_id, url_pattern, status
        ),
        json!({
            "rule_id": rule_id,
            "url_pattern": url_pattern,
            "status": status,
        }),
    )
}

pub async fn get_policy(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    ok_json(&ctx.policy.policy_snapshot())
}

pub async fn update_policy(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let policy_val = args.get("policy").ok_or("Missing 'policy' parameter")?;
    let policy: ProxyPolicy = serde_json::from_value(policy_val.clone())
        .map_err(|e| format!("Invalid policy JSON: {}", e))?;

    ctx.policy
        .update_policy_from(AuditActor::Probe, "probe.policy".to_string(), policy);
    ok_ack("Policy updated.", json!({}))
}

pub async fn patch_policy(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let patch_val = args.get("patch").ok_or("Missing 'patch' parameter")?;
    let patch: ProxyPolicyPatch = serde_json::from_value(patch_val.clone())
        .map_err(|e| format!("Invalid patch JSON: {}", e))?;

    ctx.policy
        .patch_policy_from(AuditActor::Probe, "probe.policy.patch".to_string(), patch);
    ok_ack("Policy patched.", json!({}))
}
