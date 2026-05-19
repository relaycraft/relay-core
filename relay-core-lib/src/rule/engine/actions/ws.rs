use crate::rule::engine::actions::ActionOutcome;
use crate::rule::model::{Action, TerminalReason, WebSocketDirection};
use relay_core_api::flow::{BodyData, Direction, Flow, Layer};

pub async fn execute(action: &Action, flow: &mut Flow) -> ActionOutcome {
    match action {
        Action::MockWebSocketMessage { direction, message } => {
            if let Layer::WebSocket(ws) = &mut flow.layer {
                if let Some(msg) = ws.messages.last_mut() {
                    msg.content = BodyData {
                        encoding: "utf-8".to_string(),
                        content: message.clone(),
                        size: message.len() as u64,
                    };
                    msg.direction = match direction {
                        WebSocketDirection::Incoming => Direction::ServerToClient,
                        WebSocketDirection::Outgoing => Direction::ClientToServer,
                    };
                    msg.opcode = "Text".to_string();
                    ActionOutcome::Terminated(TerminalReason::Mock)
                } else {
                    ActionOutcome::Failed("No WebSocket message to mock".to_string())
                }
            } else {
                ActionOutcome::Failed("Not a WebSocket flow".to_string())
            }
        }
        Action::DropWebSocketMessage => {
            if let Layer::WebSocket(_ws) = &mut flow.layer {
                ActionOutcome::Terminated(TerminalReason::Drop)
            } else {
                ActionOutcome::Failed("Not a WebSocket flow".to_string())
            }
        }
        _ => ActionOutcome::Failed(format!(
            "Action {:?} not supported in websocket handler",
            action
        )),
    }
}
