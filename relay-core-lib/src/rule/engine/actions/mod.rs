pub mod common;
pub mod http;
pub mod transform;
pub mod utils;
pub mod ws;

use crate::rule::engine::executor::ExecutionContext;
use crate::rule::model::{Action, TerminalReason};
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

        Action::MockWebSocketMessage { .. } | Action::DropWebSocketMessage => {
            ws::execute(action, flow).await
        }
    }
}

/// The wire-visible fields an action targets, named as they appear in a `Flow`.
///
/// Roadmap §4-4: a `MutationApplied` event has to answer *what changed*, not only that some rule ran.
/// Derived from the action itself rather than by diffing the `Flow`, so it costs nothing on the hot
/// path and cannot report a change the action did not make.
///
/// The list is empty for actions that only end the exchange (`Drop`, `Abort`) or only touch
/// engine-side bookkeeping (`Tag`, `SetVariable`, `Delay`): nothing on the wire is *changed* by
/// them, and inventing a field name would make the event less trustworthy than saying nothing.
///
/// A new [`Action`] variant fails to compile here until it is named, which is the point — an
/// unnamed mutation is exactly the invisible change this exists to prevent.
pub fn mutated_fields(action: &Action) -> &'static [&'static str] {
    match action {
        // Control: the exchange ends or is shaped, but no field of the exchange changes.
        Action::Drop
        | Action::Abort
        | Action::Delay { .. }
        | Action::Throttle { .. }
        | Action::Tag { .. }
        | Action::Inspect
        | Action::SetVariable { .. }
        | Action::RateLimit { .. }
        | Action::DropWebSocketMessage => &[],

        // L3/L4 target changes.
        Action::RedirectIp { .. } => &["network.server_ip"],
        Action::SetTtl { .. } => &["network.ttl"],
        Action::ForwardPort { .. } => &["network.server_port"],
        Action::MapRemote { .. } => &["network.server_ip", "network.server_port", "request.url"],

        // Whole-response replacements.
        Action::MockResponse { .. } | Action::MapLocal { .. } => &["response"],

        // Request fields.
        Action::SetRequestMethod { .. } => &["request.method"],
        Action::SetRequestUrl { .. } => &["request.url"],
        Action::SetRequestBody { .. } | Action::TransformRequestBody { .. } => &["request.body"],
        Action::AddRequestHeader { .. }
        | Action::UpdateRequestHeader { .. }
        | Action::DeleteRequestHeader { .. } => &["request.headers"],

        // Response fields.
        Action::SetResponseStatus { .. } => &["response.status"],
        Action::SetResponseBody { .. } | Action::TransformResponseBody { .. } => &["response.body"],
        Action::AddResponseHeader { .. }
        | Action::UpdateResponseHeader { .. }
        | Action::DeleteResponseHeader { .. } => &["response.headers"],
        Action::Redirect { .. } => &["response.status", "response.headers"],

        // Message-oriented transports.
        Action::MockWebSocketMessage { .. } => &["websocket.message"],
    }
}

#[cfg(test)]
mod tests {
    use super::mutated_fields;
    use crate::rule::model::{Action, BodySource, BodyTransform, WebSocketDirection};
    use std::collections::HashMap;

    fn header_action() -> Action {
        Action::AddRequestHeader {
            name: "x".to_string(),
            value: "y".to_string(),
        }
    }

    /// The field vocabulary is what a UI or an agent reads to explain a change, so a sample of the
    /// actions that reach the wire is pinned rather than left to drift with the implementation.
    #[test]
    fn wire_actions_name_the_fields_they_change() {
        let cases: Vec<(Action, &[&str])> = vec![
            (
                Action::SetRequestUrl {
                    url: "https://example.com/".to_string(),
                },
                &["request.url"],
            ),
            (
                Action::SetResponseStatus { status: 418 },
                &["response.status"],
            ),
            (
                Action::SetRequestBody {
                    body: BodySource::Text("x".to_string()),
                },
                &["request.body"],
            ),
            (
                Action::TransformResponseBody {
                    transform: BodyTransform::RegexReplace {
                        pattern: "a".to_string(),
                        replacement: "b".to_string(),
                    },
                },
                &["response.body"],
            ),
            (
                Action::MapRemote {
                    url: "https://other.example/".to_string(),
                    preserve_host: false,
                },
                &["network.server_ip", "network.server_port", "request.url"],
            ),
            (
                Action::MockResponse {
                    status: 200,
                    headers: HashMap::new(),
                    body: None,
                },
                &["response"],
            ),
            (header_action(), &["request.headers"]),
            (
                Action::MockWebSocketMessage {
                    direction: WebSocketDirection::Incoming,
                    message: "x".to_string(),
                },
                &["websocket.message"],
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(
                mutated_fields(&action),
                expected,
                "{action:?} should name the fields it changes"
            );
        }
    }

    /// Control and bookkeeping actions must name nothing. Claiming a field for them would make
    /// "what changed" answerable with a change that did not happen.
    #[test]
    fn control_actions_claim_no_field_change() {
        for action in [
            Action::Drop,
            Action::Abort,
            Action::Inspect,
            Action::Delay { ms: 10 },
            Action::Throttle { kbps: 100 },
            Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            },
            Action::SetVariable {
                name: "n".to_string(),
                value: "v".to_string(),
            },
            Action::RateLimit {
                key: "ip".to_string(),
                limit: 1,
                window_ms: 1000,
            },
            Action::DropWebSocketMessage,
        ] {
            assert!(
                mutated_fields(&action).is_empty(),
                "{action:?} changes no field and must not claim one"
            );
        }
    }
}
