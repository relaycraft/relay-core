use crate::server::HttpApiContext;
use axum::{
    Router,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use relay_core_api::sse::SseFrame;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, WatchStream};

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/events", get(sse_handler))
        .with_state(ctx)
}

/// GET /api/v1/events
///
/// Server-Sent Events stream. Flow frames are named and shaped by
/// [`relay_core_api::sse`](relay_core_api::sse), which is also what clients decode with:
///
/// - `event: flow` — a new or updated flow (bare Flow JSON)
/// - `event: ws-message` — a WebSocket message, with `flow_id` and `message`
/// - `event: http-body` — an HTTP body, with `flow_id`, `direction` and `body`
/// - `event: body-budget-exceeded` — `flow_id` and the `direction` whose body was too large
/// - `event: audit` / `event: lifecycle` — audit and runtime state changes
///
/// Consumers should handle `event: ping` (heartbeat) and reconnect on disconnect.
async fn sse_handler(
    State(ctx): State<Arc<HttpApiContext>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    Sse::new(event_stream(ctx)).keep_alive(KeepAlive::default())
}

/// Turn one wire frame into the SSE event axum writes out.
///
/// Deliberately inert: all naming and payload shaping lives in `relay_core_api::sse`, so there is no
/// second place for the frame to be got wrong.
fn sse_event(frame: SseFrame) -> Event {
    Event::default().event(frame.event).data(frame.data)
}

fn event_stream(
    ctx: Arc<HttpApiContext>,
) -> impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>> {
    let flow_rx = ctx.events.subscribe_flow_updates();
    let flow_event_rx = ctx.events.subscribe_flow_events();
    let audit_rx = ctx.audit.subscribe_audit_events();
    let lifecycle_rx = ctx.status.subscribe_lifecycle();

    let flow_events = ctx.events.clone();
    let flow_stream = BroadcastStream::new(flow_rx).filter_map(move |res| match res {
        Ok(update) => {
            let update = flow_events.redact_flow_update_for_output(update);
            match update.to_sse() {
                Ok(frame) => Some(Ok(sse_event(frame))),
                Err(error) => {
                    // An empty `data:` field is silently dropped by SSE clients, so dropping the
                    // frame quietly would be indistinguishable from a quiet period.
                    tracing::error!("dropping flow frame that could not be serialised: {error}");
                    None
                }
            }
        }
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            flow_events.record_flow_events_lagged(skipped);
            Some(Ok(Event::default()
                .event("lagged")
                .data("some events were dropped")))
        }
    });

    // Lifecycle transitions (`event: flow-event`) travel alongside snapshots rather than replacing
    // them: a consumer that wants "what just happened" no longer has to diff successive snapshots,
    // and one that only wants current state keeps reading `event: flow`.
    let lifecycle_events = ctx.events.clone();
    let typed_stream = BroadcastStream::new(flow_event_rx).filter_map(move |res| match res {
        Ok(event) => match event.to_sse() {
            Ok(frame) => Some(Ok(sse_event(frame))),
            Err(error) => {
                tracing::error!("dropping flow event that could not be serialised: {error}");
                None
            }
        },
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            lifecycle_events.record_flow_events_lagged(skipped);
            Some(Ok(Event::default()
                .event("lagged")
                .data("some events were dropped")))
        }
    });
    let audit_svc = ctx.audit.clone();
    let audit_stream = BroadcastStream::new(audit_rx).filter_map(move |res| match res {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default().event("audit").data(data)))
        }
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            audit_svc.record_audit_events_lagged(skipped);
            Some(Ok(Event::default()
                .event("audit-lagged")
                .data("some audit events were dropped")))
        }
    });
    let lifecycle_stream = WatchStream::new(lifecycle_rx).map(|lifecycle| {
        let data = serde_json::to_string(&relay_core_runtime::CoreStatusSnapshot::from(lifecycle))
            .unwrap_or_default();
        Ok(Event::default().event("lifecycle").data(data))
    });
    flow_stream
        .merge(typed_stream)
        .merge(audit_stream)
        .merge(lifecycle_stream)
}

