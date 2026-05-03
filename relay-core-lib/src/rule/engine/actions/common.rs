use crate::rule::model::{Action, TerminalReason};
use crate::rule::engine::executor::ExecutionContext;
use crate::rule::engine::actions::utils::substitute_variables;
use crate::rule::engine::actions::ActionOutcome;
use relay_core_api::flow::{Flow, Layer};
use url::{Host, Url};

fn infer_throttle_bytes(flow: &Flow) -> u64 {
    match &flow.layer {
        Layer::Http(http) => {
            let mut total = 0u64;
            if let Some(body) = &http.request.body {
                total = total.saturating_add(body.size);
            }
            if let Some(response) = &http.response
                && let Some(body) = &response.body {
                    total = total.saturating_add(body.size);
                }
            total
        }
        Layer::WebSocket(ws) => {
            ws.handshake_request
                .body
                .as_ref()
                .map(|b| b.size)
                .unwrap_or(0)
                .saturating_add(ws.handshake_response.body.as_ref().map(|b| b.size).unwrap_or(0))
        }
        _ => 0,
    }
}

fn authority_for_host_header(url: &Url) -> Option<String> {
    let host = url.host()?;
    let host_part = match host {
        Host::Domain(d) => d.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{}]", ip),
    };
    let authority = match url.port() {
        Some(port) => format!("{}:{}", host_part, port),
        None => host_part,
    };
    Some(authority)
}

fn upsert_host_header(headers: &mut Vec<(String, String)>, authority: &str) {
    let mut out = Vec::with_capacity(headers.len() + 1);
    let mut inserted = false;
    for (k, v) in headers.drain(..) {
        if k.eq_ignore_ascii_case("host") {
            if !inserted {
                out.push(("Host".to_string(), authority.to_string()));
                inserted = true;
            }
        } else {
            out.push((k, v));
        }
    }
    if !inserted {
        out.push(("Host".to_string(), authority.to_string()));
    }
    *headers = out;
}

fn remap_url_origin(original: &Url, target_origin: &Url) -> Result<Url, String> {
    let mut mapped = original.clone();
    mapped
        .set_scheme(target_origin.scheme())
        .map_err(|_| "MapRemote failed to set scheme".to_string())?;
    mapped
        .set_host(target_origin.host_str())
        .map_err(|_| "MapRemote failed to set host".to_string())?;
    mapped
        .set_port(target_origin.port())
        .map_err(|_| "MapRemote failed to set port".to_string())?;
    Ok(mapped)
}

fn parse_map_remote_target(substituted: &str) -> Result<Url, String> {
    if !substituted.contains("://") {
        return Err("MapRemote url must be absolute with scheme://host".to_string());
    }
    let u = Url::parse(substituted).map_err(|e| format!("MapRemote invalid url: {}", e))?;
    if u.host_str().is_none() {
        return Err("MapRemote url must include host".to_string());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("MapRemote url must not include userinfo".to_string());
    }
    match u.scheme() {
        "http" | "https" | "ws" | "wss" => Ok(u),
        other => Err(format!(
            "MapRemote unsupported scheme: {} (allowed: http, https, ws, wss)",
            other
        )),
    }
}

