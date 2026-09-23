use crate::body_plan::BodyObservation;
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ProxyPolicyPatch {
    #[serde(default)]
    pub redaction: Option<RedactionPolicyPatch>,
    #[serde(default)]
    pub upstream: Option<UpstreamProxyConfig>,
    /// Storage bounds to change. Absent fields stay as they are; `null` removes that bound.
    #[serde(default)]
    pub retention: Option<RetentionPolicyPatch>,
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
            // On by default. It was off, which meant `Authorization`, `Cookie` and API keys were
            // persisted verbatim and served verbatim — to any consumer, including an agent reading
            // traffic over MCP. A default that leaks credentials the moment someone connects is not a
            // neutral default, and the cost of the alternative is bounded: header and query names are
            // matched against a fixed list, so nothing is guessed at and no body is touched.
            enabled: true,
            sensitive_header_names: default_sensitive_header_names(),
            sensitive_query_keys: default_sensitive_query_keys(),
            // Bodies stay opt-in: redacting them changes payload content, which rules and scripts
            // may legitimately need to see, so that choice belongs to the operator.
            redact_bodies: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPolicy {
    /// In strict mode, invalid method/status does not silently rewrite to GET/200.
    #[serde(default = "default_true")]
    pub strict_http_semantics: bool,

    /// Allow fallback to GET for invalid methods (only if strict_http_semantics is false).
    ///
    /// **Reserved: no code path reads this yet.** Setting it has no effect today, and the matching
    /// `relay_core_proxy_invalid_method_total` metric was removed rather than exported at zero.
    #[serde(default = "default_false")]
    pub allow_fallback_method: bool,

    /// Allow fallback to 200 OK for invalid status (only if strict_http_semantics is false)
    #[serde(default = "default_false")]
    pub allow_fallback_status: bool,

    /// Enable automatic retries for idempotent requests.
    ///
    /// **Reserved: no retry implementation exists.** No code path reads this, `max_retries` or
    /// `retry_idempotent_only`, and the matching `relay_core_proxy_retry_total` metric was removed
    /// rather than exported at zero.
    #[serde(default = "default_false")]
    pub enable_retry: bool,

    /// Only retry idempotent methods (GET, HEAD, OPTIONS). Reserved: see `enable_retry`.
    #[serde(default = "default_true")]
    pub retry_idempotent_only: bool,

    /// Maximum number of retries. Reserved: see `enable_retry`.
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

    /// How much of each body to keep **on the live Flow** for observation when no rule, script or
    /// breakpoint inspects it.
    ///
    /// Explicit because inferring it from "is there a body-stage rule?" silently changes which
    /// bodies a host can read back off the flow.
    ///
    /// - `off` (default): keep only what inspecting consumers force. The tap path still streams
    ///   `FlowUpdate::HttpBody` to the UI/store, so display is unaffected, and bodies keep streaming.
    /// - `prefixed`: additionally record a bounded prefix on the live flow. Note that the
    ///   response-direction plan has to decide before the body is read, so `prefixed` and `full`
    ///   both materialize the response body up to the budget.
    /// - `full`: read the whole body up to the budget, unconditionally.
    #[serde(default)]
    pub body_observation: BodyObservation,

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

    /// Upstream circuit breaker. It exists to stop hammering a genuinely dead upstream, but its
    /// defaults decide how a *transient* upstream blip is amplified, so they are policy rather than
    /// constants — see `docs/decisions/0005`.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerPolicy,

    /// Requests to these endpoints are **forwarded but not captured**.
    ///
    /// An engine that is proxied by its own host — a desktop app whose UI calls its own API through
    /// the system proxy, for instance — otherwise fills the flow list with its own internal traffic,
    /// which is noise for the person reading it. Entries are `host:port` (or `host`, meaning any
    /// port); empty by default so no existing deployment changes behaviour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_exclude: Vec<String>,

    /// How much history the store may keep. The default keeps 5 000 flows and drops anything older
    /// than 7 days. Every field `None` means unbounded, which a host must set explicitly — a
    /// long-running proxy otherwise grows until the disk fills.
    ///
    /// It lives in the policy rather than behind a host-specific command because every host already
    /// sets policy, and because a bound a user cannot reach is not a bound at all.
    #[serde(default)]
    pub retention: RetentionPolicy,
}

/// When to stop sending requests to an upstream that keeps failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CircuitBreakerPolicy {
    /// Consecutive failures that open the circuit. `0` disables the breaker entirely.
    ///
    /// "Consecutive" is literal: any success resets the count, so this is a burst threshold rather
    /// than a failure *rate*. A host that fails intermittently between successes never opens.
    #[serde(default = "default_circuit_failure_threshold")]
    pub failure_threshold: u32,
    /// How long to reject requests once the circuit opens, in milliseconds.
    ///
    /// This is the amplification factor: every rejection during the window is a request that was
    /// never attempted, so a long backoff turns a brief upstream hiccup into a sustained outage.
    #[serde(default = "default_circuit_backoff_ms")]
    pub backoff_ms: u64,
}