#[cfg(test)]
mod tests {
    use super::event_stream;
    use crate::server::HttpApiContext;
    use axum::body::Body;
    use axum::http::Request;
    use relay_core_api::flow::{BodyData, Direction, FlowUpdate};
    use relay_core_api::policy::ProxyPolicy;
    use relay_core_runtime::services::FlowEventHub;
    use relay_core_runtime::{CoreState, audit::AuditActor};
    use std::{pin::pin, sync::Arc};
    use tokio::sync::broadcast;
    use tokio::time::{Duration, timeout};
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    /// A hub whose flow channel the test drives directly, so `/api/v1/events` can be observed for
    /// exactly the updates under test instead of whatever a live proxy happens to produce.
    struct DrivableHub {
        tx: broadcast::Sender<FlowUpdate>,
        event_tx: broadcast::Sender<relay_core_api::event::FlowEvent>,
    }

    impl FlowEventHub for DrivableHub {
        fn subscribe_flow_updates(&self) -> broadcast::Receiver<FlowUpdate> {
            self.tx.subscribe()
        }

        fn subscribe_flow_events(&self) -> broadcast::Receiver<relay_core_api::event::FlowEvent> {
            self.event_tx.subscribe()
        }

        fn redact_flow_update_for_output(&self, update: FlowUpdate) -> FlowUpdate {
            update
        }

        fn record_flow_events_lagged(&self, _skipped: u64) {}
    }

    /// SSE body stream of a live `/api/v1/events` response.
    type Wire = axum::body::BodyDataStream;

    async fn open_stream(tx: &broadcast::Sender<FlowUpdate>) -> Wire {
        let (event_tx, _) = broadcast::channel(16);
        open_stream_with_events(tx, &event_tx).await
    }

    async fn open_stream_with_events(
        tx: &broadcast::Sender<FlowUpdate>,
        event_tx: &broadcast::Sender<relay_core_api::event::FlowEvent>,
    ) -> Wire {
        let state = Arc::new(CoreState::new(None).await);
        let mut ctx = HttpApiContext::new(state);
        ctx.events = Arc::new(DrivableHub {
            tx: tx.clone(),
            event_tx: event_tx.clone(),
        });

        let response = super::router(Arc::new(ctx))
            .oneshot(
                Request::builder()
                    .uri("/api/v1/events")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("the events route should answer");

        assert_eq!(response.status(), 200, "events route should be served");
        response.into_body().into_data_stream()
    }

    /// Read bytes off the response until one complete SSE frame (terminated by a blank line) has
    /// been observed. This reads the *rendered wire text*, so it also catches a frame that
    /// serialises differently from what was requested.
    async fn next_frame(stream: &mut Wire) -> String {
        let mut buffered = Vec::new();
        loop {
            let chunk = timeout(Duration::from_millis(500), stream.next())
                .await
                .expect("an SSE frame should arrive in time")
                .expect("the stream should still be open")
                .expect("SSE chunks are infallible");
            buffered.extend_from_slice(&chunk);
            if buffered.windows(2).any(|pair| pair == b"\n\n") {
                return String::from_utf8(buffered).expect("SSE frames are UTF-8");
            }
        }
    }

    /// `event: http-body` is documented as "HTTP body received on a flow". A frame carrying only
    /// the flow id cannot tell a consumer *which* direction arrived, nor what it contains — the
    /// body had already been dropped by the time SSE serialised it, so a UI could only show a
    /// streamed body by polling the flow list again.
    #[tokio::test]
    async fn sse_http_body_event_carries_direction_and_body() {
        let (tx, _) = broadcast::channel(16);
        let mut stream = open_stream(&tx).await;

        // Drain the initial lifecycle frame so the next frame is the one under test.
        let _ = next_frame(&mut stream).await;

        tx.send(FlowUpdate::HttpBody {
            flow_id: "flow-1".to_string(),
            direction: Direction::ServerToClient,
            body: BodyData {
                encoding: "utf-8".to_string(),
                content: "hello from upstream".to_string(),
                size: 19,
            },
        })
        .expect("broadcast should have a receiver");

        let frame = next_frame(&mut stream).await;

        assert!(frame.contains("event: http-body"), "frame was {frame:?}");
        assert!(
            frame.contains("ServerToClient"),
            "direction must survive SSE serialisation, frame was {frame:?}"
        );
        assert!(
            frame.contains("hello from upstream"),
            "body must survive SSE serialisation, frame was {frame:?}"
        );
    }

    /// The same drift applies to the budget notification: "this direction's body was too large to
    /// inspect" is not actionable without the direction.
    #[tokio::test]
    async fn sse_body_budget_event_carries_direction() {
        let (tx, _) = broadcast::channel(16);
        let mut stream = open_stream(&tx).await;

        let _ = next_frame(&mut stream).await;

        tx.send(FlowUpdate::BodyBudgetExceeded {
            flow_id: "flow-2".to_string(),
            direction: Direction::ClientToServer,
        })
        .expect("broadcast should have a receiver");

        let frame = next_frame(&mut stream).await;

        assert!(
            frame.contains("event: body-budget-exceeded"),
            "frame was {frame:?}"
        );
        assert!(
            frame.contains("ClientToServer"),
            "direction must survive SSE serialisation, frame was {frame:?}"
        );
    }

    #[tokio::test]
    async fn sse_stream_emits_event_after_policy_audit_update() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let mut stream = pin!(event_stream(ctx));

        let first = timeout(Duration::from_millis(300), stream.next())
            .await
            .expect("initial event should arrive in time");
        assert!(
            first.is_some(),
            "stream should emit initial lifecycle event"
        );

        state.update_policy_from(
            AuditActor::Http,
            "policy".to_string(),
            ProxyPolicy::default(),
        );

        let second = timeout(Duration::from_millis(300), stream.next())
            .await
            .expect("audit event should arrive in time");
        assert!(
            second.is_some(),
            "stream should emit audit event after policy update"
        );
    }

