//! The `/api/v1/events` SSE wire contract, in one place.
//!
//! Server and client previously encoded and decoded these frames independently: the adapter built
//! ad-hoc `serde_json::json!` objects per variant while the CLI hand-wrote its own `match` over the
//! event name. Nothing tied the two together, so a frame could be emitted with fields the decoder
//! never read (`http-body` dropped `direction` and `body` on the floor) or in a shape the decoder
//! could not parse at all (`ws-message` was never a tagged `FlowUpdate`).
//!
//! Both directions now go through this module, and a round-trip test over every variant keeps the
//! two ends from drifting apart again.

use crate::event::FlowEvent;
use crate::flow::{BodyData, Direction, Flow, FlowUpdate, WebSocketMessage};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Event names this engine emits on `/api/v1/events`.
///
/// Consumed by adapters when writing and by clients when dispatching; adding a frame means adding
/// its name here rather than spelling the string at each end.
pub mod event_name {
    /// A new or updated flow, as raw `Flow` JSON.
    pub const FLOW: &str = "flow";
    /// A WebSocket message on a flow.
    pub const WS_MESSAGE: &str = "ws-message";
    /// An HTTP body observed on a flow.
    pub const HTTP_BODY: &str = "http-body";
    /// A body that exceeded the inspection budget.
    pub const BODY_BUDGET_EXCEEDED: &str = "body-budget-exceeded";
    /// A typed flow lifecycle transition (roadmap §4-4).
    pub const FLOW_EVENT: &str = "flow-event";
}

/// One SSE frame: the `event:` name plus its `data:` payload, already serialised.
///
/// Kept as text because that is what actually travels; adapters pass it straight to their SSE
/// writer instead of re-deriving the payload shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// Value of the SSE `event:` field.
    pub event: &'static str,
    /// Value of the SSE `data:` field.
    pub data: String,
}

/// A frame could not be serialised.
///
/// Surfaced rather than swallowed into empty data: an empty `data:` field is silently ignored by
/// SSE clients, so a serialisation failure would look exactly like a quiet period.
#[derive(Debug)]
pub struct SseEncodeError(serde_json::Error);

impl fmt::Display for SseEncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to serialise SSE frame: {}", self.0)
    }
}

impl std::error::Error for SseEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Payload of `event: ws-message`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WsMessageFrame {
    /// Flow the message belongs to.
    pub flow_id: String,
    /// The message itself.
    pub message: WebSocketMessage,
}

/// Payload of `event: http-body`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpBodyFrame {
    /// Flow the body belongs to.
    pub flow_id: String,
    /// Which direction the body travelled.
    pub direction: Direction,
    /// The body, already redacted by the adapter when a redaction policy is active.
    pub body: BodyData,
}

/// Payload of `event: body-budget-exceeded`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BodyBudgetExceededFrame {
    /// Flow the body belongs to.
    pub flow_id: String,
    /// Direction whose body exceeded the budget.
    pub direction: Direction,
}

impl FlowUpdate {
    /// Encode this update as the SSE frame that carries it.
    pub fn to_sse(&self) -> Result<SseFrame, SseEncodeError> {
        fn encode<T: Serialize>(
            event: &'static str,
            payload: &T,
        ) -> Result<SseFrame, SseEncodeError> {
            Ok(SseFrame {
                event,
                data: serde_json::to_string(payload).map_err(SseEncodeError)?,
            })
        }

        match self {
            // Emitted as bare `Flow` JSON: this is the frame existing clients already parse, and
            // §4-5 keeps Flow JSON additive rather than re-wrapping it in a new envelope.
            FlowUpdate::Full(flow) => encode(event_name::FLOW, flow.as_ref()),
            FlowUpdate::WebSocketMessage { flow_id, message } => encode(
                event_name::WS_MESSAGE,
                &WsMessageFrame {
                    flow_id: flow_id.clone(),
                    message: message.clone(),
                },
            ),
            FlowUpdate::HttpBody {
                flow_id,
                direction,
                body,
            } => encode(
                event_name::HTTP_BODY,
                &HttpBodyFrame {
                    flow_id: flow_id.clone(),
                    direction: direction.clone(),
                    body: body.clone(),
                },
            ),
            FlowUpdate::BodyBudgetExceeded { flow_id, direction } => encode(
                event_name::BODY_BUDGET_EXCEEDED,
                &BodyBudgetExceededFrame {
                    flow_id: flow_id.clone(),
                    direction: direction.clone(),
                },
            ),
        }
    }
}