impl Default for CircuitBreakerPolicy {
    fn default() -> Self {
        Self {
            failure_threshold: default_circuit_failure_threshold(),
            backoff_ms: default_circuit_backoff_ms(),
        }
    }
}

fn default_circuit_failure_threshold() -> u32 {
    10
}

fn default_circuit_backoff_ms() -> u64 {
    5_000
}

/// Flows kept when a host has not chosen its own count bound.
pub const DEFAULT_MAX_FLOWS: usize = 5_000;
/// Seconds of history kept when a host has not chosen its own age bound (7 days).
pub const DEFAULT_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// Storage bounds, mirroring the store's own policy so a host can set them without depending on the
/// storage crate.
///
/// [`Default`] is the product bound (5 000 flows, 7 days). An all-`None` value is unbounded and is
/// not the default: deserializing a present but empty `retention` object yields that, because each
/// field defaults to `None` on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Keep at most this many flows and summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_flows: Option<usize>,
    /// Drop flows older than this many seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_secs: Option<u64>,
    /// Keep at most this many audit events. Bounded separately: audit is a compliance record and
    /// should not be evicted by traffic history. Unbounded unless a host sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_audit_events: Option<usize>,
}

/// A partial retention update. Each field is tri-state: absent leaves the current bound, `null`
/// removes it, and a number sets it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPolicyPatch {
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub max_flows: Option<Option<usize>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub max_age_secs: Option<Option<u64>>,
    #[serde(default, deserialize_with = "deserialize_tri_state")]
    pub max_audit_events: Option<Option<usize>>,
}

/// `null` is "clear this bound", not "field was absent". Absent fields never reach here: they take
/// [`Default`] via `serde(default)`.
fn deserialize_tri_state<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_flows: Some(DEFAULT_MAX_FLOWS),
            max_age_secs: Some(DEFAULT_MAX_AGE_SECS),
            max_audit_events: None,
        }
    }
}

impl RetentionPolicy {
    /// Keep every captured flow and every audit event.
    pub const fn unbounded() -> Self {
        Self {
            max_flows: None,
            max_age_secs: None,
            max_audit_events: None,
        }
    }

