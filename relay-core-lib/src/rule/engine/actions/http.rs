use crate::rule::model::{Action, BodySource, TerminalReason};
use crate::rule::engine::executor::ExecutionContext;
use crate::rule::engine::actions::utils::{substitute_variables, resolve_body_source};
use crate::rule::engine::actions::transform::apply_transform;
use crate::rule::engine::actions::ActionOutcome;
use relay_core_api::flow::{Cookie, Flow, Layer, HttpResponse, ResponseTiming};
use url::Url;

fn status_text_for(code: u16) -> String {
    http::StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason().map(|r| r.to_string()))
        .unwrap_or_else(|| "Mocked".to_string())
}

fn parse_set_cookie_header(value: &str) -> Option<Cookie> {
    let mut segments = value.split(';');
    let first = segments.next()?.trim();
    let (name, cookie_value) = first.split_once('=')?;
    if name.trim().is_empty() {
        return None;
    }

    let mut cookie = Cookie {
        name: name.trim().to_string(),
        value: cookie_value.trim().to_string(),
        path: None,
        domain: None,
        expires: None,
        http_only: None,
        secure: None,
    };

    for seg in segments {
        let attr = seg.trim();
        if attr.eq_ignore_ascii_case("httponly") {
            cookie.http_only = Some(true);
            continue;
        }
        if attr.eq_ignore_ascii_case("secure") {
            cookie.secure = Some(true);
            continue;
        }
        if let Some((k, v)) = attr.split_once('=') {
            let key = k.trim();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("path") {
                cookie.path = Some(val);
            } else if key.eq_ignore_ascii_case("domain") {
                cookie.domain = Some(val);
            } else if key.eq_ignore_ascii_case("expires") {
                cookie.expires = Some(val);
            }
        }
    }

    Some(cookie)
}

fn parse_response_cookies(headers: &[(String, String)]) -> Vec<Cookie> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, v)| parse_set_cookie_header(v))
        .collect()
}