/// Decode one SSE frame into the update it carries.
///
/// Returns `None` for event names this engine does not own (`audit`, `lifecycle`, `lagged`, `ping`)
/// and for payloads that do not parse — a client should skip those rather than fault the stream.
pub fn parse_update(event: &str, data: &str) -> Option<FlowUpdate> {
    match event {
        event_name::FLOW => {
            // The adapter sends bare `Flow` JSON; accept a tagged `FlowUpdate` too, because early
            // builds did and a client should not lose the stream over it.
            if let Ok(update) = serde_json::from_str::<FlowUpdate>(data) {
                return Some(update);
            }
            serde_json::from_str::<Flow>(data)
                .ok()
                .map(|flow| FlowUpdate::Full(Box::new(flow)))
        }
        event_name::WS_MESSAGE => {
            let frame: WsMessageFrame = serde_json::from_str(data).ok()?;
            Some(FlowUpdate::WebSocketMessage {
                flow_id: frame.flow_id,
                message: frame.message,
            })
        }
        event_name::HTTP_BODY => {
            let frame: HttpBodyFrame = serde_json::from_str(data).ok()?;
            Some(FlowUpdate::HttpBody {
                flow_id: frame.flow_id,
                direction: frame.direction,
                body: frame.body,
            })
        }
        event_name::BODY_BUDGET_EXCEEDED => {
            let frame: BodyBudgetExceededFrame = serde_json::from_str(data).ok()?;
            Some(FlowUpdate::BodyBudgetExceeded {
                flow_id: frame.flow_id,
                direction: frame.direction,
            })
        }
        _ => None,
    }
}

impl FlowEvent {
    /// Encode this lifecycle event as the SSE frame that carries it.
    ///
    /// `FlowEvent` is additive to `FlowUpdate` (§4-5): snapshots stay on `event: flow`, and the
    /// transitions a consumer would otherwise have to infer by diffing snapshots travel separately.
    pub fn to_sse(&self) -> Result<SseFrame, SseEncodeError> {
        Ok(SseFrame {
            event: event_name::FLOW_EVENT,
            data: serde_json::to_string(self).map_err(SseEncodeError)?,
        })
    }
}