    /// Does this policy bound anything at all?
    pub const fn is_unbounded(&self) -> bool {
        self.max_flows.is_none() && self.max_age_secs.is_none() && self.max_audit_events.is_none()
    }
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
            body_observation: BodyObservation::Off,
            request_timeout_ms: 30_000, // 30 seconds
            transparent_enabled: false,
            transparent_require_original_dst: true,
            transparent_allow_host_fallback: false,
            transparent_reject_loopback_target: true,
            transparent_log_level: TransparentLogLevel::Info,
            quic_mode: QuicMode::Downgrade,
            quic_downgrade_clear_cache: false,
            redaction: RedactionPolicy::default(),
            upstream: None,
            retention: RetentionPolicy::default(),
            capture_exclude: Vec::new(),
            circuit_breaker: CircuitBreakerPolicy::default(),
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
        if let Some(retention) = patch.retention {
            if let Some(max_flows) = retention.max_flows {
                self.retention.max_flows = max_flows;
            }
            if let Some(max_age_secs) = retention.max_age_secs {
                self.retention.max_age_secs = max_age_secs;
            }
            if let Some(max_audit_events) = retention.max_audit_events {
                self.retention.max_audit_events = max_audit_events;
            }
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
    use super::ProxyPolicyPatch;

    #[test]
    fn proxy_policy_patch_rejects_unknown_fields() {
        let err = serde_json::from_value::<ProxyPolicyPatch>(serde_json::json!({
            "request_timeout_ms": 35000
        }))
        .expect_err("unknown policy patch fields must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("request_timeout_ms"),
            "error should name the rejected field, got {message}"
        );
    }

    #[test]
    fn default_retention_keeps_five_thousand_flows_for_seven_days() {
        let retention = super::RetentionPolicy::default();
        assert_eq!(retention.max_flows, Some(super::DEFAULT_MAX_FLOWS));
        assert_eq!(retention.max_age_secs, Some(super::DEFAULT_MAX_AGE_SECS));
        assert!(retention.max_audit_events.is_none());
        assert!(!retention.is_unbounded());
        assert!(super::RetentionPolicy::unbounded().is_unbounded());
    }

    #[test]
    fn retention_patch_changes_only_the_named_bound() {
        let mut policy = super::ProxyPolicy::default();
        let age = policy.retention.max_age_secs;
        policy.apply_patch(ProxyPolicyPatch {
            retention: Some(super::RetentionPolicyPatch {
                max_flows: Some(Some(10)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(policy.retention.max_flows, Some(10));
        assert_eq!(policy.retention.max_age_secs, age);

        policy.apply_patch(ProxyPolicyPatch {
            retention: Some(super::RetentionPolicyPatch {
                max_flows: Some(None),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(policy.retention.max_flows, None);
        assert_eq!(policy.retention.max_age_secs, age);
    }

    #[test]
    fn retention_patch_json_null_clears_only_that_bound() {
        let patch: ProxyPolicyPatch = serde_json::from_value(serde_json::json!({
            "retention": { "max_flows": null }
        }))
        .expect("null is a clear, not an unknown field");
        let retention = patch.retention.expect("retention was present");
        assert_eq!(
            retention.max_flows,
            Some(None),
            "null must be distinguishable from an omitted field"
        );
        assert_eq!(retention.max_age_secs, None);

        let mut policy = super::ProxyPolicy::default();
        policy.apply_patch(patch);
        assert_eq!(policy.retention.max_flows, None);
        assert_eq!(
            policy.retention.max_age_secs,
            Some(super::DEFAULT_MAX_AGE_SECS)
        );
    }

    /// The default policy must not make the engine copy bodies it has no reason to keep.
    ///
    /// Caught a real regression: defaulting observation to a mode that buffers materialized every
    /// response body before forwarding, silently trading away streaming for every exchange.
    #[test]
    fn default_policy_plan_does_not_copy_bodies() {
        use crate::body_plan::{BodyPlan, BodyPlanInputs, decide};

        let policy = ProxyPolicy::default();
        assert_eq!(
            policy.body_observation,
            BodyObservation::Off,
            "the default must not retain bodies nobody asked to observe"
        );

        let plan = decide(BodyPlanInputs {
            has_body_stage_rules: false,
            has_body_hook_script: false,
            has_body_intercept: false,
            observation: policy.body_observation,
            budget: policy.rule_body_inspect_budget,
        });

        assert_eq!(
            plan,
            BodyPlan::PassThrough,
            "with no consumer, a body must stream untouched"
        );
    }

    /// Observation is a deliberate choice, and asking for it does change the plan.
    #[test]
    fn prefixed_observation_captures_without_materializing() {
        use crate::body_plan::{BodyObservation, BodyPlan, BodyPlanInputs, decide};

        let policy = ProxyPolicy::default();
        for (mode, expected) in [
            (
                BodyObservation::Prefixed,
                BodyPlan::Capture {
                    limit: policy.rule_body_inspect_budget,
                },
            ),
            (
                BodyObservation::Full,
                BodyPlan::Buffer {
                    limit: policy.rule_body_inspect_budget,
                },
            ),
            (BodyObservation::Off, BodyPlan::PassThrough),
        ] {
            let plan = decide(BodyPlanInputs {
                has_body_stage_rules: false,
                has_body_hook_script: false,
                has_body_intercept: false,
                observation: mode,
                budget: policy.rule_body_inspect_budget,
            });
            assert_eq!(plan, expected, "unexpected plan for {mode:?}");
        }
    }

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
            retention: None,
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

/// Should a request to `url` be left out of the capture?
///
/// A pure function so the rule is testable without standing up a proxy, and so the hosts that set the
/// list and the pump that applies it cannot disagree about what an entry means.
///
/// An entry is `host:port` for one endpoint, or a bare `host` for any port on it. Matching is on the
/// host as written — `127.0.0.1` and `localhost` are different endpoints, and aliasing them would
/// hide traffic the operator did not ask to hide. A URL that cannot be parsed is not excluded: the
/// safe failure for a capture filter is to keep capturing.
pub fn is_capture_excluded(url: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    is_capture_excluded_parts(host, parsed.port_or_known_default(), patterns)
}

/// Loopback names for a port this process itself is listening on.
pub fn proxy_listen_endpoints(port: u16) -> [String; 2] {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
}

/// Point the capture-exclude list at the port the proxy is actually listening on.
///
/// `previous` is the port this process last installed, so stopping or moving the proxy drops that
/// port and leaves the control API, MCP, and any entry the operator added themselves.
pub fn replace_proxy_capture_exclude(
    exclude: &mut Vec<String>,
    previous: Option<u16>,
    listening: Option<u16>,
) {
    if let Some(port) = previous {
        let owned = proxy_listen_endpoints(port);
        exclude.retain(|entry| !owned.iter().any(|endpoint| endpoint == entry));
    }
    if let Some(port) = listening {
        for endpoint in proxy_listen_endpoints(port) {
            if !exclude.iter().any(|entry| entry == &endpoint) {
                exclude.push(endpoint);
            }
        }
    }
}

/// The same rule, for a caller that already holds a parsed URL.
///
/// The pump runs this for every flow update, and turning the URL back into a string so that it can be
/// parsed again was pure overhead on the hot path.
pub fn is_capture_excluded_parts(host: &str, port: Option<u16>, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }

    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        match pattern.rsplit_once(':') {
            // `host:port` — the port must match too, so excluding a UI on :8082 cannot silence an
            // unrelated service on :8083 of the same host.
            Some((pattern_host, pattern_port)) => {
                pattern_host.eq_ignore_ascii_case(host) && pattern_port.parse::<u16>().ok() == port
            }
            None => pattern.eq_ignore_ascii_case(host),
        }
    })
}

#[cfg(test)]
mod capture_exclude_tests {
    use super::{is_capture_excluded, replace_proxy_capture_exclude};

    fn patterns(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn an_exact_endpoint_is_excluded() {
        let list = patterns(&["127.0.0.1:8082"]);
        assert!(is_capture_excluded(
            "http://127.0.0.1:8082/api/v1/flows",
            &list
        ));
        assert!(
            !is_capture_excluded("http://127.0.0.1:8083/api/v1/flows", &list),
            "only the named port is excluded"
        );
        assert!(
            !is_capture_excluded("http://127.0.0.1/api/v1/flows", &list),
            "a different port on the same host is not the named endpoint"
        );
    }

    #[test]
    fn a_bare_host_excludes_every_port_on_it() {
        let list = patterns(&["localhost"]);
        assert!(is_capture_excluded("http://localhost:8082/x", &list));
        assert!(is_capture_excluded("https://localhost/x", &list));
        assert!(!is_capture_excluded(
            "http://localhost.example.com:8082/x",
            &list
        ));
    }

    /// The daemon excludes its own loopback endpoints. A different port on the same address is a
    /// local service, and hiding it would make that service unusable as a test upstream.
    #[test]
    fn another_loopback_port_is_not_excluded_with_the_daemons_own_endpoints() {
        let list = patterns(&[
            "127.0.0.1:8082",
            "localhost:8082",
            "127.0.0.1:18083",
            "localhost:18083",
            "127.0.0.1:8080",
            "localhost:8080",
        ]);
        assert!(!is_capture_excluded("http://127.0.0.1:3000/health", &list));
        assert!(!is_capture_excluded("http://localhost:3000/health", &list));
        assert!(!is_capture_excluded("http://[::1]:3000/health", &list));
    }

    /// Hosts are not aliased: excluding the loopback address must not silently exclude `localhost`
    /// traffic, or a filter would hide more than it says.
    #[test]
    fn hosts_are_matched_as_written() {
        let list = patterns(&["127.0.0.1:8082"]);
        assert!(!is_capture_excluded("http://localhost:8082/x", &list));
    }

    #[test]
    fn an_empty_list_excludes_nothing() {
        assert!(!is_capture_excluded("http://127.0.0.1:8082/x", &[]));
        assert!(!is_capture_excluded(
            "http://127.0.0.1:8082/x",
            &patterns(&["", "  "])
        ),);
    }

    #[test]
    fn the_proxy_port_in_the_list_is_the_one_actually_listening() {
        let mut exclude = vec![
            "127.0.0.1:8082".to_string(),
            "localhost:8082".to_string(),
            "127.0.0.1:18083".to_string(),
            "example.com:8080".to_string(),
        ];

        replace_proxy_capture_exclude(&mut exclude, None, Some(18080));
        assert!(exclude.iter().any(|entry| entry == "127.0.0.1:18080"));
        assert!(exclude.iter().any(|entry| entry == "localhost:18080"));
        assert!(
            exclude.iter().all(|entry| entry != "127.0.0.1:8080"),
            "a port the proxy is not listening on stays capturable"
        );
        assert!(exclude.iter().any(|entry| entry == "127.0.0.1:8082"));
        assert!(exclude.iter().any(|entry| entry == "example.com:8080"));

        replace_proxy_capture_exclude(&mut exclude, Some(18080), None);
        assert!(exclude.iter().all(|entry| !entry.ends_with(":18080")));
        assert!(exclude.iter().any(|entry| entry == "127.0.0.1:8082"));
        assert!(exclude.iter().any(|entry| entry == "example.com:8080"));
    }

    /// A filter must not swallow traffic just because it cannot be understood.
    #[test]
    fn an_unparsable_url_is_not_excluded() {
        let list = patterns(&["127.0.0.1:8082"]);
        assert!(!is_capture_excluded("not a url", &list));
    }
}