pub async fn execute(
    action: &Action,
    flow: &mut Flow,
    ctx: &mut ExecutionContext,
) -> ActionOutcome {
    match action {
        // --- Request Modification Actions ---
        Action::SetRequestMethod { method } => {
            let val = substitute_variables(method, flow, ctx, None);
            if let Layer::Http(http) = &mut flow.layer {
                http.request.method = val;
            }
            ActionOutcome::Continue
        }
        Action::SetRequestUrl { url } => {
            let val = substitute_variables(url, flow, ctx, None);
            if let Ok(new_url) = Url::parse(&val)
                && let Layer::Http(http) = &mut flow.layer {
                    http.request.url = new_url;
                }
            ActionOutcome::Continue
        }
        Action::SetRequestBody { body } => {
            if let Some(body_data) = resolve_body_source(body, ctx.policy.as_deref()).await
                && let Layer::Http(http) = &mut flow.layer {
                    http.request.body = Some(body_data);
                }
            ActionOutcome::Continue
        }

        // --- Response Modification Actions ---
        Action::SetResponseStatus { status } => {
            if let Layer::Http(http) = &mut flow.layer
                && let Some(res) = &mut http.response {
                    res.status = *status;
                }
            ActionOutcome::Continue
        }
        Action::SetResponseBody { body } => {
            if let Some(body_data) = resolve_body_source(body, ctx.policy.as_deref()).await
                && let Layer::Http(http) = &mut flow.layer
                     && let Some(res) = &mut http.response {
                         res.body = Some(body_data);
                     }
            ActionOutcome::Continue
        }
        Action::Redirect { location, status } => {
            let val = substitute_variables(location, flow, ctx, None);
            let headers = vec![("Location".to_string(), val)];
            
            let res = HttpResponse {
                status: *status,
                status_text: "Redirect".to_string(),
                version: "HTTP/1.1".to_string(),
                headers,
                cookies: vec![],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: Some(0),
                    time_to_last_byte: Some(0),
                connect_time_ms: None,
                ssl_time_ms: None,
                },
            };

            if let Layer::Http(http) = &mut flow.layer {
                http.response = Some(res);
            } else if let Layer::WebSocket(ws) = &mut flow.layer {
                ws.handshake_response = res;
            }
            ActionOutcome::Terminated(TerminalReason::Redirect)
        }

        // --- Request Header Actions ---
        Action::AddRequestHeader { name, value } => {
            let val = substitute_variables(value, flow, ctx, None);
            if let Layer::Http(http) = &mut flow.layer {
                http.request.headers.push((name.clone(), val));
            }
            ActionOutcome::Continue
        }
        Action::UpdateRequestHeader {
            name,
            value,
            add_if_missing,
        } => {
            // First pass: find existing value to use as {{previous}}
            let mut old_val = None;
            if let Layer::Http(http) = &flow.layer {
                 for (k, v) in &http.request.headers {
                     if k.eq_ignore_ascii_case(name) {
                         old_val = Some(v.clone());
                         break;
                     }
                 }
            }

            // Compute new value
            let new_val = substitute_variables(value, flow, ctx, old_val.as_deref());

            // Second pass: apply change
            if let Layer::Http(http) = &mut flow.layer {
                let mut found = false;
                for (k, v) in http.request.headers.iter_mut() {
                    if k.eq_ignore_ascii_case(name) {
                        *v = new_val.clone();
                        found = true;
                    }
                }
                if !found && *add_if_missing {
                    http.request.headers.push((name.clone(), new_val));
                }
            }
            ActionOutcome::Continue
        }
        Action::DeleteRequestHeader { name } => {
            if let Layer::Http(http) = &mut flow.layer {
                http.request.headers
                    .retain(|(k, _)| !k.eq_ignore_ascii_case(name));
            }
            ActionOutcome::Continue
        }

        // --- Response Header Actions ---
        Action::AddResponseHeader { name, value } => {
            let val = substitute_variables(value, flow, ctx, None);
            if let Layer::Http(http) = &mut flow.layer
                && let Some(res) = &mut http.response {
                    res.headers.push((name.clone(), val));
                }
            ActionOutcome::Continue
        }
        Action::UpdateResponseHeader { name, value, add_if_missing } => {
            // First pass: find existing value
            let mut old_val = None;
            if let Layer::Http(http) = &flow.layer
                 && let Some(res) = &http.response {
                     for (k, v) in &res.headers {
                         if k.eq_ignore_ascii_case(name) {
                             old_val = Some(v.clone());
                             break;
                         }
                     }
                 }
            
            // Compute new value
            let new_val = substitute_variables(value, flow, ctx, old_val.as_deref());

            // Second pass: apply change
            if let Layer::Http(http) = &mut flow.layer
                && let Some(res) = &mut http.response {
                    let mut found = false;
                    for (k, v) in res.headers.iter_mut() {
                        if k.eq_ignore_ascii_case(name) {
                            *v = new_val.clone();
                            found = true;
                        }
                    }
                    if !found && *add_if_missing {
                        res.headers.push((name.clone(), new_val));
                    }
                }
            ActionOutcome::Continue
        }
         Action::DeleteResponseHeader { name } => {
            if let Layer::Http(http) = &mut flow.layer
                 && let Some(res) = &mut http.response {
                    res.headers
                    .retain(|(k, _)| !k.eq_ignore_ascii_case(name));
                 }
            ActionOutcome::Continue
        }
        
        // --- Terminal Actions ---
        Action::MockResponse { status, headers, body } => {
             let body_data = if let Some(source) = body {
                 resolve_body_source(source, ctx.policy.as_deref()).await
             } else {
                 None
             };

             if let Layer::Http(http) = &mut flow.layer {
                 let mut res_headers = Vec::new();
                 for (k, v) in headers {
                     res_headers.push((k.clone(), v.clone()));
                 }
                 let cookies = parse_response_cookies(&res_headers);
                 
                 http.response = Some(HttpResponse {
                    status: *status,
                    status_text: status_text_for(*status),
                    version: "HTTP/1.1".to_string(),
                    headers: res_headers,
                    cookies,
                    body: body_data,
                    timing: ResponseTiming {
                        time_to_first_byte: Some(0),
                        time_to_last_byte: Some(0),
                connect_time_ms: None,
                ssl_time_ms: None,
                    },
                 });
             } else if let Layer::WebSocket(ws) = &mut flow.layer {
                 let mut res_headers = Vec::new();
                 for (k, v) in headers {
                     res_headers.push((k.clone(), v.clone()));
                 }
                 let cookies = parse_response_cookies(&res_headers);
                 
                 ws.handshake_response = HttpResponse {
                    status: *status,
                    status_text: status_text_for(*status),
                    version: "HTTP/1.1".to_string(),
                    headers: res_headers,
                    cookies,
                    body: body_data,
                    timing: ResponseTiming {
                        time_to_first_byte: Some(0),
                        time_to_last_byte: Some(0),
                connect_time_ms: None,
                ssl_time_ms: None,
                    },
                 };
             }
             ActionOutcome::Terminated(TerminalReason::Mock)
        }
        Action::MapLocal { path, content_type } => {
            let body_data = resolve_body_source(&BodySource::File(path.clone()), ctx.policy.as_deref()).await;
            
            if let Some(body) = body_data {
                 if let Layer::Http(http) = &mut flow.layer {
                    let headers = content_type.as_ref()
                        .map(|ct| vec![("Content-Type".to_string(), ct.clone())])
                        .unwrap_or_default();

                    http.response = Some(HttpResponse {
                        status: 200,
                        status_text: "OK".to_string(),
                        version: "HTTP/1.1".to_string(),
                        headers,
                        cookies: vec![],
                        body: Some(body),
                        timing: ResponseTiming {
                            time_to_first_byte: Some(0),
                            time_to_last_byte: Some(0),
                connect_time_ms: None,
                ssl_time_ms: None,
                        },
                     });
                }
                ActionOutcome::Terminated(TerminalReason::Mock)
            } else {
                ActionOutcome::Failed(format!("Failed to load local file: {}", path))
            }
        }
        
        // --- Transformation Actions ---
        Action::TransformRequestBody { transform } => {
             if let Layer::Http(http) = &mut flow.layer
                 && let Some(body) = &mut http.request.body {
                     apply_transform(body, transform);
                 }
             ActionOutcome::Continue
        }
        Action::TransformResponseBody { transform } => {
             if let Layer::Http(http) = &mut flow.layer
                 && let Some(res) = &mut http.response
                     && let Some(body) = &mut res.body {
                         apply_transform(body, transform);
                     }
             ActionOutcome::Continue
        }

        _ => ActionOutcome::Failed(format!("Action {:?} not supported in http handler", action)),
    }
}

