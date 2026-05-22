use crate::rule::engine::executor::ExecutionContext;
use crate::rule::model::BodySource;
use crate::utils::path::PathSanitizer;
use chrono::Utc;
use relay_core_api::flow::{BodyData, Flow, Layer};
use relay_core_api::policy::ProxyPolicy;
use uuid::Uuid;

pub async fn resolve_body_source(
    source: &BodySource,
    policy: Option<&ProxyPolicy>,
) -> Option<BodyData> {
    match source {
        BodySource::Text(t) => Some(BodyData {
            encoding: "utf-8".to_string(),
            content: t.clone(),
            size: t.len() as u64,
        }),
        BodySource::Base64(b) => Some(BodyData {
            encoding: "base64".to_string(),
            content: b.clone(),
            size: b.len() as u64, // Approximate
        }),
        BodySource::File(path) => {
            let root = if let Some(p) = policy
                && let Some(r) = &p.sandbox_root
            {
                r.clone()
            } else {
                tracing::warn!(
                    "MapLocal: sandbox_root is not configured — falling back to CWD. \
                     This will be rejected in a future version. \
                     Set sandbox_root in your ProxyPolicy."
                );
                std::env::current_dir().unwrap_or_default()
            };

            let sanitizer = PathSanitizer::new(root);
            if let Ok(canon_path) = sanitizer.sanitize(path) {
                // Check file size limit
                if let Ok(metadata) = tokio::fs::metadata(&canon_path).await {
                    let max_bytes = policy
                        .map(|p| p.max_local_file_bytes)
                        .unwrap_or(10 * 1024 * 1024);
                    if metadata.len() > max_bytes as u64 {
                        // File too large
                        return None;
                    }
                }

                if let Ok(bytes) = tokio::fs::read(&canon_path).await {
                    // Try to parse as UTF-8
                    if let Ok(text) = String::from_utf8(bytes.clone()) {
                        Some(BodyData {
                            encoding: "utf-8".to_string(),
                            content: text,
                            size: bytes.len() as u64,
                        })
                    } else {
                        // Fallback to Base64
                        use data_encoding::BASE64;
                        Some(BodyData {
                            encoding: "base64".to_string(),
                            content: BASE64.encode(&bytes),
                            size: bytes.len() as u64,
                        })
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}

pub fn substitute_variables(
    template: &str,
    flow: &Flow,
    ctx: &ExecutionContext,
    previous_value: Option<&str>,
) -> String {
    let mut result = template.to_string();

    // Variables from Context
    for (k, v) in &ctx.variables {
        let key = format!("{{{{{}}}}}", k); // {{key}}
        if result.contains(&key) {
            result = result.replace(&key, v);
        }
    }

    // {{previous}}
    if let Some(prev) = previous_value {
        result = result.replace("{{previous}}", prev);
    } else {
        result = result.replace("{{previous}}", "");
    }

    // {{timestamp}} - Unix timestamp (ms)
    if result.contains("{{timestamp}}") {
        result = result.replace("{{timestamp}}", &Utc::now().timestamp_millis().to_string());
    }

    // {{uuid}}
    if result.contains("{{uuid}}") {
        result = result.replace("{{uuid}}", &Uuid::new_v4().to_string());
    }

    // {{client.ip}}
    if result.contains("{{client.ip}}") {
        result = result.replace("{{client.ip}}", &flow.network.client_ip);
    }
    // Backward-compatible alias: {{client_ip}}
    if result.contains("{{client_ip}}") {
        result = result.replace("{{client_ip}}", &flow.network.client_ip);
    }

    // {{server.ip}}
    if result.contains("{{server.ip}}") {
        result = result.replace("{{server.ip}}", &flow.network.server_ip);
    }
    // Backward-compatible alias: {{server_ip}}
    if result.contains("{{server_ip}}") {
        result = result.replace("{{server_ip}}", &flow.network.server_ip);
    }

    // {{server.port}}
    if result.contains("{{server.port}}") {
        result = result.replace("{{server.port}}", &flow.network.server_port.to_string());
    }
    // Backward-compatible alias: {{server_port}}
    if result.contains("{{server_port}}") {
        result = result.replace("{{server_port}}", &flow.network.server_port.to_string());
    }

    // Request variables (HTTP + WebSocket handshake request)
    let request = match &flow.layer {
        Layer::Http(http) => Some(&http.request),
        Layer::WebSocket(ws) => Some(&ws.handshake_request),
        _ => None,
    };
    if let Some(req) = request {
        // {{request.method}}
        if result.contains("{{request.method}}") {
            result = result.replace("{{request.method}}", &req.method);
        }

        // {{request.host}}
        if result.contains("{{request.host}}") {
            if let Some(host) = req.url.host_str() {
                result = result.replace("{{request.host}}", host);
            } else {
                result = result.replace("{{request.host}}", "");
            }
        }

        // {{request.url}}
        if result.contains("{{request.url}}") {
            result = result.replace("{{request.url}}", req.url.as_str());
        }

        // {{request.path}}
        if result.contains("{{request.path}}") {
            result = result.replace("{{request.path}}", req.url.path());
        }

        // {{request.query}}
        if result.contains("{{request.query}}") {
            if let Some(q) = req.url.query() {
                result = result.replace("{{request.query}}", q);
            } else {
                result = result.replace("{{request.query}}", "");
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::{resolve_body_source, substitute_variables};
    use crate::rule::engine::executor::ExecutionContext;
    use crate::rule::engine::state::InMemoryRuleStateStore;
    use crate::rule::model::BodySource;
    use crate::rule::model::RuleTraceSummary;
    use chrono::Utc;
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol, WebSocketLayer,
    };
    use relay_core_api::policy::ProxyPolicy;
    use std::collections::HashMap;
    use std::sync::Arc;
    use url::Url;
    use uuid::Uuid;

    fn sample_flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12345,
                server_ip: "1.1.1.1".to_string(),
                server_port: 443,
                protocol: TransportProtocol::TCP,
                tls: true,
                tls_version: Some("TLS1.3".to_string()),
                sni: Some("example.com".to_string()),
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("https://example.com/path?q=abc").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    fn sample_ctx() -> ExecutionContext {
        ExecutionContext {
            trace: vec![],
            variables: HashMap::new(),
            policy: None,
            summary: RuleTraceSummary::NoMatch,
            state_store: Arc::new(InMemoryRuleStateStore::new()),
            throttle_bytes_per_sec: None,
        }
    }

    fn sample_ws_flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 23456,
                server_ip: "2.2.2.2".to_string(),
                server_port: 443,
                protocol: TransportProtocol::TCP,
                tls: true,
                tls_version: Some("TLS1.3".to_string()),
                sni: Some("ws.example.com".to_string()),
            },
            layer: Layer::WebSocket(WebSocketLayer {
                handshake_request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("wss://ws.example.com/socket?q=1").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                handshake_response: HttpResponse {
                    status: 101,
                    status_text: "Switching Protocols".to_string(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    #[test]
    fn test_substitute_previous_and_request_fields() {
        let flow = sample_flow();
        let mut ctx = sample_ctx();
        ctx.variables.insert("env".to_string(), "dev".to_string());

        let out = substitute_variables(
            "v={{previous}},m={{request.method}},h={{request.host}},p={{request.path}},e={{env}}",
            &flow,
            &ctx,
            Some("old"),
        );

        assert_eq!(out, "v=old,m=GET,h=example.com,p=/path,e=dev");
    }

    #[test]
    fn test_substitute_timestamp_is_unix_millis() {
        let flow = sample_flow();
        let ctx = sample_ctx();
        let out = substitute_variables("ts={{timestamp}}", &flow, &ctx, None);
        let ts = out.strip_prefix("ts=").expect("prefix");
        let millis = ts.parse::<i64>().expect("timestamp millis");
        assert!(millis > 0);
    }

    #[test]
    fn test_substitute_network_legacy_aliases() {
        let flow = sample_flow();
        let ctx = sample_ctx();
        let out = substitute_variables(
            "c={{client_ip}},s={{server_ip}},p={{server_port}}",
            &flow,
            &ctx,
            None,
        );
        assert_eq!(out, "c=127.0.0.1,s=1.1.1.1,p=443");
    }

    #[test]
    fn test_substitute_request_fields_for_websocket_handshake() {
        let flow = sample_ws_flow();
        let ctx = sample_ctx();
        let out = substitute_variables(
            "m={{request.method}},h={{request.host}},p={{request.path}},q={{request.query}}",
            &flow,
            &ctx,
            None,
        );
        assert_eq!(out, "m=GET,h=ws.example.com,p=/socket,q=q=1");
    }

    #[tokio::test]
    async fn test_resolve_body_source_file_too_large_returns_none() {
        let temp_dir = std::env::temp_dir().join(format!("relay-utils-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create dir");
        let file = temp_dir.join("large.txt");
        std::fs::write(&file, vec![b'a'; 33]).expect("write file");

        let policy = ProxyPolicy {
            sandbox_root: Some(temp_dir.clone()),
            max_local_file_bytes: 32,
            ..Default::default()
        };
        let out = resolve_body_source(
            &BodySource::File(file.to_string_lossy().to_string()),
            Some(&policy),
        )
        .await;
        assert!(out.is_none(), "large file should be rejected");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_resolve_body_source_binary_falls_back_to_base64() {
        let temp_dir = std::env::temp_dir().join(format!("relay-utils-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create dir");
        let file = temp_dir.join("bin.dat");
        std::fs::write(&file, vec![0xff, 0xfe, 0xfd, 0x00]).expect("write file");

        let policy = ProxyPolicy {
            sandbox_root: Some(temp_dir.clone()),
            max_local_file_bytes: 1024,
            ..Default::default()
        };
        let out = resolve_body_source(
            &BodySource::File(file.to_string_lossy().to_string()),
            Some(&policy),
        )
        .await
        .expect("binary file should still be loadable");
        assert_eq!(out.encoding, "base64");
        assert_eq!(out.size, 4);
        assert!(
            !out.content.is_empty(),
            "base64 payload should not be empty"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