pub async fn execute(
    action: &Action,
    flow: &mut Flow,
    ctx: &mut ExecutionContext,
) -> ActionOutcome {
    match action {
        Action::Drop => ActionOutcome::Terminated(TerminalReason::Drop),
        Action::Abort => ActionOutcome::Terminated(TerminalReason::Abort),
        Action::Inspect => ActionOutcome::Terminated(TerminalReason::Inspect),
        Action::Delay { ms } => {
            tokio::time::sleep(tokio::time::Duration::from_millis(*ms)).await;
            ActionOutcome::Continue
        }
        Action::SetVariable { name, value } => {
            let val = substitute_variables(value, flow, ctx, None);
            ctx.variables.insert(name.clone(), val);
            ActionOutcome::Continue
        }
        Action::Tag { key, value } => {
            let val = substitute_variables(value, flow, ctx, None);
            flow.tags.push(format!("{}:{}", key, val));
            ActionOutcome::Continue
        }
        Action::RateLimit { key, limit, window_ms } => {
            let key = substitute_variables(key, flow, ctx, None);
            let count = ctx
                .state_store
                .increment_counter(&key, std::time::Duration::from_millis(*window_ms))
                .await;
            if count > *limit as u64 {
                ActionOutcome::Terminated(TerminalReason::RateLimited)
            } else {
                ActionOutcome::Continue
            }
        }
        Action::RedirectIp { .. } | Action::SetTtl { .. } | Action::ForwardPort { .. } => {
            ActionOutcome::Failed("L3/L4 actions not implemented yet".to_string())
        }
        Action::MapRemote { url, preserve_host } => {
            let substituted = substitute_variables(url, flow, ctx, None);
            let target_origin = match parse_map_remote_target(&substituted) {
                Ok(u) => u,
                Err(e) => return ActionOutcome::Failed(e),
            };

            match &mut flow.layer {
                Layer::Http(http) => {
                    let mapped = match remap_url_origin(&http.request.url, &target_origin) {
                        Ok(u) => u,
                        Err(e) => return ActionOutcome::Failed(e),
                    };
                    http.request.url = mapped;
                    if !*preserve_host
                        && let Some(authority) = authority_for_host_header(&http.request.url) {
                            upsert_host_header(&mut http.request.headers, &authority);
                        }
                    ActionOutcome::Continue
                }
                Layer::WebSocket(ws) => {
                    let mapped = match remap_url_origin(&ws.handshake_request.url, &target_origin) {
                        Ok(u) => u,
                        Err(e) => return ActionOutcome::Failed(e),
                    };
                    ws.handshake_request.url = mapped;
                    if !*preserve_host
                        && let Some(authority) = authority_for_host_header(&ws.handshake_request.url) {
                            upsert_host_header(&mut ws.handshake_request.headers, &authority);
                        }
                    ActionOutcome::Continue
                }
                _ => ActionOutcome::Failed("MapRemote only supports HTTP/WebSocket flows".to_string()),
            }
        }
        Action::Throttle { kbps } => {
            if *kbps == 0 {
                return ActionOutcome::Failed("Throttle kbps must be > 0".to_string());
            }

            // Conservative implementation: emulate bandwidth throttle via delay
            // derived from known payload size in current flow snapshot.
            let bytes = infer_throttle_bytes(flow);
            if bytes == 0 {
                return ActionOutcome::Continue;
            }

            // delay_ms = ceil((bytes * 8) / kbps), where kbps is kilobits per second.
            let bits = (bytes as u128).saturating_mul(8);
            let kbps_u128 = *kbps as u128;
            let delay_ms = bits
                .saturating_add(kbps_u128.saturating_sub(1))
                .checked_div(kbps_u128)
                .unwrap_or(0)
                .min(u64::MAX as u128) as u64;

            if delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
            ActionOutcome::Continue
        }
        _ => ActionOutcome::Failed(format!("Action {:?} not supported in common handler", action)),
    }
}

#[cfg(test)]
mod tests {
    use super::execute;
    use crate::rule::engine::actions::ActionOutcome;
    use crate::rule::engine::executor::ExecutionContext;
    use crate::rule::engine::state::InMemoryRuleStateStore;
    use crate::rule::model::{Action, RuleTraceSummary};
    use chrono::Utc;
    use relay_core_api::flow::{
        BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol, WebSocketLayer,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::time::Instant;
    use url::Url;
    use uuid::Uuid;

    fn sample_flow_with_request_body(size: u64) -> Flow {
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
                    method: "POST".to_string(),
                    url: Url::parse("https://example.com/upload").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: Some(BodyData {
                        encoding: "utf-8".to_string(),
                        content: "x".repeat(size as usize),
                        size,
                    }),
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
        }
    }

