//! Shared flow search matching and filter expression parsing (TUI / CLI / runtime).

use crate::flow::{Flow, Layer};
use crate::modification::FlowQuery;

/// Parsed filter: structured [`FlowQuery`] fields (AND) plus optional plain-text tokens (AND, each matches URL or method).
#[derive(Debug, Clone, Default)]
pub struct FlowFilter {
    pub query: FlowQuery,
    pub text_tokens: Vec<String>,
}

/// Returns true when `flow` satisfies all set fields in `query` (AND semantics).
pub fn flow_matches_query(flow: &Flow, query: &FlowQuery) -> bool {
    let (host_str, path_str, method_str, status, is_ws) = flow_layer_fields(flow);

    if let Some(h) = &query.host
        && !host_str
            .to_ascii_lowercase()
            .contains(&h.to_ascii_lowercase())
    {
        return false;
    }
    if let Some(p) = &query.path_contains
        && !path_str
            .to_ascii_lowercase()
            .contains(&p.to_ascii_lowercase())
    {
        return false;
    }
    if let Some(m) = &query.method
        && !method_str.eq_ignore_ascii_case(m)
    {
        return false;
    }
    if let Some(min) = query.status_min
        && status.is_none_or(|s| s < min)
    {
        return false;
    }
    if let Some(max) = query.status_max
        && status.is_none_or(|s| s > max)
    {
        return false;
    }
    if let Some(ws_only) = query.is_websocket
        && is_ws != ws_only
    {
        return false;
    }
    if let Some(err_only) = query.has_error {
        // Same rule as the summary's `has_error` and the UI's badge: the filter must not disagree
        // with the badge the user is looking at.
        let is_err = crate::flow::flow_is_error(flow);
        if err_only != is_err {
            return false;
        }
    }
    true
}

/// Structured query + plain-text tokens.
pub fn flow_matches_filter(flow: &Flow, filter: &FlowFilter) -> bool {
    if !flow_matches_query(flow, &filter.query) {
        return false;
    }
    if filter.text_tokens.is_empty() {
        return true;
    }
    let (host_str, path_str, method_str, _status, _is_ws) = flow_layer_fields(flow);
    let url_blob = format!(
        "{host_str}{path_str}",
        host_str = host_str.to_ascii_lowercase(),
        path_str = path_str.to_ascii_lowercase()
    );
    let method_lc = method_str.to_ascii_lowercase();
    for token in &filter.text_tokens {
        let needle = token.to_ascii_lowercase();
        if !url_blob.contains(&needle) && !method_lc.contains(&needle) {
            return false;
        }
    }
    true
}

/// Parse a TUI/CLI filter bar expression into structured + plain tokens.
///
/// Structured tokens (AND): `host:`, `path:`, `method:`, `status:` (`404`, `>=400`, `400-499`),
/// bare `err` / `ws`.
/// Plain tokens: words without `:` (unless unknown `key:value` → treated as plain).
/// When there are no structured tokens, the whole input is one plain-text search (preserves spaces).
pub fn parse_flow_filter(input: &str) -> FlowFilter {
    let input = input.trim();
    if input.is_empty() {
        return FlowFilter::default();
    }

    let mut query = FlowQuery::default();
    let mut text_tokens = Vec::new();
    let mut had_structured = false;

    for token in input.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            let key_lc = key.to_ascii_lowercase();
            if value.is_empty() {
                text_tokens.push(token.to_string());
                continue;
            }
            had_structured = true;
            match key_lc.as_str() {
                "host" => query.host = Some(value.to_string()),
                "path" => query.path_contains = Some(value.to_string()),
                "method" => query.method = Some(value.to_string()),
                "status" => apply_status_token(&mut query, value),
                _ => text_tokens.push(token.to_string()),
            }
        } else {
            match token.to_ascii_lowercase().as_str() {
                "err" | "error" => {
                    had_structured = true;
                    query.has_error = Some(true);
                }
                "ws" | "websocket" => {
                    had_structured = true;
                    query.is_websocket = Some(true);
                }
                _ => text_tokens.push(token.to_string()),
            }
        }
    }

    if !had_structured && !text_tokens.is_empty() {
        let joined = text_tokens.join(" ");
        text_tokens = vec![joined];
    }

    FlowFilter { query, text_tokens }
}

