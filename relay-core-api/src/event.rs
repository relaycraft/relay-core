//! Typed flow lifecycle events (roadmap §4-4).
//!
//! `FlowUpdate` is a *snapshot* channel: it re-sends the whole `Flow` whenever anything changes, so
//! a consumer cannot tell "headers arrived" from "exchange completed" without diffing successive
//! snapshots, and there is no event at all for a mutation being applied or a breakpoint pausing.
//!
//! The types here describe lifecycle transitions explicitly. They are additive: `FlowUpdate` keeps
//! working for existing adapters, and events can be layered on as hosts adopt them.

use crate::flow::Direction;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Why a flow stopped, distinguished by who ended it.
///
/// Roadmap §4-3: previously the only signals were free-form tag strings (`"error"`, `"ws-error"`,
/// `"ws-ended:<code>"`) plus a boolean `WebSocketLayer.closed`, so nothing could reason about a
/// close cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CloseReason {
    /// The client finished or disconnected first.
    ClientClosed,
    /// The upstream finished or disconnected first.
    UpstreamClosed,
    /// A configured timeout elapsed.
    Timeout {
        /// Which timeout fired, e.g. `request`, `idle`, `handshake`.
        kind: String,
    },
    /// A peer reset the stream (e.g. HTTP/2 RST_STREAM).
    Reset,
    /// A rule, script or policy deliberately dropped the exchange.
    PolicyDrop {
        /// Human-readable cause for audit/UI.
        detail: String,
    },
    /// A protocol parser rejected the bytes.
    ParserError {
        /// Parser diagnostic.
        detail: String,
    },
    /// TLS negotiation or certificate validation failed.
    TlsError {
        /// TLS diagnostic.
        detail: String,
    },
    /// Completed normally.
    Completed,
}

/// A lifecycle event for one flow.
///
/// Deliberately stage-discriminated: each variant states what just happened, so consumers no longer
/// infer it from which fields happen to be populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FlowEvent {
    /// The exchange has begun and request metadata is known.
    Started {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// When the flow started.
        at: DateTime<Utc>,
    },
    /// Response or request headers are available.
    HeadersReceived {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Which direction's headers these are.
        direction: Direction,
    },
    /// A bounded body chunk is available; emitted only when the BodyPlan retains the body.
    BodyChunk {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Which direction this chunk belongs to.
        direction: Direction,
        /// Cumulative bytes observed for this direction so far.
        observed_bytes: u64,
        /// Whether the retention budget was hit, meaning the recorded body is truncated.
        truncated: bool,
    },
    /// A discrete message on a message-oriented transport (WebSocket frame, SSE event, …).
    MessageReceived {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Which direction the message travelled.
        direction: Direction,
    },
    /// A mutation was applied to what will actually go on the wire.
    ///
    /// Roadmap §18: MCP/AI consumers need to explain *what changed and why*, so the actor is part
    /// of the event rather than only appearing in an audit log.
    MutationApplied {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Which direction was modified.
        direction: Direction,
        /// Who caused it, e.g. `rule:<id>`, `script`, `manual_intercept`.
        actor: String,
        /// Names of the fields that changed, for explainability.
        fields: Vec<String>,
    },
    /// A breakpoint paused the exchange.
    InterceptPaused {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Stage key, matching the runtime's intercept phase strings.
        phase: String,
    },
    /// A paused breakpoint was resolved.
    InterceptResolved {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Stage key, matching the runtime's intercept phase strings.
        phase: String,
        /// Whether the resolution modified anything.
        mutated: bool,
    },
    /// The exchange finished; the Flow will not change again.
    Completed {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// When the flow finished.
        at: DateTime<Utc>,
    },
    /// The exchange failed; the Flow will not change again.
    Errored {
        /// Flow this event belongs to.
        flow_id: Uuid,
        /// Classification of the failure.
        reason: CloseReason,
        /// When the failure was observed.
        at: DateTime<Utc>,
    },
}

impl FlowEvent {
    /// The flow this event belongs to.
    pub const fn flow_id(&self) -> Uuid {
        match self {
            FlowEvent::Started { flow_id, .. }
            | FlowEvent::HeadersReceived { flow_id, .. }
            | FlowEvent::BodyChunk { flow_id, .. }
            | FlowEvent::MessageReceived { flow_id, .. }
            | FlowEvent::MutationApplied { flow_id, .. }
            | FlowEvent::InterceptPaused { flow_id, .. }
            | FlowEvent::InterceptResolved { flow_id, .. }
            | FlowEvent::Completed { flow_id, .. }
            | FlowEvent::Errored { flow_id, .. } => *flow_id,
        }
    }

    /// Is this event terminal for its flow?
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            FlowEvent::Completed { .. } | FlowEvent::Errored { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseReason, FlowEvent};
    use crate::flow::Direction;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn close_reason_is_tagged_and_self_describing() {
        let json = serde_json::to_value(CloseReason::Timeout {
            kind: "idle".to_string(),
        })
        .expect("serialize");

        assert_eq!(json["reason"], "timeout");
        assert_eq!(json["kind"], "idle");
    }

    #[test]
    fn events_discriminate_stage_rather_than_relying_on_field_presence() {
        let flow_id = Uuid::new_v4();

        let started = serde_json::to_value(FlowEvent::Started {
            flow_id,
            at: Utc::now(),
        })
        .expect("serialize");
        let completed = serde_json::to_value(FlowEvent::Completed {
            flow_id,
            at: Utc::now(),
        })
        .expect("serialize");

        // Both carry the same fields, yet remain distinguishable.
        assert_eq!(started["event"], "started");
        assert_eq!(completed["event"], "completed");
    }

    #[test]
    fn mutation_events_name_their_actor_and_fields() {
        let event = FlowEvent::MutationApplied {
            flow_id: Uuid::new_v4(),
            direction: Direction::ClientToServer,
            actor: "rule:block-tracking".to_string(),
            fields: vec!["headers".to_string()],
        };

        assert!(!event.is_terminal());
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["actor"], "rule:block-tracking");
        assert_eq!(json["fields"][0], "headers");
    }

    #[test]
    fn terminal_events_are_identified() {
        let flow_id = Uuid::new_v4();
        assert!(
            FlowEvent::Errored {
                flow_id,
                reason: CloseReason::UpstreamClosed,
                at: Utc::now(),
            }
            .is_terminal()
        );
        assert!(
            !FlowEvent::InterceptPaused {
                flow_id,
                phase: "response_body".to_string(),
            }
            .is_terminal()
        );
    }

    #[test]
    fn flow_id_is_available_without_matching_on_variant() {
        let flow_id = Uuid::new_v4();
        let event = FlowEvent::HeadersReceived {
            flow_id,
            direction: Direction::ServerToClient,
        };
        assert_eq!(event.flow_id(), flow_id);
    }
}
