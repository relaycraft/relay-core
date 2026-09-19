//! Proxy lifecycle routes: the control-plane half of decision
//! [`0007`](../../../docs/decisions/0007-daemon-control-plane.md).
//!
//! These endpoints exist so that "start the proxy" and "stop the proxy" are commands with a
//! reportable outcome, instead of a side effect of connecting a client. Failures carry a stable
//! `code` so an agent can branch on the cause instead of parsing prose.

use crate::server::HttpApiContext;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::services::Requester;
use relay_core_runtime::services::{
    ProxyControlError, ProxyControlErrorCode, ProxyStartOutcome, ProxyStartRequest,
    ProxyStopOutcome,
};
use serde::Serialize;
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/proxy/status", get(proxy_status))
        .route("/api/v1/proxy/start", post(proxy_start))
        .route("/api/v1/proxy/stop", post(proxy_stop))
        .with_state(ctx)
}

/// Structured failure body shared by every lifecycle route.
#[derive(Debug, Serialize)]
struct ProxyErrorBody {
    error: ProxyErrorDetail,
}

#[derive(Debug, Serialize)]
struct ProxyErrorDetail {
    code: &'static str,
    message: String,
}

fn error_response(
    status: StatusCode,
    code: &'static str,
    message: String,
) -> axum::response::Response {
    (
        status,
        Json(ProxyErrorBody {
            error: ProxyErrorDetail { code, message },
        }),
    )
        .into_response()
}

/// Map a control error onto a status code.
///
/// `StartFailed` is a conflict rather than a server error: the request is well-formed, but the
/// machine is in a state that forbids it (most often the port is already bound). A client should
/// not retry it unchanged.
fn status_for(error: &ProxyControlError) -> StatusCode {
    match error.code() {
        ProxyControlErrorCode::InvalidConfig => StatusCode::BAD_REQUEST,
        ProxyControlErrorCode::StartFailed => StatusCode::CONFLICT,
        ProxyControlErrorCode::StartTimeout | ProxyControlErrorCode::StopTimeout => {
            StatusCode::GATEWAY_TIMEOUT
        }
        ProxyControlErrorCode::StopFailed => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// GET /api/v1/proxy/status
async fn proxy_status(State(ctx): State<Arc<HttpApiContext>>) -> impl IntoResponse {
    let Some(proxy) = ctx.proxy.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "proxy_control_unavailable",
            "this host does not expose proxy lifecycle control".to_string(),
        );
    };
    Json(proxy.proxy_lifecycle()).into_response()
}

/// Header a client may set to describe itself; recorded in the audit trail.
///
/// Self-reported and never trusted for authorization — the bearer token is the boundary. It exists
/// because "which client started the proxy" is otherwise unanswerable once the caller has exited.
const CLIENT_HEADER: &str = "x-relay-client";

/// The actor and label for a request that arrived over HTTP.
fn requester(headers: &HeaderMap) -> Requester {
    let mut requester = Requester::new(AuditActor::Http);
    if let Some(label) = headers
        .get(CLIENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        requester = requester.label(label);
    }
    requester
}

/// POST /api/v1/proxy/start
async fn proxy_start(
    State(ctx): State<Arc<HttpApiContext>>,
    headers: HeaderMap,
    body: Option<Json<ProxyStartRequest>>,
) -> impl IntoResponse {
    let Some(proxy) = ctx.proxy.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "proxy_control_unavailable",
            "this host does not expose proxy lifecycle control".to_string(),
        );
    };
    let request = body.map(|Json(req)| req).unwrap_or_default();

    match proxy.proxy_start(requester(&headers), request).await {
        Ok(outcome) => {
            // 201 only when this call is what started the proxy; an idempotent repeat is a 200.
            let status = match outcome {
                ProxyStartOutcome::Started { .. } => StatusCode::CREATED,
                ProxyStartOutcome::AlreadyRunning { .. } => StatusCode::OK,
            };
            (status, Json(outcome)).into_response()
        }
        Err(error) => error_response(status_for(&error), error.code().as_str(), error.to_string()),
    }
}

