//! Human-readable `tracing` messages (avoid `Option` Debug noise like `Some(9090)`).

use crate::RuntimeLifecycle;
use crate::audit::AuditEventKind;
use serde_json::Value;

/// One-line lifecycle snapshot for logs.
pub fn lifecycle_log_line(lifecycle: &RuntimeLifecycle) -> String {
    let port = opt_u16(lifecycle.port);
    let started = opt_u64(lifecycle.started_at_ms);
    let err = lifecycle.last_error.as_deref().unwrap_or("-");
    format!(
        "phase={} port={port} started_at_ms={started} last_error={err}",
        lifecycle.phase.as_str(),
    )
}

fn opt_u16(v: Option<u16>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
}

fn opt_u64(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
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
    use crate::RuntimeLifecyclePhase;
    use serde_json::json;

    #[test]
    fn lifecycle_line_uses_dash_for_none() {
        let line = lifecycle_log_line(&RuntimeLifecycle {
            phase: RuntimeLifecyclePhase::Starting,
            port: Some(9090),
            started_at_ms: None,
            last_error: None,
        });
        assert_eq!(
            line,
            "phase=starting port=9090 started_at_ms=- last_error=-"
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
