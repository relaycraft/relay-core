use crate::services::InterceptService;
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
        TerminalReason::Inspect => await_user_inspect(intercepts, flow, phase, ws_message).await,
    }
}

async fn await_user_inspect(
    intercepts: &Arc<dyn InterceptService>,
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
    if let Some(msg) = ws_message {
        intercepts
            .set_pending_ws_message(key.clone(), msg.clone())
            .await;
    }

    match timeout(INSPECT_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => {
            let _ = intercepts
                .resolve_intercept(key, InterceptionResult::Continue)
                .await;
            InterceptionResult::Continue
        }
    }
}
