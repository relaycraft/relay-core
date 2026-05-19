use super::{make_tool, ok_text, require_str};
use crate::server::ProbeContext;
use relay_core_runtime::audit::AuditActor;
use rmcp::model::{Content, Tool};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn set_script_schema() -> Tool {
    make_tool(
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
    )
}

pub async fn set_script(ctx: &Arc<ProbeContext>, args: Value) -> Result<Vec<Content>, String> {
    let script = require_str(&args, "script")?.to_string();
    ctx.script
        .load_script_from(AuditActor::Probe, "probe.set_script".to_string(), &script)
        .await?;
    ok_text(format!("Script loaded ({} bytes).", script.len()))
}