#[cfg(test)]
mod tests {
    use super::{execute, parse_set_cookie_header};
    use crate::rule::engine::actions::ActionOutcome;
    use crate::rule::engine::executor::ExecutionContext;
    use crate::rule::engine::state::InMemoryRuleStateStore;
    use crate::rule::model::{Action, RuleTraceSummary};
    use chrono::Utc;
    use relay_core_api::flow::{Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol};
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
                    url: Url::parse("https://example.com/").expect("url"),
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
        }
    }

    #[test]
    fn test_parse_set_cookie_header_with_attributes() {
        let c = parse_set_cookie_header("sid=abc; Path=/; Domain=example.com; HttpOnly; Secure; Expires=Wed, 21 Oct 2026 07:28:00 GMT")
            .expect("cookie");
        assert_eq!(c.name, "sid");
        assert_eq!(c.value, "abc");
        assert_eq!(c.path.as_deref(), Some("/"));
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.http_only, Some(true));
        assert_eq!(c.secure, Some(true));
        assert!(c.expires.is_some());
    }

    #[tokio::test]
    async fn test_mock_response_extracts_set_cookie() {
        let mut headers = HashMap::new();
        headers.insert(
            "Set-Cookie".to_string(),
            "token=xyz; Path=/; HttpOnly".to_string(),
        );
        let action = Action::MockResponse {
            status: 200,
            headers,
            body: None,
        };

        let mut flow = sample_flow();
        let mut ctx = sample_ctx();
        let out = execute(&action, &mut flow, &mut ctx).await;
        assert!(matches!(out, ActionOutcome::Terminated(_)));

        let Layer::Http(http) = flow.layer else {
            panic!("expected http layer");
        };
        let res = http.response.expect("mocked response");
        assert_eq!(res.cookies.len(), 1);
        assert_eq!(res.cookies[0].name, "token");
        assert_eq!(res.cookies[0].value, "xyz");
        assert_eq!(res.cookies[0].path.as_deref(), Some("/"));
        assert_eq!(res.cookies[0].http_only, Some(true));
    }
}
