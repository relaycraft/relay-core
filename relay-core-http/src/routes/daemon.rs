//! Daemon host routes: operations that act on the host process itself rather than on traffic.
//!
//! Only the shutdown route lives here for now. It exists because "stop the daemon" must be a
//! command a client can issue and get an answer to, rather than a signal the client guesses at
//! from a pid file.

use crate::server::HttpApiContext;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use std::sync::Arc;

pub fn router(ctx: Arc<HttpApiContext>) -> Router {
    Router::new()
        .route("/api/v1/daemon/shutdown", post(shutdown))
        .with_state(ctx)
}

/// POST /api/v1/daemon/shutdown
async fn shutdown(State(ctx): State<Arc<HttpApiContext>>) -> impl IntoResponse {
    let Some(signal) = ctx.shutdown.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "code": "shutdown_unavailable",
                    "message": "this host does not accept shutdown requests",
                }
            })),
        );
    };

    // Answered before the process exits so the caller learns the request was accepted rather
    // than seeing a connection reset and guessing whether it worked.
    signal.notify_one();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "shutting_down" })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::HttpApiContext;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use relay_core_runtime::CoreState;
    use serde_json::Value;
    use tower::ServiceExt;

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/v1/daemon/shutdown")
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn shutdown_notifies_the_host() {
        let state = Arc::new(CoreState::new(None).await);
        let signal = Arc::new(tokio::sync::Notify::new());
        let ctx = Arc::new(HttpApiContext::new(state).with_shutdown_signal(signal.clone()));

        let response = router(ctx).oneshot(request()).await.expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["status"], "shutting_down");

        // The notification is what the host's graceful-shutdown future is waiting on.
        tokio::time::timeout(std::time::Duration::from_secs(1), signal.notified())
            .await
            .expect("host must have been notified");
    }

    #[tokio::test]
    async fn a_host_without_a_shutdown_signal_refuses() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state));

        let response = router(ctx).oneshot(request()).await.expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "shutdown_unavailable"
        );
    }
}
