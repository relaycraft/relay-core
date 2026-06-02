use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::fmt;
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
    #[serde(default)]
    pub upstream: Option<UpstreamProxyConfig>,
}

// ── Upstream Proxy ──────────────────────────────────────

/// Upstream proxy configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct UpstreamProxyConfig {
    /// Upstream proxy URL, e.g. "http://corp-proxy:8080" or "https://secure-proxy:8443"
    pub proxy_url: String,
    /// Optional HTTP Basic authentication
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<UpstreamAuth>,
    /// Hosts to bypass upstream (CIDR with `cidr:` prefix, IP literals, or glob hostnames)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bypass_hosts: Vec<String>,
    /// When true, unreachable upstream falls back to direct connection (default false = fail-closed)
    #[serde(default)]
    pub fail_open: bool,
}

/// HTTP Basic credentials for upstream proxy authentication.
#[derive(Clone, Serialize, Deserialize)]
pub struct UpstreamAuth {
    pub username: String,
    #[serde(
        serialize_with = "serialize_secret",
        deserialize_with = "deserialize_secret"
    )]
    pub password: SecretString,
}

fn serialize_secret<S: serde::Serializer>(_v: &SecretString, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str("***")
}
fn deserialize_secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SecretString, D::Error> {
    let s = String::deserialize(d)?;
    Ok(SecretString::new(s.into()))
}

impl fmt::Debug for UpstreamProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpstreamProxyConfig")
            .field("proxy_url", &self.proxy_url)
            .field("auth", &self.auth.as_ref().map(|_| "***"))
            .field("bypass_hosts", &self.bypass_hosts)
            .field("fail_open", &self.fail_open)
            .finish()
    }
}

impl UpstreamAuth {
    pub fn new(username: String, password: String) -> Self {
        Self {
            username,
            password: SecretString::new(password.into()),
        }
    }
}

impl fmt::Debug for UpstreamAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpstreamAuth")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

impl PartialEq for UpstreamProxyConfig {
    fn eq(&self, other: &Self) -> bool {
        self.proxy_url == other.proxy_url
            && self.bypass_hosts == other.bypass_hosts
            && self.fail_open == other.fail_open
            && match (&self.auth, &other.auth) {
                (Some(a), Some(b)) => {
                    a.username == b.username
                        && a.password.expose_secret() == b.password.expose_secret()
                }
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for UpstreamProxyConfig {}

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

    /// P1: Maximum bytes to buffer for rule body inspection.
    /// When exceeded, body-stage rules are skipped with budget_exceeded tag.
    /// Default: 1 MB. Set to 0 to disable rule body inspection entirely.
    #[serde(default = "default_rule_body_inspect_budget")]
    pub rule_body_inspect_budget: usize,

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

    /// Upstream (parent) proxy configuration. None = direct connection mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamProxyConfig>,
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
            rule_body_inspect_budget: 1024 * 1024,  // 1MB
            request_timeout_ms: 30_000,             // 30 seconds
            transparent_enabled: false,
            transparent_require_original_dst: true,
            transparent_allow_host_fallback: false,
            transparent_reject_loopback_target: true,
            transparent_log_level: TransparentLogLevel::Info,
            quic_mode: QuicMode::Downgrade,
            quic_downgrade_clear_cache: false,
            redaction: RedactionPolicy::default(),
            upstream: None,
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
        if let Some(upstream) = patch.upstream {
            self.upstream = Some(upstream);
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
fn default_rule_body_inspect_budget() -> usize {
    1024 * 1024 // 1MB
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

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn upstream_proxy_config_debug_masks_password() {
        let cfg = UpstreamProxyConfig {
            proxy_url: "http://proxy:8080".to_string(),
            auth: Some(UpstreamAuth {
                username: "user".to_string(),
                password: SecretString::new("s3cret".to_string().into()),
            }),
            bypass_hosts: vec!["*.local".to_string()],
            fail_open: false,
        };
        let dbg = format!("{:?}", cfg);
        assert!(dbg.contains("http://proxy:8080"));
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("s3cret"));
    }

    #[test]
    fn upstream_auth_debug_masks_password() {
        let auth = UpstreamAuth {
            username: "user".to_string(),
            password: SecretString::new("s3cret".to_string().into()),
        };
        let dbg = format!("{:?}", auth);
        assert!(dbg.contains("user"));
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("s3cret"));
    }

    #[test]
    fn proxy_policy_default_has_no_upstream() {
        let policy = ProxyPolicy::default();
        assert!(policy.upstream.is_none());
    }

    #[test]
    fn proxy_policy_patch_applies_upstream() {
        let mut policy = ProxyPolicy::default();
        let upstream = UpstreamProxyConfig {
            proxy_url: "http://corp:8080".to_string(),
            auth: None,
            bypass_hosts: vec![],
            fail_open: false,
        };
        policy.apply_patch(ProxyPolicyPatch {
            redaction: None,
            upstream: Some(upstream.clone()),
        });
        assert_eq!(policy.upstream, Some(upstream));
    }

    #[test]
    fn upstream_proxy_config_serde_masks_password() {
        let cfg = UpstreamProxyConfig {
            proxy_url: "https://secure-proxy:8443".to_string(),
            auth: Some(UpstreamAuth {
                username: "admin".to_string(),
                password: SecretString::new("p@ss".to_string().into()),
            }),
            bypass_hosts: vec!["cidr:10.0.0.0/8".to_string(), "*.internal".to_string()],
            fail_open: true,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        // Serialized JSON must NOT contain the real password.
        assert!(!json.contains("p@ss"));
        assert!(json.contains("***"));
        // Non-password fields round-trip correctly.
        let decoded: UpstreamProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.proxy_url, cfg.proxy_url);
        assert_eq!(decoded.bypass_hosts, cfg.bypass_hosts);
        assert_eq!(decoded.fail_open, cfg.fail_open);
    }

    #[test]
    fn proxy_policy_serialization_hides_upstream_when_none() {
        let policy = ProxyPolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        assert!(!json.contains("upstream"));
    }

    #[test]
    fn upstream_partial_eq_compares_password() {
        let a = UpstreamProxyConfig {
            proxy_url: "http://p:8080".to_string(),
            auth: Some(UpstreamAuth {
                username: "u".to_string(),
                password: SecretString::new("a".to_string().into()),
            }),
            bypass_hosts: vec![],
            fail_open: false,
        };
        let b = UpstreamProxyConfig {
            proxy_url: "http://p:8080".to_string(),
            auth: Some(UpstreamAuth {
                username: "u".to_string(),
                password: SecretString::new("b".to_string().into()),
            }),
            bypass_hosts: vec![],
            fail_open: false,
        };
        assert_ne!(a, b);
    }
}
