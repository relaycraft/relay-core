use super::ToolError;
use super::{ToolOutcome, ToolSpec, ack_output_schema, ok_ack, ok_json, require_str, tool};
use crate::server::ProbeContext;
use relay_core_runtime::audit::AuditActor;
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn get_script_schema() -> Tool {
    tool(
        ToolSpec::read_only(
            "get_script",
            "Read the JavaScript source currently loaded in the daemon. \
             loaded is false until the first successful set_script. A failed reload leaves the \
             previous source in place.",
            json!({ "type": "object", "properties": {} }),
        )
        .with_output(json!({
            "type": "object",
            "properties": {
                "loaded": { "type": "boolean" },
                "script": { "type": ["string", "null"] }
            },
            "required": ["loaded", "script"]
        })),
    )
}

pub fn set_script_schema() -> Tool {
    tool(
        ToolSpec::write(
            "set_script",
            "Load a JavaScript (Deno) script for dynamic request/response modification. \
         Hooks: onRequestHeaders(context, flow), onResponseHeaders(context, flow), \
         onRequest(body, flow), onResponse(body, flow), \
         onWebSocketMessage(context, flow, message). \
         Return the flow from onResponseHeaders to keep streaming the upstream body. \
         onWebSocketMessage returns the message, the string DROP, or nothing. Returning the \
         flow, or throwing, tags only that flow with script-error; the next flow is unaffected \
         and a later set_script replaces the hook. A WebSocket flow's layer type is WebSocket, \
         not Http. \
         onRequest and onResponse may be async; body.text() and body.json() read a buffered \
         copy up to 1 MiB, and a hook that does not finish is aborted so the original body \
         is still forwarded.",
            json!({
                "type": "object",
                "required": ["script"],
                "properties": {
                    "script": {
                        "type": "string",
                        "description": "JavaScript source code to load into the script engine"
                    }
                }
            }),
            true,
            true,
        )
        .with_output(ack_output_schema(json!({
            "bytes": { "type": "integer" },
        }))),
    )
}

pub async fn get_script(ctx: &Arc<ProbeContext>) -> Result<ToolOutcome, ToolError> {
    let script = ctx.script.current_script();
    ok_json(&json!({
        "loaded": script.is_some(),
        "script": script,
    }))
}

pub async fn set_script(ctx: &Arc<ProbeContext>, args: Value) -> Result<ToolOutcome, ToolError> {
    let script = require_str(&args, "script")?.to_string();
    ctx.script
        .load_script_from(AuditActor::Probe, "probe.set_script".to_string(), &script)
        .await?;
    ok_ack(
        format!("Script loaded ({} bytes).", script.len()),
        json!({ "bytes": script.len() }),
    )
}