/// Decode one `event: flow-event` frame.
///
/// `None` for other event names and for payloads that do not parse, matching [`parse_update`].
pub fn parse_flow_event(event: &str, data: &str) -> Option<FlowEvent> {
    if event != event_name::FLOW_EVENT {
        return None;
    }
    serde_json::from_str(data).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        BodyBudgetExceededFrame, HttpBodyFrame, WsMessageFrame, event_name, parse_flow_event,
        parse_update,
    };
    use crate::event::{CloseReason, FlowEvent};
    use crate::flow::{BodyData, Direction, FlowUpdate, WebSocketMessage};
    use uuid::Uuid;

    fn body(content: &str) -> BodyData {
        BodyData {
            encoding: "utf-8".to_string(),
            content: content.to_string(),
            size: content.len() as u64,
        }
    }

    fn flow_update() -> FlowUpdate {
        let flow: crate::flow::Flow = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000009","start_time":"2026-05-20T10:00:00Z",
                "end_time":null,
                "network":{"client_ip":"127.0.0.1","client_port":1234,"server_ip":"0.0.0.0",
                           "server_port":0,"protocol":"TCP","tls":false,"tls_version":null,
                           "sni":null},
                "layer":{"type":"Http","data":{"request":{"method":"GET","url":"https://example.com/",
                          "version":"HTTP/1.1","headers":[],"cookies":[],"query":[],"body":null},
                          "response":null,"error":null}},
                "tags":[],"meta":{}}"#,
        )
        .expect("sample flow should parse");
        FlowUpdate::Full(Box::new(flow))
    }

    fn every_variant() -> Vec<FlowUpdate> {
        vec![
            flow_update(),
            FlowUpdate::WebSocketMessage {
                flow_id: "flow-ws".to_string(),
                message: WebSocketMessage {
                    id: uuid::Uuid::new_v4(),
                    timestamp: chrono::Utc::now(),
                    direction: Direction::ServerToClient,
                    content: body("frame"),
                    opcode: "Text".to_string(),
                },
            },
            FlowUpdate::HttpBody {
                flow_id: "flow-body".to_string(),
                direction: Direction::ServerToClient,
                body: body("hello"),
            },
            FlowUpdate::BodyBudgetExceeded {
                flow_id: "flow-budget".to_string(),
                direction: Direction::ClientToServer,
            },
        ]
    }

    /// The lock that stops the two ends drifting: whatever an adapter writes, a client built on
    /// this module reads back as the same update.
    #[test]
    fn every_update_round_trips_through_its_frame() {
        for update in every_variant() {
            let frame = update.to_sse().expect("update should encode");
            let decoded = parse_update(frame.event, &frame.data)
                .unwrap_or_else(|| panic!("frame {} should decode: {}", frame.event, frame.data));

            assert_eq!(
                serde_json::to_value(&decoded).expect("decoded should serialise"),
                serde_json::to_value(&update).expect("original should serialise"),
                "{} did not survive the round trip",
                frame.event
            );
        }
    }

    /// The specific field loss this module exists to prevent: a direction the decoder cannot see
    /// makes "which side sent this body" unanswerable.
    #[test]
    fn http_body_frame_carries_direction_and_body() {
        let frame = FlowUpdate::HttpBody {
            flow_id: "flow-body".to_string(),
            direction: Direction::ServerToClient,
            body: body("upstream payload"),
        }
        .to_sse()
        .expect("update should encode");

        assert_eq!(frame.event, event_name::HTTP_BODY);
        let payload: HttpBodyFrame =
            serde_json::from_str(&frame.data).expect("payload should be a HttpBodyFrame");
        assert_eq!(payload.direction, Direction::ServerToClient);
        assert_eq!(payload.body.content, "upstream payload");
    }

    #[test]
    fn budget_frame_carries_direction() {
        let frame = FlowUpdate::BodyBudgetExceeded {
            flow_id: "flow-budget".to_string(),
            direction: Direction::ClientToServer,
        }
        .to_sse()
        .expect("update should encode");

        let payload: BodyBudgetExceededFrame =
            serde_json::from_str(&frame.data).expect("payload should be a BodyBudgetExceededFrame");
        assert_eq!(payload.direction, Direction::ClientToServer);
    }

    /// `ws-message` was the frame clients could never read: the adapter wrote a bare
    /// `{flow_id, message}` object while the decoder expected a tagged `FlowUpdate`.
    #[test]
    fn ws_message_frame_decodes_into_a_flow_update() {
        let frame = FlowUpdate::WebSocketMessage {
            flow_id: "flow-ws".to_string(),
            message: WebSocketMessage {
                id: uuid::Uuid::new_v4(),
                timestamp: chrono::Utc::now(),
                direction: Direction::ServerToClient,
                content: body("frame"),
                opcode: "Text".to_string(),
            },
        }
        .to_sse()
        .expect("update should encode");

        assert_eq!(frame.event, event_name::WS_MESSAGE);
        let payload: WsMessageFrame =
            serde_json::from_str(&frame.data).expect("payload should be a WsMessageFrame");
        assert_eq!(payload.flow_id, "flow-ws");

        match parse_update(frame.event, &frame.data) {
            Some(FlowUpdate::WebSocketMessage { flow_id, message }) => {
                assert_eq!(flow_id, "flow-ws");
                assert_eq!(message.opcode, "Text");
            }
            other => panic!("expected a decoded ws message, got {other:?}"),
        }
    }

    /// Frames owned by other producers, and garbage, must be skippable rather than fatal.
    #[test]
    fn frames_from_other_producers_are_not_mistaken_for_updates() {
        assert!(parse_update("audit", r#"{"actor":"http"}"#).is_none());
        assert!(parse_update("lifecycle", r#"{"state":"running"}"#).is_none());
        assert!(parse_update("ping", "").is_none());
        assert!(parse_update(event_name::HTTP_BODY, "not json").is_none());
        // A body frame missing its direction is exactly the drift this module removes: reject it
        // instead of inventing a direction.
        assert!(parse_update(event_name::HTTP_BODY, r#"{"flow_id":"f"}"#).is_none());
    }

    fn every_flow_event() -> Vec<FlowEvent> {
        let flow_id = Uuid::new_v4();
        vec![
            FlowEvent::Started {
                flow_id,
                at: chrono::Utc::now(),
            },
            FlowEvent::HeadersReceived {
                flow_id,
                direction: Direction::ClientToServer,
            },
            FlowEvent::BodyChunk {
                flow_id,
                direction: Direction::ServerToClient,
                observed_bytes: 4096,
                truncated: true,
            },
            FlowEvent::MessageReceived {
                flow_id,
                direction: Direction::ServerToClient,
            },
            FlowEvent::MutationApplied {
                flow_id,
                direction: Direction::ClientToServer,
                actor: "rule:rewrite-url".to_string(),
                fields: vec!["request.url".to_string()],
            },
            FlowEvent::InterceptPaused {
                flow_id,
                phase: "request_headers".to_string(),
            },
            FlowEvent::InterceptResolved {
                flow_id,
                phase: "request_headers".to_string(),
                mutated: true,
            },
            FlowEvent::Completed {
                flow_id,
                at: chrono::Utc::now(),
            },
            FlowEvent::Errored {
                flow_id,
                reason: CloseReason::Timeout {
                    kind: "idle".to_string(),
                },
                at: chrono::Utc::now(),
            },
        ]
    }

    /// Lifecycle events travel on their own frame name, so a consumer can dispatch on one field
    /// instead of guessing from which `Flow` fields happen to be populated.
    #[test]
    fn every_flow_event_round_trips_through_its_frame() {
        for event in every_flow_event() {
            let frame = event.to_sse().expect("event should encode");
            assert_eq!(frame.event, event_name::FLOW_EVENT);

            let decoded = parse_flow_event(frame.event, &frame.data)
                .unwrap_or_else(|| panic!("frame should decode: {}", frame.data));

            assert_eq!(
                serde_json::to_value(&decoded).expect("decoded should serialise"),
                serde_json::to_value(&event).expect("original should serialise"),
                "{event:?} did not survive the round trip"
            );
        }
    }

    /// A lifecycle frame must never be mistaken for a snapshot, nor the reverse: they decode through
    /// different entry points, and a consumer routes on the event name alone.
    #[test]
    fn lifecycle_and_snapshot_frames_do_not_decode_as_each_other() {
        let event = FlowEvent::Completed {
            flow_id: Uuid::new_v4(),
            at: chrono::Utc::now(),
        };
        let frame = event.to_sse().expect("event should encode");

        assert!(parse_update(frame.event, &frame.data).is_none());
        assert!(parse_flow_event(event_name::FLOW, "{}").is_none());

        let update = FlowUpdate::BodyBudgetExceeded {
            flow_id: "flow-budget".to_string(),
            direction: Direction::ClientToServer,
        }
        .to_sse()
        .expect("update should encode");
        assert!(parse_flow_event(update.event, &update.data).is_none());
    }

    /// The actor and the fields it changed are the whole point of the mutation event: without them
    /// a consumer can only say "something changed".
    #[test]
    fn mutation_event_names_its_actor_and_fields_on_the_wire() {
        let event = FlowEvent::MutationApplied {
            flow_id: Uuid::new_v4(),
            direction: Direction::ClientToServer,
            actor: "rule:rewrite-url".to_string(),
            fields: vec!["request.url".to_string(), "request.headers".to_string()],
        };
        let frame = event.to_sse().expect("event should encode");

        let decoded = parse_flow_event(frame.event, &frame.data).expect("frame should decode");
        match decoded {
            FlowEvent::MutationApplied { actor, fields, .. } => {
                assert_eq!(actor, "rule:rewrite-url");
                assert_eq!(fields, vec!["request.url", "request.headers"]);
            }
            other => panic!("expected a mutation event, got {other:?}"),
        }
    }
}
