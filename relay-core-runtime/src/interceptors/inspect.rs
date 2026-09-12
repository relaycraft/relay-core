use crate::services::{FlowEventSink, InterceptService};
use relay_core_api::event::FlowEvent;
use relay_core_api::flow::{Flow, Layer, WebSocketMessage};
use relay_core_api::rule::TerminalReason;
use relay_core_lib::InterceptionResult;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};

/// Max wait for a UI/agent to resolve an intercepted flow.
///
/// Chosen as the midpoint between the prior divergent values:
/// runtime was 300 s, Tauri adapter was 30 s. 60 s balances UI
/// responsiveness with enough time for human review. Callers relying
/// on the old 300 s budget will need to adjust — inspect is a
/// breakpoint, not a queue, and should not block indefinitely.
pub const INSPECT_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn handle_rule_termination(
    intercepts: &Arc<dyn InterceptService>,
    events: &Arc<dyn FlowEventSink>,
    reason: &TerminalReason,
    flow: &Flow,
    phase: &str,
    ws_message: Option<&WebSocketMessage>,
) -> InterceptionResult {
    match reason {
        TerminalReason::Drop | TerminalReason::Abort | TerminalReason::RateLimited => {
            InterceptionResult::Drop
        }
        TerminalReason::Mock | TerminalReason::Redirect => match &flow.layer {
            Layer::Http(http) => {
                if let Some(res) = &http.response {
                    InterceptionResult::MockResponse(res.clone())
                } else {
                    InterceptionResult::Drop
                }
            }
            Layer::WebSocket(ws) => {
                if ws.handshake_response.status != 0 && ws.handshake_response.status != 101 {
                    InterceptionResult::MockResponse(ws.handshake_response.clone())
                } else {
                    InterceptionResult::Drop
                }
            }
            _ => InterceptionResult::Drop,
        },
        TerminalReason::Inspect => {
            await_user_inspect(intercepts, events, flow, phase, ws_message).await
        }
    }
}

async fn await_user_inspect(
    intercepts: &Arc<dyn InterceptService>,
    events: &Arc<dyn FlowEventSink>,
    flow: &Flow,
    phase: &str,
    ws_message: Option<&WebSocketMessage>,
) -> InterceptionResult {
    let (tx, rx) = oneshot::channel();
    let key = if let Some(msg) = ws_message {
        format!("{}:ws_msg:{}", flow.id, msg.id)
    } else {
        format!("{}:{}", flow.id, phase)
    };

    intercepts.register_intercept(key.clone(), tx).await;

    // A consumer watching the stream has to learn that this exchange is now waiting on a human.
    // `event: intercept` was documented as this signal and had no producer at all.
    events.publish_flow_event(FlowEvent::InterceptPaused {
        flow_id: flow.id,
        phase: phase.to_string(),
    });

    if let Some(msg) = ws_message {
        intercepts
            .set_pending_ws_message(key.clone(), msg.clone())
            .await;
    }

    let result = match timeout(INSPECT_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => {
            let _ = intercepts
                .resolve_intercept(key, InterceptionResult::Continue)
                .await;
            InterceptionResult::Continue
        }
    };

    // Published here rather than where the resolution arrives: this is the point that knows both
    // the structured flow id and phase *and* how the wait actually ended, including the timeout
    // path, which resolves from inside this function.
    events.publish_flow_event(FlowEvent::InterceptResolved {
        flow_id: flow.id,
        phase: phase.to_string(),
        mutated: !matches!(result, InterceptionResult::Continue),
    });

    result
}
