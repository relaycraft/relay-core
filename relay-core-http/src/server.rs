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
    AuditService, FlowEventHub, FlowReadService, InterceptService, PolicyService,
    ProxyControlService, RuleService, RuntimeStatusService,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::routes;
#[cfg(feature = "webui")]
use crate::webui::serve_webui;

/// Configuration for the HTTP API server.
#[derive(Debug, Clone)]
pub struct HttpApiConfig {
    /// Address to bind (e.g. `127.0.0.1:8082`)
    pub addr: SocketAddr,
    pub bearer_token: Option<String>,
    pub allowed_origins: Vec<HeaderValue>,
    /// Serve embedded Web UI static files on non-API routes
    pub serve_webui: bool,
}

impl HttpApiConfig {
    pub fn new(port: u16) -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            bearer_token: None,
            allowed_origins: Vec::new(),
            serve_webui: false,
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

    pub fn with_webui(mut self, serve: bool) -> Self {
        self.serve_webui = serve;
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
    pub policy: Arc<dyn PolicyService>,
    pub status: Arc<dyn RuntimeStatusService>,
    /// Proxy lifecycle control. `None` for hosts that serve the API without owning a proxy
    /// lifecycle; those answer the lifecycle routes with `proxy_control_unavailable` instead of
    /// pretending to have started something.
    pub proxy: Option<Arc<dyn ProxyControlService>>,
    /// Notified when a caller asks the host to exit (daemon shutdown).
    pub shutdown: Option<Arc<tokio::sync::Notify>>,
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
            policy: core.clone(),
            status: core.clone(),
            proxy: None,
            shutdown: None,
            #[cfg(feature = "script")]
            scripts: core.clone(),
        }
    }

    /// Enable the proxy lifecycle routes for this context.
    pub fn with_proxy_control(mut self, proxy: Arc<dyn ProxyControlService>) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Enable the daemon shutdown route for this context.
    pub fn with_shutdown_signal(mut self, shutdown: Arc<tokio::sync::Notify>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }
}

/// HTTP API server handle.
pub struct HttpApiServer {
    config: HttpApiConfig,
    state: Arc<CoreState>,
    proxy: Option<Arc<dyn ProxyControlService>>,
    shutdown: Option<Arc<tokio::sync::Notify>>,
    /// Pre-bound listener from [`HttpApiServer::bind`].
    listener: Option<tokio::net::TcpListener>,
}

impl HttpApiServer {
    pub fn new(config: HttpApiConfig, state: Arc<CoreState>) -> Self {
        Self {
            config,
            state,
            proxy: None,
            shutdown: None,
            listener: None,
        }
    }

    /// Serve the proxy lifecycle routes. Without this the routes answer `proxy_control_unavailable`.
    pub fn with_proxy_control(mut self, proxy: Arc<dyn ProxyControlService>) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Exit gracefully when `POST /api/v1/daemon/shutdown` is called.
    pub fn with_shutdown_signal(mut self, shutdown: Arc<tokio::sync::Notify>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// Bind now and serve later, so the caller can learn the *actual* port before serving.
    ///
    /// A daemon needs this: it publishes the port in its manifest, and a client that reads a
    /// guessed port instead of the bound one would attach to whatever else happens to hold it.
    /// Binding to port 0 (or falling back to it) makes the real port knowable only this way.
    pub async fn bind(self) -> std::io::Result<(Self, SocketAddr)> {
        let listener = tokio::net::TcpListener::bind(self.config.addr).await?;
        let addr = listener.local_addr()?;
        Ok((
            Self {
                listener: Some(listener),
                ..self
            },
            addr,
        ))
    }

    /// The address this server will serve on, if it was pre-bound.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
    }

    /// Start the server; resolves when the server exits or an error occurs.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let mut ctx = HttpApiContext::new(self.state);
        ctx.proxy = self.proxy;
        ctx.shutdown = self.shutdown.clone();
        let ctx = Arc::new(ctx);
        let config = Arc::new(self.config.clone());
        let listener = match self.listener {
            Some(listener) => listener,
            None => tokio::net::TcpListener::bind(config.addr).await?,
        };
        let bound = listener.local_addr().unwrap_or(config.addr);
        info!("relay-core HTTP API listening on {}", bound);

        if !config.addr.ip().is_loopback() && config.bearer_token.is_none() {
            tracing::warn!(
                "HTTP API is listening on {} without a bearer token configured. \
                 This exposes the API to the network without authentication. \
                 Set a bearer token for production use, or use --bind 127.0.0.1 for local-only access.",
                config.addr
            );
        }