fn apply_status_token(query: &mut FlowQuery, value: &str) {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix(">=") {
        if let Ok(n) = rest.parse::<u16>() {
            query.status_min = Some(n);
        }
        return;
    }
    if let Some(rest) = value.strip_prefix("<=") {
        if let Ok(n) = rest.parse::<u16>() {
            query.status_max = Some(n);
        }
        return;
    }
    if let Some(rest) = value.strip_prefix('>') {
        if let Ok(n) = rest.parse::<u16>() {
            query.status_min = Some(n.saturating_add(1));
        }
        return;
    }
    if let Some(rest) = value.strip_prefix('<') {
        if let Ok(n) = rest.parse::<u16>() {
            query.status_max = Some(n.saturating_sub(1));
        }
        return;
    }
    if let Some((lo, hi)) = value.split_once('-')
        && let (Ok(min), Ok(max)) = (lo.trim().parse::<u16>(), hi.trim().parse::<u16>())
    {
        query.status_min = Some(min);
        query.status_max = Some(max);
        return;
    }
    if let Ok(code) = value.parse::<u16>() {
        query.status_min = Some(code);
        query.status_max = Some(code);
    }
}

fn flow_layer_fields(flow: &Flow) -> (String, String, String, Option<u16>, bool) {
    match &flow.layer {
        Layer::Http(h) => {
            let status = h.response.as_ref().map(|r| r.status);
            (
                h.request.url.host_str().unwrap_or("").to_string(),
                h.request.url.path().to_string(),
                h.request.method.clone(),
                status,
                false,
            )
        }
        Layer::WebSocket(ws) => (
            ws.handshake_request
                .url
                .host_str()
                .unwrap_or("")
                .to_string(),
            ws.handshake_request.url.path().to_string(),
            ws.handshake_request.method.clone(),
            Some(ws.handshake_response.status),
            true,
        ),
        _ => (String::new(), String::new(), String::new(), None, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{
        Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol,
    };
    use chrono::Utc;
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn http_flow(url: &str, method: &str, status: Option<u16>) -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".into(),
                client_port: 1,
                server_ip: "127.0.0.1".into(),
                server_port: 443,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: method.into(),
                    url: Url::parse(url).unwrap(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: status.map(|s| HttpResponse {
                    status: s,
                    status_text: "OK".into(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[test]
    fn parse_plain_text_single_blob() {
        let f = parse_flow_filter("  api example ");
        assert_eq!(f.text_tokens, vec!["api example"]);
        assert_eq!(f.query.host, None);
    }

    #[test]
    fn parse_structured_host_and_method() {
        let f = parse_flow_filter("host:api.example method:POST");
        assert_eq!(f.query.host.as_deref(), Some("api.example"));
        assert_eq!(f.query.method.as_deref(), Some("POST"));
        assert!(f.text_tokens.is_empty());
    }

    #[test]
    fn parse_path_token_is_a_substring_constraint() {
        let f = parse_flow_filter("path:/v1/orders");
        assert_eq!(f.query.path_contains.as_deref(), Some("/v1/orders"));
        assert!(f.query.host.is_none());
    }

    /// The web UI's filter bar mirrors this parser (`webui/src/lib/flowFilter.test.ts`), so the cases
    /// below are the ones a second implementation is most likely to get subtly wrong: an unparsable
    /// value is *no* constraint rather than an impossible one, and the strict comparisons saturate.
    #[test]
    fn unparsable_status_token_leaves_no_constraint() {
        for token in [
            "status:abc",
            "status:>abc",
            "status:404abc",
            "status:0x1a",
            "status:-100",
            "status:1-2-3",
            "status:70000",
            "status:>=70000",
        ] {
            let f = parse_flow_filter(token);
            assert_eq!(f.query.status_min, None, "{token} must not set a minimum");
            assert_eq!(f.query.status_max, None, "{token} must not set a maximum");
        }
        // A filter that constrains nothing selects everything, including a row it could not parse.
        assert!(flow_matches_filter(
            &http_flow("http://x.com", "GET", Some(200)),
            &parse_flow_filter("status:70000")
        ));
    }

    #[test]
    fn status_bounds_saturate_at_the_u16_edges() {
        assert_eq!(
            parse_flow_filter("status:>65535").query.status_min,
            Some(65535)
        );
        assert_eq!(parse_flow_filter("status:<0").query.status_max, Some(0));
    }

    #[test]
    fn a_status_constraint_excludes_a_flow_with_no_status() {
        // A request that has not been answered is not a `0`; `status:<400` must not select it.
        let pending = http_flow("http://x.com", "GET", None);
        let q = FlowQuery {
            status_min: Some(200),
            ..Default::default()
        };
        assert!(!flow_matches_query(&pending, &q));
        let q = FlowQuery {
            status_max: Some(399),
            ..Default::default()
        };
        assert!(!flow_matches_query(&pending, &q));
        assert!(flow_matches_query(&pending, &FlowQuery::default()));
    }

    #[test]
    fn a_key_without_a_value_is_plain_text() {
        // `had_structured` is only set for a `key:value` token with a value, so a half-typed token is
        // a word to search for — and it selects nothing, which is the visible consequence.
        let f = parse_flow_filter("status:");
        assert_eq!(f.query.status_min, None);
        assert_eq!(f.query.status_max, None);
        assert_eq!(f.text_tokens, vec!["status:"]);
        assert!(!flow_matches_filter(
            &http_flow("http://x.com", "GET", Some(200)),
            &f
        ));
    }

    #[test]
    fn plain_text_matches_the_method_as_a_substring() {
        // The engine matches `method` with `contains`, not equality: `atch` finds `PATCH`.
        let flow = http_flow("http://api.example.com/x", "PATCH", Some(200));
        assert!(flow_matches_filter(&flow, &parse_flow_filter("atch")));
        assert!(flow_matches_filter(
            &flow,
            &parse_flow_filter("method:PATCH")
        ));
        assert!(!flow_matches_filter(
            &flow,
            &parse_flow_filter("method:PAT")
        ));
    }

    #[test]
    fn plain_text_may_span_the_host_and_the_path() {
        // `host_str` and `path` are concatenated before matching, so a token can cross the boundary.
        let flow = http_flow("http://api.example.com/v1/users", "GET", Some(200));
        assert!(flow_matches_filter(&flow, &parse_flow_filter("com/v1")));
        assert!(flow_matches_filter(&flow, &parse_flow_filter("/v1/users")));
    }

    #[test]
    fn parse_status_range_and_err_ws() {
        let f = parse_flow_filter("status:>=400 err ws");
        assert_eq!(f.query.status_min, Some(400));
        assert_eq!(f.query.status_max, None);
        assert_eq!(f.query.has_error, Some(true));
        assert_eq!(f.query.is_websocket, Some(true));
    }

    #[test]
    fn parse_status_exact_and_range() {
        assert_eq!(parse_flow_filter("status:404").query.status_min, Some(404));
        assert_eq!(parse_flow_filter("status:404").query.status_max, Some(404));
        let r = parse_flow_filter("status:400-499");
        assert_eq!(r.query.status_min, Some(400));
        assert_eq!(r.query.status_max, Some(499));
    }

    #[test]
    fn parse_mixed_structured_and_plain() {
        let f = parse_flow_filter("api host:foo");
        assert_eq!(f.query.host.as_deref(), Some("foo"));
        assert_eq!(f.text_tokens, vec!["api"]);
    }

    #[test]
    fn flow_matches_query_host_case_insensitive() {
        let flow = http_flow("http://API.Example.com/x", "GET", Some(200));
        let q = FlowQuery {
            host: Some("api.example".into()),
            ..Default::default()
        };
        assert!(flow_matches_query(&flow, &q));
    }

    #[test]
    fn flow_matches_query_status_min_and_error() {
        let ok = http_flow("http://x.com", "GET", Some(200));
        let err = http_flow("http://x.com", "GET", Some(500));
        let q = FlowQuery {
            status_min: Some(400),
            ..Default::default()
        };
        assert!(!flow_matches_query(&ok, &q));
        assert!(flow_matches_query(&err, &q));

        let q_err = FlowQuery {
            has_error: Some(true),
            ..Default::default()
        };
        assert!(flow_matches_query(&err, &q_err));
    }

    #[test]
    fn flow_matches_filter_plain_case_insensitive() {
        let flow = http_flow("http://api.example.com/x", "get", Some(200));
        let filter = parse_flow_filter("API");
        assert!(flow_matches_filter(&flow, &filter));
        let filter2 = parse_flow_filter("POST");
        assert!(!flow_matches_filter(&flow, &filter2));
    }

    #[test]
    fn flow_matches_filter_combined() {
        let flow = http_flow("http://api.example.com/x", "POST", Some(201));
        let filter = parse_flow_filter("host:api method:POST status:>=200");
        assert!(flow_matches_filter(&flow, &filter));
        let filter_fail = parse_flow_filter("host:other method:POST");
        assert!(!flow_matches_filter(&flow, &filter_fail));
    }

    #[test]
    fn err_filter_matches_an_upstream_failure() {
        // The request never reached an answer, so there is no status to look at. `err` used to be
        // status-and-tag only, which meant the filter could not find the failures the interface
        // labels as errors — the same row the summary called healthy.
        let mut flow = http_flow("http://api.example.com/x", "GET", None);
        if let Layer::Http(http) = &mut flow.layer {
            http.error = Some("Upstream Error: connection refused".into());
        }
        assert!(crate::flow::flow_is_error(&flow));
        assert!(flow_matches_query(
            &flow,
            &FlowQuery {
                has_error: Some(true),
                ..Default::default()
            }
        ));
        assert!(flow_matches_filter(&flow, &parse_flow_filter("err")));
    }

    #[test]
    fn err_filter_ignores_a_flow_that_only_has_a_404() {
        let flow = http_flow("http://api.example.com/missing", "GET", Some(404));
        assert!(!crate::flow::flow_is_error(&flow));
        assert!(!flow_matches_filter(&flow, &parse_flow_filter("err")));
    }
}
