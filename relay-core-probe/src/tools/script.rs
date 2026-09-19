use super::ToolError;
use super::{ToolOutcome, ToolSpec, ack_output_schema, ok_ack, require_str, tool};
use crate::server::ProbeContext;
use relay_core_runtime::audit::AuditActor;
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn set_script_schema() -> Tool {
    tool(
        ToolSpec::write(
            "set_script",
            "Load a JavaScript (Deno) script for dynamic request/response modification. \
         The script runs inside the Deno/V8 engine and can hook into onRequest, onResponse, \
         onRequestHeaders, onResponseHeaders, and onWebSocketMessage events.",
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