/// POST /api/v1/proxy/stop
async fn proxy_stop(
    State(ctx): State<Arc<HttpApiContext>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(proxy) = ctx.proxy.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "proxy_control_unavailable",
            "this host does not expose proxy lifecycle control".to_string(),
        );
    };

    match proxy.proxy_stop(requester(&headers)).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "outcome": match outcome {
                    ProxyStopOutcome::Stopped => "stopped",
                    ProxyStopOutcome::AlreadyStopped => "already_stopped",
                },
            })),
        )
            .into_response(),
        Err(error) => error_response(status_for(&error), error.code().as_str(), error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use relay_core_runtime::CoreState;
    use relay_core_runtime::RuntimeLifecycle;
    use relay_core_runtime::services::CoreProxyController;
    use serde_json::Value;
    use std::net::TcpListener;
    use tower::ServiceExt;

    fn install_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    async fn context(dir: &std::path::Path) -> Arc<HttpApiContext> {
        install_crypto_provider();
        let state = Arc::new(CoreState::new(None).await);
        let controller = Arc::new(CoreProxyController::new(state.clone()));
        let _ = dir;
        Arc::new(HttpApiContext::new(state).with_proxy_control(controller))
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn ca_request(dir: &std::path::Path, port: u16) -> serde_json::Value {
        serde_json::json!({
            "port": port,
            "ca_cert": dir.join("ca_cert.pem"),
            "ca_key": dir.join("ca_key.pem"),
        })
    }

    async fn call(
        ctx: Arc<HttpApiContext>,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> axum::response::Response {
        call_as(ctx, method, uri, body, None).await
    }

    async fn call_as(
        ctx: Arc<HttpApiContext>,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        client_label: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(label) = client_label {
            builder = builder.header("x-relay-client", label);
        }

        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .expect("request"),
            None => builder.body(Body::empty()).expect("request"),
        };
        router(ctx).oneshot(request).await.expect("response")
    }

    #[tokio::test]
    async fn status_reports_the_lifecycle_before_anything_runs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let response = call(
            context(dir.path()).await,
            "GET",
            "/api/v1/proxy/status",
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["phase"], "created");
        assert_eq!(json["is_active"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn start_then_status_then_stop_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = context(dir.path()).await;
        let port = free_port();

        let started = call(
            ctx.clone(),
            "POST",
            "/api/v1/proxy/start",
            Some(ca_request(dir.path(), port)),
        )
        .await;
        assert_eq!(started.status(), StatusCode::CREATED);
        assert_eq!(body_json(started).await["outcome"], "started");

        let running = call(ctx.clone(), "GET", "/api/v1/proxy/status", None).await;
        let lifecycle: RuntimeLifecycle =
            serde_json::from_value(body_json(running).await).expect("lifecycle");
        assert_eq!(lifecycle.port, Some(port));

        let stopped = call(ctx, "POST", "/api/v1/proxy/stop", None).await;
        assert_eq!(stopped.status(), StatusCode::OK);
        assert_eq!(body_json(stopped).await["outcome"], "stopped");
    }

    /// "Which client started the proxy" must survive the client exiting, so the route records the
    /// self-reported label next to the actor.
    #[tokio::test]
    async fn a_start_records_who_asked_for_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        install_crypto_provider();
        let state = Arc::new(CoreState::new(None).await);
        let controller = Arc::new(CoreProxyController::new(state.clone()));
        let ctx = Arc::new(HttpApiContext::new(state.clone()).with_proxy_control(controller));
        let port = free_port();

        let response = call_as(
            ctx,
            "POST",
            "/api/v1/proxy/start",
            Some(ca_request(dir.path(), port)),
            Some("cli:relay start"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);

        let events = state.recent_audit_events();
        let change = events
            .iter()
            .find(|event| {
                event.kind == relay_core_runtime::audit::AuditEventKind::ProxyLifecycleChanged
            })
            .expect("the lifecycle change must be recorded");

        assert_eq!(
            change.actor,
            relay_core_runtime::audit::AuditActor::Http,
            "the transport is the actor"
        );
        assert_eq!(change.details["requested_by"], "cli:relay start");
        assert_eq!(change.details["change"], "started");

        let stop = call_as(
            Arc::new(
                HttpApiContext::new(state.clone())
                    .with_proxy_control(Arc::new(CoreProxyController::new(state.clone()))),
            ),
            "POST",
            "/api/v1/proxy/stop",
            None,
            None,
        )
        .await;
        assert_eq!(stop.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn starting_twice_is_a_success_not_a_conflict() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = context(dir.path()).await;
        let port = free_port();

        call(
            ctx.clone(),
            "POST",
            "/api/v1/proxy/start",
            Some(ca_request(dir.path(), port)),
        )
        .await;

        let again = call(
            ctx.clone(),
            "POST",
            "/api/v1/proxy/start",
            Some(ca_request(dir.path(), free_port())),
        )
        .await;

        assert_eq!(again.status(), StatusCode::OK);
        let json = body_json(again).await;
        assert_eq!(json["outcome"], "already_running");
        assert_eq!(json["port"], port);

        call(ctx, "POST", "/api/v1/proxy/stop", None).await;
    }

    #[tokio::test]
    async fn a_taken_port_is_reported_as_a_coded_conflict() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = context(dir.path()).await;
        let occupied = TcpListener::bind("127.0.0.1:0").expect("hold port");
        let port = occupied.local_addr().expect("addr").port();

        let response = call(
            ctx,
            "POST",
            "/api/v1/proxy/start",
            Some(ca_request(dir.path(), port)),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "start_failed");
        assert!(
            json["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(&port.to_string())),
            "the message should name the port: {json}"
        );
    }

    #[tokio::test]
    async fn stopping_nothing_reports_already_stopped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let response = call(
            context(dir.path()).await,
            "POST",
            "/api/v1/proxy/stop",
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["outcome"], "already_stopped");
    }

    #[tokio::test]
    async fn a_host_without_proxy_control_says_so() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state));

        let response = call(ctx, "POST", "/api/v1/proxy/start", None).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "proxy_control_unavailable"
        );
    }
}
