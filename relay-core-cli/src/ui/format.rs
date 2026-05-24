//! TUI formatting helpers (durations, URL display, cURL export).

use relay_core_api::flow::{Flow, HttpRequest, Layer};
use url::Url;

/// Terminal width below which the UI uses single-pane list/detail mode.
pub const LAYOUT_NARROW_MAX: u16 = 80;

/// Minimum width for Host + Path + Duration columns.
pub const TABLE_WIDE_MIN: u16 = 120;

/// Middle-ellipsis truncation keeping both ends visible (paths).
pub fn smart_truncate(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.chars().count() <= max_width {
        return s.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let tail_len = max_width.saturating_sub(3) / 2;
    let head_len = max_width.saturating_sub(3).saturating_sub(tail_len);
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

/// Milliseconds → display (`---` if None).
pub fn format_duration_ms(ms: Option<u64>) -> String {
    match ms {
        None => "---".to_string(),
        Some(0) => "0ms".to_string(),
        Some(n) if n < 1000 => format!("{n}ms"),
        Some(n) if n < 60_000 => format!("{:.1}s", n as f64 / 1000.0),
        Some(n) => format!("{:.1}m", n as f64 / 60_000.0),
    }
}

/// Duration in ms from flow timestamps or response timing.
pub fn flow_duration_ms(flow: &Flow) -> Option<u64> {
    if let Some(end) = flow.end_time {
        let ms = (end - flow.start_time).num_milliseconds();
        if ms >= 0 {
            return Some(ms as u64);
        }
    }
    match &flow.layer {
        Layer::Http(h) => h
            .response
            .as_ref()
            .and_then(|r| r.timing.time_to_last_byte.or(r.timing.time_to_first_byte)),
        Layer::WebSocket(w) => w
            .handshake_response
            .timing
            .time_to_last_byte
            .or(w.handshake_response.timing.time_to_first_byte),
        _ => None,
    }
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Method label with POST+ when body present.
pub fn display_method(method: &str, has_body: bool) -> String {
    if method == "POST" && has_body {
        format!("{method}+")
    } else {
        method.to_string()
    }
}

/// Path (+ `?` when query present), truncated for column width.
pub fn display_path(url: &Url, max_width: usize, append_query_hint: bool) -> String {
    let mut path = url.path().to_string();
    if path.is_empty() {
        path = "/".to_string();
    }
    if append_query_hint && url.query().is_some() {
        path.push('?');
    }
    smart_truncate(&path, max_width)
}

pub fn host_from_url(url: &Url) -> String {
    url.host_str().unwrap_or("-").to_string()
}

pub fn flow_list_title(filter: &str, filtered_count: usize, total_in_list: usize) -> String {
    if filter.is_empty() {
        format!(" Flows ({filtered_count}) ")
    } else {
        format!(" Flows · filter: {filter} ({filtered_count}/{total_in_list}) ")
    }
}

fn shell_escape_single(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Build a shell-ready cURL command for an HTTP request.
pub fn http_request_to_curl(req: &HttpRequest) -> String {
    let mut parts = vec![format!("curl -X {}", req.method)];
    parts.push(format!("'{}'", shell_escape_single(req.url.as_str())));
    for (name, value) in &req.headers {
        parts.push(format!(
            "-H '{}: {}'",
            shell_escape_single(name),
            shell_escape_single(value)
        ));
    }
    if let Some(body) = &req.body
        && !body.content.is_empty()
    {
        parts.push(format!("-d '{}'", shell_escape_single(&body.content)));
    }
    parts.join(" \\\n  ")
}

pub fn http_flow_to_curl(flow: &Flow) -> Option<String> {
    match &flow.layer {
        Layer::Http(h) => Some(http_request_to_curl(&h.request)),
        Layer::WebSocket(w) => Some(http_request_to_curl(&w.handshake_request)),
        _ => None,
    }
}

/// Copy text to the system clipboard (best effort).
pub fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        for cmd in ["wl-copy", "xclip -selection clipboard"] {
            let mut parts = cmd.split_whitespace();
            if let Some(bin) = parts.next()
                && let Ok(mut child) = Command::new(bin).args(parts).stdin(Stdio::piped()).spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if child.wait().map(|s| s.success()).unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    let _ = text;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol,
    };
    use std::collections::HashMap;

    fn sample_flow(url: &str, method: &str, end: Option<chrono::DateTime<Utc>>) -> Flow {
        Flow {
            id: uuid::Uuid::new_v4(),
            start_time: Utc::now() - chrono::Duration::milliseconds(200),
            end_time: end,
            network: NetworkInfo {
                client_ip: "127.0.0.1".into(),
                client_port: 1,
                server_ip: "1.1.1.1".into(),
                server_port: 443,
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
                    headers: vec![("Accept".into(), "application/json".into())],
                    cookies: vec![],
                    query: if url.contains('?') {
                        vec![("q".into(), "1".into())]
                    } else {
                        vec![]
                    },
                    body: if method == "POST" {
                        Some(relay_core_api::flow::BodyData {
                            encoding: "utf-8".into(),
                            content: "{}".into(),
                            size: 2,
                        })
                    } else {
                        None
                    },
                },
                response: Some(HttpResponse {
                    status: 200,
                    status_text: "OK".into(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    timing: ResponseTiming {
                        time_to_first_byte: Some(42),
                        time_to_last_byte: Some(150),
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
    fn smart_truncate_keeps_ends() {
        let s = "/x/web-show/render/extra/long/path";
        let t = smart_truncate(s, 20);
        assert!(t.contains("..."));
        assert!(t.starts_with('/'));
        assert!(t.ends_with("path"));
        assert!(t.chars().count() <= 20);
    }

    #[test]
    fn smart_truncate_short_unchanged() {
        assert_eq!(smart_truncate("/api", 10), "/api");
    }

    #[test]
    fn smart_truncate_tiny_width() {
        assert_eq!(smart_truncate("abcdef", 2).len(), 2);
    }

    #[test]
    fn format_duration_ms_formats() {
        assert_eq!(format_duration_ms(None), "---");
        assert_eq!(format_duration_ms(Some(50)), "50ms");
        assert_eq!(format_duration_ms(Some(1500)), "1.5s");
    }

    #[test]
    fn flow_duration_prefers_end_minus_start() {
        let start = Utc::now();
        let end = start + chrono::Duration::milliseconds(88);
        let flow = sample_flow("http://example.com/", "GET", Some(end));
        let mut flow = flow;
        flow.start_time = start;
        flow.end_time = Some(end);
        assert_eq!(flow_duration_ms(&flow), Some(88));
    }

    #[test]
    fn flow_duration_falls_back_to_timing() {
        let flow = sample_flow("http://example.com/", "GET", None);
        assert_eq!(flow_duration_ms(&flow), Some(150));
    }

    #[test]
    fn display_method_post_plus() {
        assert_eq!(display_method("POST", true), "POST+");
        assert_eq!(display_method("POST", false), "POST");
        assert_eq!(display_method("GET", true), "GET");
    }

    #[test]
    fn display_path_query_hint() {
        let url = Url::parse("http://h.example/a/b?q=1").unwrap();
        assert!(display_path(&url, 40, true).ends_with('?'));
        assert!(!display_path(&url, 40, false).ends_with('?'));
    }

    #[test]
    fn flow_list_title_with_filter() {
        assert!(flow_list_title("host:api", 3, 10).contains("host:api"));
        assert!(flow_list_title("host:api", 3, 10).contains("3/10"));
    }

    #[test]
    fn http_request_to_curl_includes_method_url_and_header() {
        let flow = sample_flow("http://example.com/path", "GET", None);
        let curl = http_flow_to_curl(&flow).unwrap();
        assert!(curl.contains("curl -X GET"));
        assert!(curl.contains("http://example.com/path"));
        assert!(curl.contains("Accept"));
    }

    #[test]
    fn layout_constants_match_spec() {
        assert_eq!(LAYOUT_NARROW_MAX, 80);
        assert_eq!(TABLE_WIDE_MIN, 120);
    }
}
