//! Lifecycle tools: let an agent see and control the proxy instead of discovering, too late, that
//! there was never one running.
//!
//! Decision [`0007`](../../docs/decisions/0007-daemon-control-plane.md). Before these tools the MCP
//! surface could only read traffic: an agent that connected to a daemon whose proxy was stopped saw
//! empty results and had no way to tell that from "no traffic", and no way to fix it.

use super::{ToolError, ToolOutcome, ToolSpec, ok_ack, ok_json, tool};
use crate::server::ProbeContext;
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::services::{
    ProxyControlError, ProxyStartOutcome, ProxyStartRequest, Requester,
};
use rmcp::model::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

/// Lifecycle tools, and whether each one changes the machine.
struct LifecycleTool {
    name: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

fn definitions() -> Vec<LifecycleTool> {
    vec![
        LifecycleTool {
            name: "proxy_status",
            description: "Report whether the proxy is running, on which port, for how long, and why \
                          it failed if it did. Call this first when traffic tools return nothing: \
                          it distinguishes \"no traffic yet\" from \"nothing is being captured\".",
            read_only: true,
            destructive: false,
            idempotent: true,
        },
        LifecycleTool {
            name: "proxy_start",
            description: "Start the proxy on the daemon that owns the engine. Idempotent: starting \
                          an already-running proxy succeeds and reports the port it is listening \
                          on, so it is safe to call when unsure. Fails with a reason (for example \
                          the port is taken) instead of reporting success for a proxy that never \
                          bound.",
            read_only: false,
            destructive: false,
            idempotent: true,
        },
        LifecycleTool {
            name: "proxy_stop",
            description: "Stop the proxy. The daemon keeps running, so captured history, rules and \
                          intercepts are preserved and proxy_start brings the proxy back. \
                          Idempotent. Requests in flight are dropped.",
            read_only: false,
            destructive: true,
            idempotent: true,
        },
    ]
}

fn schema(name: &str) -> Value {
    match name {
        "proxy_start" => json!({
            "type": "object",
            "properties": {
                "port": {
                    "type": "integer",
                    "description": "Proxy listen port (default 8080)"
                },
                "transparent": {
                    "type": "boolean",
                    "description": "Enable transparent capture mode (macOS PF / Linux TPROXY)"
                },
                "udp_tproxy_port": {
                    "type": "integer",
                    "description": "Enable UDP TPROXY on this port (Linux only)"
                }
            }
        }),
        _ => json!({ "type": "object", "properties": {} }),
    }
}

pub fn lifecycle_tool_schemas() -> Vec<Tool> {
    definitions()
        .iter()
        .map(|definition| {
            let input = schema(definition.name);
            let spec = if definition.read_only {
                ToolSpec::read_only(definition.name, definition.description, input)
            } else {
                ToolSpec::write(
                    definition.name,
                    definition.description,
                    input,
                    definition.destructive,
                    definition.idempotent,
                )
            };
            tool(spec.with_output(output_schema(definition.name)))
        })
        .collect()
}

/// Structured result shapes. Declared because they are stable, and because an agent can then read
/// `phase` or `port` without parsing anything.
fn output_schema(name: &str) -> Value {
    match name {
        "proxy_status" => json!({
            "type": "object",
            "properties": {
                "phase": { "type": "string", "description": "created | starting | running | stopping | stopped | failed" },
                "running": { "type": "boolean" },
                "port": { "type": ["integer", "null"] },
                "uptime_seconds": { "type": ["integer", "null"] },
                "last_error": { "type": ["string", "null"] },
                "hint": { "type": "string" }
            },
            "required": ["phase", "running", "hint"],
        }),
        _ => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "outcome": { "type": "string", "description": "started | already_running | stopped | already_stopped" },
                "port": { "type": ["integer", "null"] },
                "message": { "type": "string" }
            },
            "required": ["ok", "outcome", "message"],
        }),
    }
}

/// Is `name` one of this module's tools?
pub fn handles(name: &str) -> bool {
    definitions().iter().any(|tool| tool.name == name)
}

pub async fn dispatch(
    ctx: &Arc<ProbeContext>,
    name: &str,
    args: Value,
) -> Result<ToolOutcome, ToolError> {
    let proxy = ctx
        .proxy
        .clone()
        .ok_or_else(|| ToolError::unavailable("proxy_control_unavailable", PROXY_CONTROL_HINT))?;

    match name {
        "proxy_status" => {
            let lifecycle = proxy.proxy_lifecycle();
            ok_json(&json!({
                "phase": lifecycle.phase.as_str(),
                "running": lifecycle.is_active(),
                "port": lifecycle.port,
                "started_at_ms": lifecycle.started_at_ms,
                "uptime_seconds": lifecycle.uptime_seconds(),
                "last_error": lifecycle.last_error,
                "hint": if lifecycle.is_active() {
                    "The proxy is capturing. Configure clients to use it as their HTTP proxy."
                } else {
                    "The proxy is not running: captured history is still readable, but no new \
                     traffic is being captured. Call proxy_start to begin."
                },
            }))
        }
        "proxy_start" => {
            let request = ProxyStartRequest {
                port: args
                    .get("port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok())
                    .unwrap_or(relay_core_runtime::services::DEFAULT_PROXY_PORT),
                transparent: args
                    .get("transparent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                udp_tproxy_port: args
                    .get("udp_tproxy_port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok()),
                ca_cert: None,
                ca_key: None,
            };

            match proxy
                .proxy_start(
                    Requester::new(AuditActor::Probe).label("mcp:proxy_start"),
                    request,
                )
                .await
            {
                Ok(ProxyStartOutcome::Started { port }) => ok_ack(
                    format!(
                        "Proxy listening on 127.0.0.1:{port}. Point the client under test at it, \
                         then use search_flows to read the traffic."
                    ),
                    json!({ "outcome": "started", "port": port }),
                ),
                Ok(ProxyStartOutcome::AlreadyRunning { port }) => ok_ack(
                    format!("A proxy is already listening on 127.0.0.1:{port}."),
                    json!({ "outcome": "already_running", "port": port }),
                ),
                Err(error) => Err(control_error_to_tool_error(error)),
            }
        }
        "proxy_stop" => match proxy
            .proxy_stop(Requester::new(AuditActor::Probe).label("mcp:proxy_stop"))
            .await
        {
            Ok(outcome) => ok_ack(
                "The proxy is stopped. The daemon kept the captured history and rules; \
                 proxy_start brings it back.",
                json!({
                    "outcome": match outcome {
                        relay_core_runtime::services::ProxyStopOutcome::Stopped => "stopped",
                        relay_core_runtime::services::ProxyStopOutcome::AlreadyStopped => "already_stopped",
                    },
                }),
            ),
            Err(error) => Err(control_error_to_tool_error(error)),
        },
        other => Err(ToolError::not_found(format!("Unknown tool: {other}"))),
    }
}

/// Message used when the host serves the tools but exposes no proxy lifecycle — a host that
/// embedded the probe without passing a controller.
pub const PROXY_CONTROL_HINT: &str = "this MCP server does not own a proxy lifecycle, so it cannot \
     start or stop one. Connect through the daemon (`relay start`), which owns the proxy, and the \
     lifecycle tools become available.";

/// Turn a control-plane failure into a tool error an agent can branch on.
fn control_error_to_tool_error(error: ProxyControlError) -> ToolError {
    ToolError::unavailable(error.code().as_str(), error.to_string())
}
