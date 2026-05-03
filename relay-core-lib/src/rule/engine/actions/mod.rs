pub mod common;
pub mod http;
pub mod ws;
pub mod transform;
pub mod utils;

use crate::rule::model::{Action, TerminalReason};
use crate::rule::engine::executor::ExecutionContext;
use relay_core_api::flow::Flow;

#[derive(Debug, Clone)]
pub enum ActionOutcome {
    Continue,
    Terminated(TerminalReason),
    Failed(String),
}

pub async fn execute_action(
    action: &Action,
    flow: &mut Flow,
    ctx: &mut ExecutionContext,
) -> ActionOutcome {
    match action {
        Action::Drop
        | Action::Abort
        | Action::Inspect
        | Action::Delay { .. }
        | Action::SetVariable { .. }
        | Action::Tag { .. }
        | Action::RedirectIp { .. }
        | Action::SetTtl { .. }
        | Action::ForwardPort { .. }
        | Action::MapRemote { .. }
        | Action::RateLimit { .. }
        | Action::Throttle { .. } => common::execute(action, flow, ctx).await,

        Action::SetRequestMethod { .. }
        | Action::SetRequestUrl { .. }
        | Action::SetRequestBody { .. }
        | Action::SetResponseStatus { .. }
        | Action::SetResponseBody { .. }
        | Action::Redirect { .. }
        | Action::AddRequestHeader { .. }
        | Action::UpdateRequestHeader { .. }
        | Action::DeleteRequestHeader { .. }
        | Action::AddResponseHeader { .. }
        | Action::UpdateResponseHeader { .. }
        | Action::DeleteResponseHeader { .. }
        | Action::MockResponse { .. }
        | Action::MapLocal { .. }
        | Action::TransformRequestBody { .. }
        | Action::TransformResponseBody { .. } => http::execute(action, flow, ctx).await,

        Action::MockWebSocketMessage { .. }
        | Action::DropWebSocketMessage => ws::execute(action, flow).await,
    }
}
