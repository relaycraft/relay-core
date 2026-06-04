//! Human-readable `tracing` messages (avoid `Option` Debug noise like `Some(9090)`).

use crate::audit::AuditEventKind;
use serde_json::Value;

pub(crate) fn opt_u16(v: Option<u16>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
}

pub(crate) fn opt_u64(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
}

/// Stable `key=value` lifecycle line for tests and log pipelines (matches tracing field values).
#[cfg(test)]
fn lifecycle_fields_line(
    phase: &str,
    port: Option<u16>,
    started_at_ms: Option<u64>,
    last_error: &str,
) -> String {
    format!(
        "phase={phase} port={} started_at_ms={} last_error={last_error}",
        opt_u16(port),
        opt_u64(started_at_ms),
    )
}

/// Compact audit `details` for logs.
pub fn audit_details_log(kind: &AuditEventKind, details: &Value) -> String {
    match kind {
        AuditEventKind::PolicyUpdated => {
            let strict = bool_on(details.get("strict_http_semantics"));
            let timeout = details
                .get("request_timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let max_body = details
                .get("max_body_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let transparent = bool_on(details.get("transparent_enabled"));
            let redaction = bool_on(details.get("redaction_enabled"));
            format!(
                "strict_http={strict} timeout_ms={timeout} max_body={max_body} transparent={transparent} redaction={redaction}"
            )
        }
        _ => truncate_json(details, 160),
    }
}

fn bool_on(v: Option<&Value>) -> &'static str {
    if v.and_then(|x| x.as_bool()).unwrap_or(false) {
        "on"
    } else {
        "off"
    }
}

fn truncate_json(v: &Value, max: usize) -> String {
    let s = v.to_string();
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_fields_line_uses_dash_for_none() {
        assert_eq!(
            lifecycle_fields_line("starting", Some(9090), None, "-"),
            "phase=starting port=9090 started_at_ms=- last_error=-"
        );
    }

    #[test]
    fn lifecycle_fields_line_running_snapshot() {
        assert_eq!(
            lifecycle_fields_line("running", Some(9090), Some(1_780_585_037_045), "-"),
            "phase=running port=9090 started_at_ms=1780585037045 last_error=-"
        );
    }

    #[test]
    fn truncate_json_respects_utf8_char_boundaries() {
        let v = json!({ "msg": "中文测试🙂".repeat(20) });
        let s = truncate_json(&v, 12);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 13); // max body + ellipsis
        std::str::from_utf8(s.as_bytes()).expect("truncated output must be valid UTF-8");
    }

    #[test]
    fn policy_updated_details_are_compact() {
        let details = json!({
            "strict_http_semantics": true,
            "request_timeout_ms": 30000,
            "max_body_size": 10485760,
            "transparent_enabled": false,
            "redaction_enabled": false
        });
        let s = audit_details_log(&AuditEventKind::PolicyUpdated, &details);
        assert!(s.contains("strict_http=on"));
        assert!(s.contains("timeout_ms=30000"));
        assert!(!s.contains("Some("));
    }
}