    /// Typed lifecycle transitions must reach consumers on their own frame, so a UI or agent can
    /// react to "this exchange is paused" without diffing snapshots. `event: intercept` was
    /// documented for exactly this and never emitted.
    #[tokio::test]
    async fn sse_flow_event_frame_carries_the_typed_transition() {
        let (tx, _) = broadcast::channel(16);
        let (event_tx, _) = broadcast::channel(16);
        let mut stream = open_stream_with_events(&tx, &event_tx).await;

        let _ = next_frame(&mut stream).await;

        let flow_id = uuid::Uuid::new_v4();
        event_tx
            .send(relay_core_api::event::FlowEvent::InterceptPaused {
                flow_id,
                phase: "response_body".to_string(),
            })
            .expect("broadcast should have a receiver");

        let frame = next_frame(&mut stream).await;

        assert!(frame.contains("event: flow-event"), "frame was {frame:?}");
        assert!(
            frame.contains("intercept_paused"),
            "the transition kind must survive serialisation, frame was {frame:?}"
        );
        assert!(
            frame.contains(&flow_id.to_string()),
            "the flow must be identifiable, frame was {frame:?}"
        );
        assert!(
            frame.contains("response_body"),
            "the phase must survive serialisation, frame was {frame:?}"
        );
    }

    /// A mutation frame has to name its actor and fields; that is the whole reason it exists.
    #[tokio::test]
    async fn sse_mutation_frame_names_actor_and_fields() {
        let (tx, _) = broadcast::channel(16);
        let (event_tx, _) = broadcast::channel(16);
        let mut stream = open_stream_with_events(&tx, &event_tx).await;

        let _ = next_frame(&mut stream).await;

        event_tx
            .send(relay_core_api::event::FlowEvent::MutationApplied {
                flow_id: uuid::Uuid::new_v4(),
                direction: Direction::ClientToServer,
                actor: "rule:rewrite-url".to_string(),
                fields: vec!["request.url".to_string()],
            })
            .expect("broadcast should have a receiver");

        let frame = next_frame(&mut stream).await;

        assert!(frame.contains("mutation_applied"), "frame was {frame:?}");
        assert!(frame.contains("rule:rewrite-url"), "frame was {frame:?}");
        assert!(frame.contains("request.url"), "frame was {frame:?}");
    }
}