        let app = build_router(ctx, config);
        match self.shutdown {
            Some(shutdown) => {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move { shutdown.notified().await })
                    .await?;
            }
            None => axum::serve(listener, app).await?,
        }
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
        .merge(routes::events::router(ctx.clone()))
        .merge(routes::policy::router(ctx.clone()))
        .merge(routes::proxy::router(ctx.clone()))
        .merge(routes::daemon::router(ctx.clone()));

    #[cfg(feature = "script")]
    let router = router.merge(routes::scripts::router(ctx.clone()));

    let router = router
        .route_layer(middleware::from_fn_with_state(
            config.clone(),
            require_bearer_token,
        ))
        .layer(TraceLayer::new_for_http());

    let router = if config.allowed_origins.is_empty() {
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
    };

    #[cfg(feature = "webui")]
    if config.serve_webui {
        return router.fallback_service(serve_webui());
    }

    router
}

/// Name of the cookie the Web UI uses to authenticate.
///
/// A browser cannot attach an `Authorization` header to an `EventSource`, so the Web UI cannot use
/// the bearer token for its live stream. It gets the token from the URL fragment the daemon prints
/// (fragments are never sent to a server, so the token stays out of request logs), stores it in this
/// cookie, and the cookie rides along on both `fetch` and `EventSource`.
pub const AUTH_COOKIE_NAME: &str = "relay_core_token";

/// Is this request carrying the token, as a bearer header or as the Web UI cookie?
fn is_authorized(headers: &axum::http::HeaderMap, expected_token: &str) -> bool {
    let bearer_ok = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {expected_token}"));

    if bearer_ok {
        return true;
    }

    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|cookies| cookie_value(cookies, AUTH_COOKIE_NAME) == Some(expected_token))
}

/// Read one cookie out of a `Cookie` header.
fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim())
    })
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

    if is_authorized(request.headers(), expected_token) {
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
        let minute = (ts / 60_000) % 60;
        let second = (ts / 1_000) % 60;
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
        assert!(text.contains("relay_core_proxy_bytes_sent_total "));
        assert!(text.contains("relay_core_proxy_bytes_recv_total "));
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

    /// A browser cannot set headers on an `EventSource`, so the Web UI authenticates with a cookie
    /// that the same middleware accepts.
    #[test]
    fn the_webui_cookie_authorizes_a_request() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("theme=dark; relay_core_token=s3cret; other=1"),
        );

        assert!(super::is_authorized(&headers, "s3cret"));
        assert!(
            !super::is_authorized(&headers, "different"),
            "a cookie that is merely present is not authorization"
        );
    }

    #[test]
    fn a_bearer_header_still_authorizes_a_request() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer s3cret"),
        );

        assert!(super::is_authorized(&headers, "s3cret"));
        assert!(
            !super::is_authorized(&axum::http::HeaderMap::new(), "s3cret"),
            "no credential is not authorization"
        );
    }

    #[test]
    fn cookie_values_are_read_by_exact_name() {
        assert_eq!(
            super::cookie_value("relay_core_token=a b; x=1", "relay_core_token"),
            Some("a b")
        );
        assert_eq!(
            super::cookie_value("relay_core_token_extra=1", "relay_core_token"),
            None,
            "a name that merely starts the same is a different cookie"
        );
        assert_eq!(super::cookie_value("", "relay_core_token"), None);
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

    #[tokio::test]
    async fn get_intercepts_returns_pending_items() {
        use relay_core_runtime::audit::AuditActor;
        use tokio::sync::oneshot;

        let state = Arc::new(CoreState::new(None).await);
        let flow_id = "00000000-0000-0000-0000-000000000001".to_string();
        let key = format!("{}:request_headers", flow_id);
        let (tx, _rx) = oneshot::channel();
        state.register_intercept(key.clone(), tx).await;

        let ctx = Arc::new(HttpApiContext::new(state.clone()));
        let app = build_router(ctx, Arc::new(HttpApiConfig::new(8082)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/intercepts")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(json["pending_count"], 1);
        assert_eq!(json["items"].as_array().expect("items array").len(), 1);
        assert_eq!(json["items"][0]["key"], key);

        state
            .resolve_intercept_with_modifications_from(AuditActor::Http, key, "continue", None)
            .await
            .expect("intercept should resolve");
    }

    #[cfg(feature = "webui")]
    #[tokio::test]
    async fn webui_fallback_serves_index_at_root() {
        use tower::ServiceExt;

        let core = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(HttpApiContext::new(core));
        let config = Arc::new(HttpApiConfig::new(8082).with_webui(true));
        let app = build_router(ctx, config);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        assert!(body.len() > 100, "fallback body empty, len={}", body.len());
    }
}