    fn sample_ws_flow() -> Flow {
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
            layer: Layer::WebSocket(WebSocketLayer {
                handshake_request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("ws://old.example.com/socket?token=1").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![("Host".to_string(), "old.example.com".to_string())],
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
                    },
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_throttle_zero_kbps_fails() {
        let mut flow = sample_flow_with_request_body(1024);
        let mut ctx = sample_ctx();
        let outcome = execute(&Action::Throttle { kbps: 0 }, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("kbps must be > 0")),
            _ => panic!("expected failed outcome"),
        }
    }

    #[tokio::test]
    async fn test_throttle_delays_by_payload_size() {
        let mut flow = sample_flow_with_request_body(1000);
        let mut ctx = sample_ctx();
        let start = Instant::now();
        let outcome = execute(&Action::Throttle { kbps: 1000 }, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        assert!(
            start.elapsed().as_millis() >= 6,
            "expected at least ~8ms delay for 1000B@1000kbps"
        );
    }

    #[tokio::test]
    async fn test_map_remote_rewrites_origin_and_host_header() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com:8080/api/v1?x=1").expect("url");
            http.request.headers = vec![("Host".to_string(), "old.example.com:8080".to_string())];
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://new.example.com:8443".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));

        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        assert_eq!(
            http.request.url.as_str(),
            "https://new.example.com:8443/api/v1?x=1"
        );
        let host = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "new.example.com:8443");
    }

