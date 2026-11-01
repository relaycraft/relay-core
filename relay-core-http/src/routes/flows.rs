use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, Sse},
    routing::{get, post},
};
use relay_core_api::flow::Layer;
use relay_core_api::har::flow_to_har_entry;
use relay_core_api::modification::FlowQuery;
use relay_core_api::modification::FlowSummary;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/flows/export/har", get(export_har))
        .route("/api/v1/flows", get(search_flows))
        .route("/api/v1/flows/{id}/har", get(get_flow_har))
        .route("/api/v1/flows/{id}/trace", get(get_flow_trace))
        .route("/api/v1/flows/{id}/events", get(replay_flow_events))
        .route("/api/v1/flows/{id}", get(get_flow))
        .route("/api/v1/flows/{id}/replay", post(replay_flow))
        .with_state(ctx)
}

/// Query parameters for GET /api/v1/flows
#[derive(Debug, Deserialize)]
pub struct FlowQueryParams {
    pub host: Option<String>,
    pub path: Option<String>,
    pub path_contains: Option<String>,
    pub method: Option<String>,
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
    pub has_error: Option<bool>,
    pub is_websocket: Option<bool>,
    /// Include the CONNECT instruction flows that opened tunnels. Off unless asked for.
    pub include_connect: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct FlowSearchResponse {
    items: Vec<FlowSummary>,
    returned: usize,
    limit: usize,
    offset: usize,
}

async fn search_flows(
    State(ctx): State<Arc<HttpApiContext>>,
    Query(params): Query<FlowQueryParams>,
) -> Json<FlowSearchResponse> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let offset = params.offset.unwrap_or(0);
    let query = FlowQuery {
        host: params.host,
        path_contains: params.path_contains.or(params.path),
        method: params.method,
        status_min: params.status_min,
        status_max: params.status_max,
        has_error: params.has_error,
        is_websocket: params.is_websocket,
        include_connect: params.include_connect.unwrap_or(false),
        limit: Some(limit),
        offset: Some(offset),
    };
    let items = ctx.flows.search_flows(query).await;
    Json(FlowSearchResponse {
        returned: items.len(),
        items,
        limit,
        offset,
    })
}

