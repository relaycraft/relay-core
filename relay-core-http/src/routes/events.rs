use crate::server::HttpApiContext;
use axum::{
    Router,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use relay_core_api::flow::FlowUpdate;
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
/// Server-Sent Events stream. Each event is one of:
///
/// - `event: flow` — a new or updated flow (full Flow JSON)
/// - `event: ws-message` — a WebSocket message on a flow
/// - `event: http-body` — HTTP body received on a flow
/// - `event: intercept` — a flow was paused at an intercept breakpoint
///
/// Consumers should handle `event: ping` (heartbeat) and reconnect on disconnect.
async fn sse_handler(
    State(ctx): State<Arc<HttpApiContext>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    Sse::new(event_stream(ctx)).keep_alive(KeepAlive::default())
}

fn event_stream(
    ctx: Arc<HttpApiContext>,
) -> impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>> {
    let flow_rx = ctx.events.subscribe_flow_updates();
    let audit_rx = ctx.audit.subscribe_audit_events();
    let lifecycle_rx = ctx.status.subscribe_lifecycle();

    let flow_events = ctx.events.clone();
    let flow_stream = BroadcastStream::new(flow_rx).filter_map(move |res| match res {
        Ok(update) => {
            let update = flow_events.redact_flow_update_for_output(update);
            let event = match &update {
                FlowUpdate::Full(flow) => {
                    let data = serde_json::to_string(flow).unwrap_or_default();
                    Some(Event::default().event("flow").data(data))
                }
                FlowUpdate::WebSocketMessage { flow_id, message } => {
                    let data = serde_json::json!({
                        "flow_id": flow_id,
                        "message": message
                    });
                    Some(Event::default().event("ws-message").data(data.to_string()))
                }
                FlowUpdate::HttpBody { flow_id, .. } => {
                    let data = serde_json::json!({ "flow_id": flow_id });
                    Some(Event::default().event("http-body").data(data.to_string()))
                }
                FlowUpdate::BodyBudgetExceeded { flow_id, .. } => {
                    let data = serde_json::json!({ "flow_id": flow_id });
                    Some(
                        Event::default()
                            .event("body-budget-exceeded")
                            .data(data.to_string()),
                    )
                }
            };
            event.map(Ok)
        }
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            flow_events.record_flow_events_lagged(skipped);
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
    flow_stream.merge(audit_stream).merge(lifecycle_stream)
}

#[cfg(test)]
mod tests {
    use super::event_stream;
    use crate::server::HttpApiContext;
    use relay_core_api::policy::ProxyPolicy;
    use relay_core_runtime::{CoreState, audit::AuditActor};
    use std::{pin::pin, sync::Arc};
    use tokio::time::{Duration, timeout};
    use tokio_stream::StreamExt;

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
}
