use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{
        HeaderValue, Method, Request, StatusCode,
        header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ORIGIN, WWW_AUTHENTICATE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use relay_core_runtime::CoreState;
#[cfg(feature = "script")]
use relay_core_runtime::services::ScriptService;
use relay_core_runtime::services::{
    AuditService, FlowEventHub, FlowReadService, InterceptService, RuleService,
    RuntimeStatusService,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::routes;

/// Configuration for the HTTP API server.
#[derive(Debug, Clone)]
pub struct HttpApiConfig {
    /// Address to bind (e.g. `127.0.0.1:8082`)
    pub addr: SocketAddr,
    pub bearer_token: Option<String>,
    pub allowed_origins: Vec<HeaderValue>,
}

impl HttpApiConfig {
    pub fn new(port: u16) -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            bearer_token: None,
            allowed_origins: Vec::new(),
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    pub fn with_allowed_origins(mut self, origins: impl IntoIterator<Item = HeaderValue>) -> Self {
        self.allowed_origins = origins.into_iter().collect();
        self
    }
}

/// Shared context for HTTP API handlers, exposing only narrow-capability traits.
/// Constructed from a `CoreState` instance; handlers never import `CoreState` directly.
pub struct HttpApiContext {
    pub flows: Arc<dyn FlowReadService>,
    pub events: Arc<dyn FlowEventHub>,
    pub rules: Arc<dyn RuleService>,
    pub intercepts: Arc<dyn InterceptService>,
    pub audit: Arc<dyn AuditService>,
    pub status: Arc<dyn RuntimeStatusService>,
    #[cfg(feature = "script")]
    pub scripts: Arc<dyn ScriptService>,
}

impl HttpApiContext {
    pub fn new(core: Arc<CoreState>) -> Self {
        Self {
            flows: core.clone(),
            events: core.clone(),
            rules: core.clone(),
            intercepts: core.clone(),
            audit: core.clone(),
            status: core.clone(),
            #[cfg(feature = "script")]
            scripts: core.clone(),
        }
    }
}

/// HTTP API server handle.
pub struct HttpApiServer {
    config: HttpApiConfig,
    state: Arc<CoreState>,
}

impl HttpApiServer {
    pub fn new(config: HttpApiConfig, state: Arc<CoreState>) -> Self {
        Self { config, state }
    }

    /// Start the server; resolves when the server exits or an error occurs.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let ctx = Arc::new(HttpApiContext::new(self.state));
        let config = Arc::new(self.config.clone());
        let listener = tokio::net::TcpListener::bind(config.addr).await?;
        info!("relay-core HTTP API listening on {}", config.addr);

        if !config.addr.ip().is_loopback() && config.bearer_token.is_none() {
            tracing::warn!(
                "HTTP API is listening on {} without a bearer token configured. \
                 This exposes the API to the network without authentication. \
                 Set a bearer token for production use, or use --bind 127.0.0.1 for local-only access.",
                config.addr
            );
        }

        let app = build_router(ctx, config);
        axum::serve(listener, app).await?;
        Ok(())
    }
}

fn build_router(ctx: Arc<HttpApiContext>, config: Arc<HttpApiConfig>) -> Router {
    let router = Router::new()
        .merge(routes::version::router())
        .merge(routes::metrics::router(ctx.clone()))
        .merge(routes::flows::router(ctx.clone()))
        .merge(routes::rules::router(ctx.clone()))
        .merge(routes::intercepts::router(ctx.clone()))
        .merge(routes::events::router(ctx.clone()));

    #[cfg(feature = "script")]
    let router = router.merge(routes::scripts::router(ctx.clone()));

    let router = router
        .route_layer(middleware::from_fn_with_state(
            config.clone(),
            require_bearer_token,
        ))
        .layer(TraceLayer::new_for_http());

    if config.allowed_origins.is_empty() {
        router
    } else {
        router.layer(
            CorsLayer::new()
                .allow_origin(config.allowed_origins.clone())
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([AUTHORIZATION, CONTENT_TYPE, ACCEPT, ORIGIN]),
        )
    }
}

async fn require_bearer_token(
    State(config): State<Arc<HttpApiConfig>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    let Some(expected_token) = config.bearer_token.as_deref() else {
        return next.run(request).await;
    };

    let expected_value = format!("Bearer {}", expected_token);
    let is_authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected_value)
        .unwrap_or(false);

    if is_authorized {
        return next.run(request).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [
            (WWW_AUTHENTICATE, HeaderValue::from_static("Bearer")),
            (CONTENT_TYPE, HeaderValue::from_static("application/json")),
        ],
        serde_json::json!({
            "error": "missing_or_invalid_bearer_token"
        })
        .to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{HttpApiConfig, HttpApiContext, build_router};
    use axum::{
        body::{Body, to_bytes},
        http::{HeaderValue, Method, Request, StatusCode, header::ACCESS_CONTROL_ALLOW_ORIGIN},
    };
    use relay_core_api::flow::Flow;
    use relay_core_api::policy::ProxyPolicy;
    use relay_core_runtime::{CoreState, audit::AuditActor};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::time::{Duration, sleep};
    use tower::ServiceExt;

    fn sample_http_flow(host: &str, path: &str, method: &str, status: u16, ts: i64) -> Flow {
        let flow_id = format!(
            "00000000-0000-0000-0000-{:012}",
            (ts as u64) % 1_000_000_000_000
        );
        let minute = ((ts / 60_000) % 60) as i64;
        let second = ((ts / 1_000) % 60) as i64;
        let millis = (ts % 1_000).abs();
        let start_rfc3339 = format!("2023-11-14T22:{:02}:{:02}.{:03}Z", minute, second, millis);
        serde_json::from_value(json!({
            "id": flow_id,
            "start_time": start_rfc3339,
            "end_time": start_rfc3339,
            "network": {
                "client_ip": "127.0.0.1",
                "client_port": 12000,
                "server_ip": "127.0.0.1",
                "server_port": 8080,
                "protocol": "TCP",
                "tls": false,
                "tls_version": null,
                "sni": null
            },
            "layer": {
                "type": "Http",
                "data": {
                    "request": {
                        "method": method,
                        "url": format!("http://{}{}", host, path),
                        "version": "HTTP/1.1",
                        "headers": [],
                        "cookies": [],
                        "query": [],
                        "body": null
                    },
                    "response": {
                        "status": status,
                        "status_text": "OK",
                        "version": "HTTP/1.1",
                        "headers": [],
                        "cookies": [],
                        "body": null,
                        "timing": {
                            "time_to_first_byte": null,
                            "time_to_last_byte": null
                        }
                    },
                    "error": null
                }
            },
            "tags": []
        }))
        .expect("flow json should deserialize")
    }

    #[tokio::test]
    async fn status_endpoint_is_available_without_auth_by_default() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("body should be valid json");
        assert_eq!(json["phase"], "created");
        assert_eq!(json["running"], false);
        assert!(json.get("started_at_ms").is_none());
    }

    #[tokio::test]
    async fn intercepts_endpoint_uses_shared_snapshot_shape() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/intercepts")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("body should be valid json");
        assert_eq!(json["pending_count"], 0);
        assert_eq!(json["ws_pending_count"], 0);
    }

    #[tokio::test]
    async fn audit_endpoint_uses_shared_snapshot_shape() {
        let state = Arc::new(CoreState::new(None).await);
        state.update_policy_from(
            AuditActor::Http,
            "policy".to_string(),
            ProxyPolicy {
                transparent_enabled: true,
                ..Default::default()
            },
        );
        let _ = state
            .resolve_intercept_with_modifications_from(
                AuditActor::Probe,
                "missing-flow:request".to_string(),
                "drop",
                None,
            )
            .await;
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/audit?actor=http&kind=policy_updated&outcome=success&limit=1")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("body should be valid json");
        assert!(json["events"].is_array());
        assert_eq!(json["events"].as_array().map(|v| v.len()), Some(1));
        assert_eq!(json["events"][0]["actor"], "http");
        assert_eq!(json["events"][0]["kind"], "policy_updated");
        assert_eq!(json["events"][0]["outcome"], "success");
    }

    #[tokio::test]
    async fn prometheus_metrics_endpoint_returns_text_format() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/prometheus")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let text = String::from_utf8(body.to_vec()).expect("prometheus body should be utf-8");
        assert!(text.contains("relay_core_flows_total "));
        assert!(text.contains("relay_core_audit_events_total "));
    }

    #[tokio::test]
    async fn flows_endpoint_returns_pagination_metadata() {
        let state = Arc::new(CoreState::new(None).await);
        let flow_a = sample_http_flow("api.example.com", "/a", "GET", 200, 1_700_000_001_000);
        let flow_b = sample_http_flow("api.example.com", "/b", "POST", 201, 1_700_000_002_000);
        let flow_c = sample_http_flow("api.example.com", "/c", "GET", 500, 1_700_000_003_000);
        let flow_b_id = flow_b.id.to_string();
        state.upsert_flow(Box::new(flow_a));
        state.upsert_flow(Box::new(flow_b));
        state.upsert_flow(Box::new(flow_c));
        sleep(Duration::from_millis(30)).await;

        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/flows?host=api.example.com&limit=1&offset=1")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("body should be valid json");
        assert_eq!(json["returned"], 1);
        assert_eq!(json["limit"], 1);
        assert_eq!(json["offset"], 1);
        assert_eq!(json["items"].as_array().map(|v| v.len()), Some(1));
        assert_eq!(json["items"][0]["id"], flow_b_id);
    }

    #[tokio::test]
    async fn status_endpoint_requires_bearer_token_when_configured() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(
            ctx,
            Arc::new(HttpApiConfig::new(8082).with_bearer_token("secret-token")),
        );

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .header("Authorization", "Bearer secret-token")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cors_is_not_open_by_default() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .header("Origin", "https://example.com")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert!(
            response
                .headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[tokio::test]
    async fn cors_allows_explicit_origin_only() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(
            ctx,
            Arc::new(
                HttpApiConfig::new(8082)
                    .with_allowed_origins([HeaderValue::from_static("https://allowed.example")]),
            ),
        );

        let allowed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .header("Origin", "https://allowed.example")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(
            allowed.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://allowed.example"))
        );

        let denied = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .method(Method::GET)
                    .header("Origin", "https://denied.example")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");
        assert!(denied.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    }
}
