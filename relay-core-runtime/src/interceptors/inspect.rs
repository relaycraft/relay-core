use crate::CoreState;
use relay_core_api::flow::{Flow, Layer, WebSocketMessage};
use relay_core_api::rule::TerminalReason;
use relay_core_lib::InterceptionResult;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};

pub async fn handle_rule_termination(
    state: &Arc<CoreState>,
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
        TerminalReason::Inspect => await_user_inspect(state, flow, phase, ws_message).await,
    }
}

async fn await_user_inspect(
    state: &Arc<CoreState>,
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

    state.register_intercept(key.clone(), tx).await;
    if let Some(msg) = ws_message {
        state.set_pending_ws_message(key.clone(), msg.clone()).await;
    }

    match timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => {
            let _ = state
                .resolve_intercept(key, InterceptionResult::Continue)
                .await;
            InterceptionResult::Continue
        }
    }
}
