use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedactionPolicy {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_sensitive_header_names")]
    pub sensitive_header_names: Vec<String>,
    #[serde(default = "default_sensitive_query_keys")]
    pub sensitive_query_keys: Vec<String>,
    #[serde(default = "default_false")]
    pub redact_bodies: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RedactionPolicyPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub sensitive_header_names: Option<Vec<String>>,
    #[serde(default)]
    pub sensitive_query_keys: Option<Vec<String>>,
    #[serde(default)]
    pub redact_bodies: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ProxyPolicyPatch {
    #[serde(default)]
    pub redaction: Option<RedactionPolicyPatch>,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            sensitive_header_names: default_sensitive_header_names(),
            sensitive_query_keys: default_sensitive_query_keys(),
            redact_bodies: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPolicy {
    /// In strict mode, invalid method/status does not silently rewrite to GET/200.
    #[serde(default = "default_true")]
    pub strict_http_semantics: bool,

    /// Allow fallback to GET for invalid methods (only if strict_http_semantics is false)
    #[serde(default = "default_false")]
    pub allow_fallback_method: bool,

    /// Allow fallback to 200 OK for invalid status (only if strict_http_semantics is false)
    #[serde(default = "default_false")]
    pub allow_fallback_status: bool,

    /// Enable automatic retries for idempotent requests
    #[serde(default = "default_false")]
    pub enable_retry: bool,

    /// Only retry idempotent methods (GET, HEAD, OPTIONS)
    #[serde(default = "default_true")]
    pub retry_idempotent_only: bool,

    /// Maximum number of retries
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,

    /// Root directory for local file access (sandbox)
    pub sandbox_root: Option<PathBuf>,

    /// Maximum allowed size for local file read
    #[serde(default = "default_max_file_bytes")]
    pub max_local_file_bytes: usize,

    /// Maximum allowed request/response body size for proxy inspection
    #[serde(default = "default_max_body_bytes")]
    pub max_body_size: usize,

    /// Request timeout in milliseconds (connect + send request + receive response headers)
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,

    /// Enable transparent proxy mode
    #[serde(default = "default_false")]
    pub transparent_enabled: bool,

    /// Require original destination to be present (strict mode)
    #[serde(default = "default_true")]
    pub transparent_require_original_dst: bool,

    /// Allow fallback to Host header when original destination is missing
    #[serde(default = "default_false")]
    pub transparent_allow_host_fallback: bool,

    /// Reject connections that would create a loop
    #[serde(default = "default_true")]
    pub transparent_reject_loopback_target: bool,

    /// Log level for transparent proxy events
    #[serde(default = "default_transparent_log_level")]
    pub transparent_log_level: TransparentLogLevel,

    /// QUIC handling mode
    #[serde(default = "default_quic_mode")]
    pub quic_mode: QuicMode,

    /// Optionally emit Clear-Site-Data: "cache" to invalidate client Alt-Svc cache
    #[serde(default = "default_false")]
    pub quic_downgrade_clear_cache: bool,

    #[serde(default)]
    pub redaction: RedactionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransparentLogLevel {
    Silent, // No logging
    Info,   // Log connections only
    Debug,  // Log with destination details
    Trace,  // Full packet-level logging (expensive)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QuicMode {
    /// Force clients to use HTTP/1.1 or HTTP/2
    Downgrade,

    /// Pass through QUIC traffic without inspection
    Passthrough,

    /// [EXPERIMENTAL] Full HTTP/3 MITM
    #[cfg(feature = "quic_mitm_experimental")]
    ExperimentalMitm,
}

fn default_quic_mode() -> QuicMode {
    QuicMode::Downgrade
}

impl Default for ProxyPolicy {
    fn default() -> Self {
        Self {
            strict_http_semantics: true,
            allow_fallback_method: false,
            allow_fallback_status: false,
            enable_retry: false,
            retry_idempotent_only: true,
            max_retries: 3,
            sandbox_root: None,
            max_local_file_bytes: 10 * 1024 * 1024, // 10MB
            max_body_size: 10 * 1024 * 1024,        // 10MB
            request_timeout_ms: 30_000,             // 30 seconds
            transparent_enabled: false,
            transparent_require_original_dst: true,
            transparent_allow_host_fallback: false,
            transparent_reject_loopback_target: true,
            transparent_log_level: TransparentLogLevel::Info,
            quic_mode: QuicMode::Downgrade,
            quic_downgrade_clear_cache: false,
            redaction: RedactionPolicy::default(),
        }
    }
}

impl RedactionPolicy {
    pub fn apply_patch(&mut self, patch: RedactionPolicyPatch) {
        if let Some(enabled) = patch.enabled {
            self.enabled = enabled;
        }
        if let Some(names) = patch.sensitive_header_names {
            self.sensitive_header_names = names;
        }
        if let Some(keys) = patch.sensitive_query_keys {
            self.sensitive_query_keys = keys;
        }
        if let Some(redact_bodies) = patch.redact_bodies {
            self.redact_bodies = redact_bodies;
        }
    }
}

impl ProxyPolicy {
    pub fn apply_patch(&mut self, patch: ProxyPolicyPatch) {
        if let Some(redaction_patch) = patch.redaction {
            self.redaction.apply_patch(redaction_patch);
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}
fn default_max_retries() -> u8 {
    3
}
fn default_max_file_bytes() -> usize {
    10 * 1024 * 1024
}
fn default_max_body_bytes() -> usize {
    10 * 1024 * 1024
}
fn default_request_timeout_ms() -> u64 {
    30_000
}
fn default_transparent_log_level() -> TransparentLogLevel {
    TransparentLogLevel::Info
}
fn default_sensitive_header_names() -> Vec<String> {
    vec![
        "authorization".to_string(),
        "proxy-authorization".to_string(),
        "cookie".to_string(),
        "set-cookie".to_string(),
        "x-api-key".to_string(),
        "x-auth-token".to_string(),
    ]
}
fn default_sensitive_query_keys() -> Vec<String> {
    vec![
        "token".to_string(),
        "access_token".to_string(),
        "refresh_token".to_string(),
        "api_key".to_string(),
        "apikey".to_string(),
        "password".to_string(),
        "secret".to_string(),
    ]
}
