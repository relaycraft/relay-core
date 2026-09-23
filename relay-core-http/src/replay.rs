//! Replay a captured request by sending it back through the proxy this process is running.
//!
//! The client ignores `HTTP_PROXY` and the system proxy. A replay that dialled the origin itself
//! would never show up in the flow list, and one that followed the process environment would land
//! in whatever proxy happened to be configured there.

use base64::Engine;
use relay_core_api::flow::BodyData;
use relay_core_runtime::{CoreStatusSnapshot, RuntimeLifecyclePhase};
use std::time::Duration;

/// Answer of one replayed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayResponse {
    pub status: u16,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Port of a proxy that is actually listening. Anything else cannot capture the replay.
pub fn require_running_proxy(snapshot: &CoreStatusSnapshot) -> Result<u16, String> {
    match (&snapshot.phase, snapshot.port) {
        (RuntimeLifecyclePhase::Running, Some(port)) => Ok(port),
        _ => Err(
            "the proxy is not running, so a replay cannot be captured. Start it and retry."
                .to_string(),
        ),
    }
}

/// Send the captured request through `http://127.0.0.1:{proxy_port}`.
///
/// `ca_pem` is added as a trust anchor so an HTTPS replay can accept the certificate this proxy
/// forges. `accept_invalid_certs` still skips verification on the replay client.
pub async fn send_captured_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&BodyData>,
    proxy_port: u16,
    accept_invalid_certs: bool,
    ca_pem: Option<&str>,
) -> Result<ReplayResponse, String> {
    let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{proxy_port}"))
        .map_err(|error| error.to_string())?;
    let mut builder = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(30));
    if accept_invalid_certs {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(pem) = ca_pem.filter(|pem| !pem.is_empty()) {
        match reqwest::Certificate::from_pem(pem.as_bytes()) {
            Ok(cert) => builder = builder.add_root_certificate(cert),
            Err(error) => tracing::warn!("replay client could not load the proxy CA: {error}"),
        }
    }
    let client = builder.build().map_err(|error| error.to_string())?;

    let method = method
        .parse::<reqwest::Method>()
        .map_err(|error| format!("Invalid method: {error}"))?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        if is_replay_header(name) {
            request = request.header(name, value);
        }
    }
    if let Some(body) = body {
        request = request.body(body_bytes(body)?);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Replay request failed: {error}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response.text().await.map_err(|error| error.to_string())?;
    Ok(ReplayResponse {
        status,
        url: url.to_string(),
        headers,
        body,
    })
}

/// Headers the replay client must set itself. Copying them from the capture fights the new body
/// and the proxy hop.
fn is_replay_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "proxy-connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "content-length"
    )
}

fn body_bytes(body: &BodyData) -> Result<Vec<u8>, String> {
    if body.encoding.eq_ignore_ascii_case("base64") {
        base64::engine::general_purpose::STANDARD
            .decode(body.content.as_bytes())
            .map_err(|error| format!("replay body is not valid base64: {error}"))
    } else {
        Ok(body.content.as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::require_running_proxy;
    use crate::routes::flows::router;
    use crate::server::HttpApiContext;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use relay_core_api::modification::FlowQuery;
    use relay_core_runtime::audit::AuditActor;
    use relay_core_runtime::services::{
        CoreProxyController, ProxyControlService, ProxyStartRequest, Requester,
    };
    use relay_core_runtime::{CoreState, CoreStatusSnapshot, RuntimeLifecyclePhase};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    fn install_crypto() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn a_stopped_proxy_cannot_replay() {
        let snapshot = CoreStatusSnapshot {
            phase: RuntimeLifecyclePhase::Stopped,
            running: false,
            port: Some(8080),
            uptime: None,
            last_error: None,
        };
        let error = require_running_proxy(&snapshot).unwrap_err();
        assert!(error.contains("not running"));
    }

    #[tokio::test]
    async fn a_replay_is_captured_as_a_new_flow() {
        install_crypto();
        let dir = tempfile::tempdir().expect("temp dir");
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = origin.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf).await;
                    let body = "from-origin";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        drop(proxy_listener);

        let state = Arc::new(CoreState::new(None).await);
        let controller = Arc::new(CoreProxyController::new(state.clone()));
        controller
            .proxy_start(
                Requester::new(AuditActor::Http),
                ProxyStartRequest {
                    port: proxy_port,
                    ca_cert: Some(dir.path().join("ca_cert.pem")),
                    ca_key: Some(dir.path().join("ca_key.pem")),
                    ..ProxyStartRequest::default()
                },
            )
            .await
            .expect("proxy start");
        let ctx = Arc::new(HttpApiContext::new(state).with_proxy_control(controller.clone()));
        let pem = ctx
            .status
            .ca_cert_pem()
            .expect("running proxy publishes its CA");
        assert!(pem.contains("BEGIN CERTIFICATE"));

        let client = reqwest::Client::builder()
            .proxy(
                reqwest::Proxy::all(format!("http://127.0.0.1:{proxy_port}")).expect("proxy url"),
            )
            .build()
            .unwrap();
        let url = format!("http://{origin_addr}/only-replay");
        let seeded = client.get(&url).send().await.unwrap().text().await.unwrap();
        assert_eq!(seeded, "from-origin");

        let first = wait_for_count(&ctx, 1).await;
        assert_eq!(first.len(), 1, "the seeding request was not captured");

        let response = router(ctx.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/flows/{}/replay", first[0].id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 200);
        assert_eq!(json["body"], "from-origin");

        let captured = wait_for_count(&ctx, 2).await;
        assert_eq!(
            captured.len(),
            2,
            "replay did not come back through the proxy: {captured:?}"
        );

        controller
            .proxy_stop(Requester::new(AuditActor::Http))
            .await
            .expect("proxy stop");
        let refused = router(ctx)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/flows/{}/replay", first[0].id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    async fn wait_for_count(
        ctx: &HttpApiContext,
        count: usize,
    ) -> Vec<relay_core_api::modification::FlowSummary> {
        let mut last = Vec::new();
        for _ in 0..40 {
            last = ctx
                .flows
                .search_flows(FlowQuery {
                    path_contains: Some("/only-replay".to_string()),
                    limit: Some(20),
                    ..FlowQuery::default()
                })
                .await;
            if last.len() >= count {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        last
    }
}