/// GET /api/v1/flows/{id}/trace — rule evaluations for one captured flow.
///
/// The trace is diagnostic state for the running process, not part of the stored flow: it answers
/// "why did this rule not fire" for a rule that was evaluated and declined. A missing flow is 404,
/// not an empty trace, so "the flow is gone" and "no rules were evaluated" stay distinguishable.
async fn get_flow_trace(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if ctx.flows.get_flow(&id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let snapshot = ctx.flows.rule_trace(&id);
    Ok(Json(serde_json::json!({
        "flow_id": id,
        "events": snapshot.events,
        "truncated": snapshot.truncated,
    })))
}

/// Query/header input for resuming a recorded event stream.
#[derive(Debug, Deserialize)]
pub struct EventReplayParams {
    /// Event id to resume **after**. The `Last-Event-ID` header wins over this when both are sent,
    /// because that is what a reconnecting `EventSource` sends automatically.
    pub last_event_id: Option<String>,
}

/// GET /api/v1/flows/{id}/events — replay a captured `text/event-stream` in order.
///
/// The events were recorded as they were parsed, so this is a replay of what the client actually
/// received, not a re-parse of the stored bytes. `Last-Event-ID` resumes after the matching event,
/// which is the same contract a live SSE client uses when it reconnects.
async fn replay_flow_events(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
    Query(params): Query<EventReplayParams>,
    headers: HeaderMap,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>,
    StatusCode,
> {
    let flow = ctx.flows.get_flow(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    let relay_core_api::flow::Layer::Http(http) = &flow.layer else {
        return Ok(Sse::new(tokio_stream::iter(Vec::new())));
    };
    let events = http
        .response
        .as_ref()
        .map(|response| response.sse_events.clone())
        .unwrap_or_default();

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or(params.last_event_id);

    let start = last_event_id
        .as_deref()
        .and_then(|wanted| {
            events
                .iter()
                .position(|event| event.id.as_deref() == Some(wanted))
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let frames: Vec<Result<Event, std::convert::Infallible>> = events
        .into_iter()
        .skip(start)
        .map(|recorded| {
            let mut event = Event::default().event(recorded.kind()).data(recorded.data);
            if let Some(id) = recorded.id {
                event = event.id(id);
            }
            if let Some(retry) = recorded.retry {
                event = event.retry(std::time::Duration::from_millis(retry));
            }
            Ok(event)
        })
        .collect();

    Ok(Sse::new(tokio_stream::iter(frames)))
}

async fn get_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match ctx.flows.get_flow_detail(&id).await {
        Some(flow) => Ok(Json(serde_json::to_value(&flow).unwrap_or_default())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Debug, Deserialize)]
pub struct ReplayParams {
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

/// POST /api/v1/flows/{id}/replay
///
/// Re-sends the captured HTTP request through the running proxy and returns the new response.
/// The replay is captured as a new flow. Only HTTP flows are accepted.
///
/// Query parameters:
/// - `accept_invalid_certs` (bool, default false): skip TLS verification on the replay client
async fn replay_flow(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
    Query(params): Query<ReplayParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let flow = ctx
        .flows
        .get_flow(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, format!("Flow {} not found", id)))?;

    let (method, url, headers, body) = match &flow.layer {
        Layer::Http(http) => (
            http.request.method.clone(),
            http.request.url.to_string(),
            http.request.headers.clone(),
            http.request.body.clone(),
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Replay only supports HTTP flows".to_string(),
            ));
        }
    };

    let port = crate::replay::require_running_proxy(&ctx.status.status_snapshot())
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?;
    let ca_pem = ctx.status.ca_cert_pem();
    let response = crate::replay::send_captured_request(
        &method,
        &url,
        &headers,
        body.as_ref(),
        port,
        params.accept_invalid_certs,
        ca_pem.as_deref(),
    )
    .await
    .map_err(|error| {
        let status = if error.starts_with("Invalid method") || error.contains("not valid base64") {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        (status, error)
    })?;

    Ok(Json(serde_json::json!({
        "status": response.status,
        "url": response.url,
        "headers": response.headers,
        "body": response.body,
    })))
}

/// GET /api/v1/flows/{id}/har — export a single flow as HAR entry
async fn get_flow_har(
    State(ctx): State<Arc<HttpApiContext>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let flow = ctx.flows.get_flow(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    let entry = flow_to_har_entry(&flow);
    let har = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "RelayCore", "version": env!("CARGO_PKG_VERSION") },
            "entries": [entry]
        }
    });
    Ok(Json(har))
}

/// GET /api/v1/flows/export/har — batch export flows as HAR log
async fn export_har(
    State(ctx): State<Arc<HttpApiContext>>,
    Query(params): Query<FlowQueryParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let query = FlowQuery {
        host: params.host,
        path_contains: params.path_contains.or(params.path),
        method: params.method,
        status_min: params.status_min,
        status_max: params.status_max,
        has_error: params.has_error,
        is_websocket: params.is_websocket,
        include_connect: params.include_connect.unwrap_or(false),
        limit: Some(limit),
        offset: params.offset,
    };
    let summaries = ctx.flows.search_flows(query).await;
    let mut entries = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        if let Some(flow) = ctx.flows.get_flow(&summary.id).await {
            entries.push(flow_to_har_entry(&flow));
        }
    }

    Json(serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "RelayCore", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::router;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode},
    };
    use relay_core_api::event::FlowEvent;
    use relay_core_api::flow::{Direction, Flow};
    use relay_core_api::rule::{RuleOutcome, RuleStage};
    use relay_core_runtime::CoreState;
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn flow(id: &str) -> Flow {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "start_time": "2026-05-20T10:00:00Z",
            "end_time": null,
            "network": {
                "client_ip": "127.0.0.1", "client_port": 1234,
                "server_ip": "0.0.0.0", "server_port": 0,
                "protocol": "TCP", "tls": false, "tls_version": null, "sni": null
            },
            "layer": { "type": "Http", "data": {
                "request": {
                    "method": "GET", "url": "https://example.com/",
                    "version": "HTTP/1.1", "headers": [], "cookies": [], "query": [], "body": null
                },
                "response": null, "error": null
            }},
            "tags": [], "meta": {}
        }))
        .expect("sample flow")
    }

    #[tokio::test]
    async fn a_captured_event_stream_replays_in_order_and_resumes_from_a_last_event_id() {
        let state = Arc::new(CoreState::new(None).await);
        let id = uuid::Uuid::new_v4().to_string();
        let flow: Flow = serde_json::from_value(serde_json::json!({
            "id": id,
            "start_time": "2026-05-20T10:00:00Z",
            "end_time": "2026-05-20T10:00:01Z",
            "network": {
                "client_ip": "127.0.0.1", "client_port": 1, "server_ip": "0.0.0.0",
                "server_port": 0, "protocol": "TCP", "tls": false,
                "tls_version": null, "sni": null
            },
            "layer": { "type": "Http", "data": {
                "request": { "method": "GET", "url": "https://example.com/events", "version": "HTTP/1.1",
                             "headers": [], "cookies": [], "query": [], "body": null },
                "response": {
                    "status": 200, "status_text": "OK", "version": "HTTP/1.1",
                    "headers": [["content-type", "text/event-stream"]], "cookies": [], "body": null,
                    "trailers": [],
                    "timing": { "time_to_first_byte": null, "time_to_last_byte": null,
                                "connect_time_ms": null, "ssl_time_ms": null },
                    "sse_events": [
                        { "event": "tick", "data": "one", "id": "1" },
                        { "event": "tick", "data": "two", "id": "2" },
                        { "event": "tick", "data": "three", "id": "3" }
                    ]
                },
                "error": null
            }},
            "tags": [], "meta": {}
        }))
        .expect("flow with recorded events");
        state.upsert_flow(Box::new(flow));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let app = router(Arc::new(crate::server::HttpApiContext::new(state)));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{id}/events"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
                .to_vec(),
        )
        .expect("utf-8");
        let one = body.find("data: one").expect("first event");
        let two = body.find("data: two").expect("second event");
        let three = body.find("data: three").expect("third event");
        assert!(
            one < two && two < three,
            "events must replay in order: {body}"
        );
        assert!(body.contains("id: 1"), "ids must be preserved: {body}");

        let resumed = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{id}/events"))
                    .header("last-event-id", "2")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        let body = String::from_utf8(
            to_bytes(resumed.into_body(), usize::MAX)
                .await
                .expect("body")
                .to_vec(),
        )
        .expect("utf-8");
        assert!(
            !body.contains("data: one") && !body.contains("data: two"),
            "got {body}"
        );
        assert!(
            body.contains("data: three"),
            "resume must continue after the id: {body}"
        );
    }

    #[tokio::test]
    async fn a_flow_trace_reports_evaluations_and_404s_for_a_missing_flow() {
        let state = Arc::new(CoreState::new(None).await);
        let id = uuid::Uuid::new_v4().to_string();
        state.upsert_flow(Box::new(flow(&id)));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        state.publish_flow_event(FlowEvent::RuleEvaluated {
            flow_id: id.parse().expect("uuid"),
            direction: Direction::ClientToServer,
            rule_id: "post-only".to_string(),
            stage: RuleStage::RequestHeaders,
            matched: false,
            duration_us: 4,
            outcome: RuleOutcome::NotMatched,
        });

        let ctx = Arc::new(crate::server::HttpApiContext::new(state));
        let app = router(ctx);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{id}/trace"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["flow_id"], id);
        assert_eq!(json["events"][0]["rule_id"], "post-only");
        assert_eq!(json["events"][0]["matched"], false);
        assert_eq!(json["truncated"], false);

        let missing = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/flows/{}/trace", uuid::Uuid::new_v4()))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("request");
        assert_eq!(
            missing.status(),
            StatusCode::NOT_FOUND,
            "a missing flow must not look like an empty trace"
        );
    }
}