    #[tokio::test]
    async fn test_map_remote_preserve_host_keeps_original_header() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com/path").expect("url");
            http.request.headers = vec![("Host".to_string(), "old.example.com".to_string())];
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://new.example.com".to_string(),
            preserve_host: true,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));

        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        assert_eq!(http.request.url.as_str(), "https://new.example.com/path");
        let host = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "old.example.com");
    }

    #[tokio::test]
    async fn test_map_remote_invalid_url_fails() {
        let mut flow = sample_flow_with_request_body(0);
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "://bad-url".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("invalid url")),
            _ => panic!("expected failed outcome"),
        }
    }

    #[tokio::test]
    async fn test_map_remote_rejects_non_http_like_layers() {
        let mut flow = sample_flow_with_request_body(0);
        flow.layer = Layer::Tcp(relay_core_api::flow::TcpLayer {
            bytes_up: 0,
            bytes_down: 0,
        });
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://new.example.com".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("only supports HTTP/WebSocket")),
            _ => panic!("expected failed outcome"),
        }
    }

    #[tokio::test]
    async fn test_map_remote_url_supports_variable_substitution() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com/a").expect("url");
            http.request.headers = vec![("Host".to_string(), "old.example.com".to_string())];
        }
        let mut ctx = sample_ctx();
        ctx.variables
            .insert("upstream".to_string(), "new.example.com:9443".to_string());
        let action = Action::MapRemote {
            url: "https://{{upstream}}".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        assert_eq!(http.request.url.as_str(), "https://new.example.com:9443/a");
        let host = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "new.example.com:9443");
    }

    #[tokio::test]
    async fn test_map_remote_inserts_host_header_when_missing() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com/p").expect("url");
            http.request.headers.clear();
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://api.example.com".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        let host_values: Vec<_> = http
            .request
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(host_values, vec!["api.example.com".to_string()]);
    }

    #[tokio::test]
    async fn test_map_remote_deduplicates_multiple_host_headers() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com/p").expect("url");
            http.request.headers = vec![
                ("Host".to_string(), "old.example.com".to_string()),
                ("host".to_string(), "old2.example.com".to_string()),
                ("X-Test".to_string(), "1".to_string()),
            ];
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://api.example.com:9443".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        let host_values: Vec<_> = http
            .request
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(host_values, vec!["api.example.com:9443".to_string()]);
        assert!(
            http.request
                .headers
                .iter()
                .any(|(k, v)| k == "X-Test" && v == "1")
        );
    }

    #[tokio::test]
    async fn test_map_remote_formats_ipv6_host_header_authority() {
        let mut flow = sample_flow_with_request_body(0);
        if let Layer::Http(http) = &mut flow.layer {
            http.request.url = Url::parse("http://old.example.com/p").expect("url");
            http.request.headers = vec![("Host".to_string(), "old.example.com".to_string())];
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://[2001:db8::1]:9443".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        let host = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "[2001:db8::1]:9443");
    }

    #[tokio::test]
    async fn test_map_remote_websocket_formats_ipv6_host_header_authority() {
        let mut flow = sample_ws_flow();
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "wss://[2001:db8::3]:9443".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::WebSocket(ws) = flow.layer else {
            panic!("expected websocket layer");
        };
        let host = ws
            .handshake_request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "[2001:db8::3]:9443");
    }

    #[tokio::test]
    async fn test_map_remote_rewrites_websocket_handshake_origin_and_host() {
        let mut flow = sample_ws_flow();
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "wss://new.example.com:9443".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::WebSocket(ws) = flow.layer else {
            panic!("expected websocket layer");
        };
        assert_eq!(
            ws.handshake_request.url.as_str(),
            "wss://new.example.com:9443/socket?token=1"
        );
        let host = ws
            .handshake_request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "new.example.com:9443");
    }

    #[tokio::test]
    async fn test_map_remote_websocket_preserve_host_keeps_original() {
        let mut flow = sample_ws_flow();
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "wss://new.example.com".to_string(),
            preserve_host: true,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::WebSocket(ws) = flow.layer else {
            panic!("expected websocket layer");
        };
        assert_eq!(ws.handshake_request.url.as_str(), "wss://new.example.com/socket?token=1");
        let host = ws
            .handshake_request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "old.example.com");
    }

    #[tokio::test]
    async fn test_map_remote_websocket_url_supports_variable_substitution() {
        let mut flow = sample_ws_flow();
        let mut ctx = sample_ctx();
        ctx.variables
            .insert("ws_upstream".to_string(), "new.example.com:9555".to_string());
        let action = Action::MapRemote {
            url: "wss://{{ws_upstream}}".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::WebSocket(ws) = flow.layer else {
            panic!("expected websocket layer");
        };
        assert_eq!(
            ws.handshake_request.url.as_str(),
            "wss://new.example.com:9555/socket?token=1"
        );
        let host = ws
            .handshake_request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .expect("host header");
        assert_eq!(host, "new.example.com:9555");
    }

    #[tokio::test]
    async fn test_map_remote_websocket_inserts_host_when_missing() {
        let mut flow = sample_ws_flow();
        if let Layer::WebSocket(ws) = &mut flow.layer {
            ws.handshake_request.headers.clear();
        }
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "wss://api.example.com".to_string(),
            preserve_host: false,
        };

        let outcome = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(outcome, ActionOutcome::Continue));
        let Layer::WebSocket(ws) = flow.layer else {
            panic!("expected websocket layer");
        };
        let host_values: Vec<_> = ws
            .handshake_request
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(host_values, vec!["api.example.com".to_string()]);
    }

    #[tokio::test]
    async fn test_map_remote_rejects_url_without_host() {
        let mut flow = sample_flow_with_request_body(0);
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "ws:missing-host".to_string(),
            preserve_host: false,
        };
        let outcome = execute(&action, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("scheme://host")),
            _ => panic!("expected failed outcome"),
        }
    }

    #[tokio::test]
    async fn test_map_remote_rejects_unsupported_scheme() {
        let mut flow = sample_flow_with_request_body(0);
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "ftp://example.com/resource".to_string(),
            preserve_host: false,
        };
        let outcome = execute(&action, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("unsupported scheme")),
            _ => panic!("expected failed outcome"),
        }
    }

    #[tokio::test]
    async fn test_map_remote_rejects_userinfo() {
        let mut flow = sample_flow_with_request_body(0);
        let mut ctx = sample_ctx();
        let action = Action::MapRemote {
            url: "https://user:pass@example.com/path".to_string(),
            preserve_host: false,
        };
        let outcome = execute(&action, &mut flow, &mut ctx).await;
        match outcome {
            ActionOutcome::Failed(msg) => assert!(msg.contains("must not include userinfo")),
            _ => panic!("expected failed outcome"),
        }
    }
}
