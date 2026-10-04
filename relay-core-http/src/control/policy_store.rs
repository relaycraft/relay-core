//! Persisting the proxy policy across restarts.
//!
//! The policy is runtime state a user tunes through the HTTP API or MCP — `body_observation`,
//! redaction, upstream config, and the rest. Held only in memory, every daemon restart silently
//! reverted it to the built-in default, so a script or rewrite that depended on
//! `body_observation = full` "mysteriously" stopped working after a restart. This file is that
//! state's home: written whenever the policy changes, read at startup.
//!
//! It is machine-written state, not a hand-edited config: the whole struct is serialized, so a
//! round trip always decodes. Fields a host recomputes at startup (the capture-exclude list of its
//! own ports) are written like any other and simply overwritten on the next boot.

use relay_core_api::policy::ProxyPolicy;
use std::path::{Path, PathBuf};

pub const POLICY_FILE_NAME: &str = "policy.json";

#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    #[error("{path} is not valid policy JSON: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn policy_path(data_dir: &Path) -> PathBuf {
    data_dir.join(POLICY_FILE_NAME)
}

/// Load the persisted policy, or `None` when no file has been written.
///
/// Absence is normal: a host that never had a policy change simply starts from the default. A file
/// that exists and cannot be understood is an error, not a silent fallback to defaults — running
/// with settings the user did not ask for is the failure mode persistence exists to remove.
pub fn load_policy(data_dir: &Path) -> Result<Option<ProxyPolicy>, PolicyStoreError> {
    let path = policy_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|source| PolicyStoreError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|source| PolicyStoreError::Json { path, source })
}

/// Write the policy atomically: a crash mid-write must not leave a truncated file that fails the
/// next start.
///
/// The upstream-proxy password is deliberately not written. It serializes as the `***` placeholder
/// the API shows clients, so persisting the struct verbatim would reload `***` as the real password
/// and break upstream authentication. The password stays a flag/environment value supplied at each
/// start; everything else about the upstream config survives.
pub fn save_policy(data_dir: &Path, policy: &ProxyPolicy) -> Result<(), PolicyStoreError> {
    let path = policy_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PolicyStoreError::Write {
            path: path.clone(),
            source,
        })?;
    }
    let mut policy = policy.clone();
    if let Some(upstream) = policy.upstream.as_mut() {
        upstream.auth = None;
    }
    let json = serde_json::to_string_pretty(&policy).map_err(|source| PolicyStoreError::Json {
        path: path.clone(),
        source,
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|source| PolicyStoreError::Write {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, &path).map_err(|source| PolicyStoreError::Write { path, source })
}

#[cfg(test)]
mod tests {
    use super::{POLICY_FILE_NAME, load_policy, policy_path, save_policy};
    use relay_core_api::body_plan::BodyObservation;
    use relay_core_api::policy::{ProxyPolicy, RedactionPolicy};

    #[test]
    fn an_unwritten_policy_loads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_policy(dir.path()).expect("load").is_none());
    }

    /// The whole point: a tuned policy must come back unchanged, so a restart cannot revert
    /// `body_observation` to the host's startup default or turn redaction back on.
    #[test]
    fn a_tuned_policy_survives_a_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = ProxyPolicy {
            body_observation: BodyObservation::Full,
            redaction: RedactionPolicy {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };

        save_policy(dir.path(), &policy).expect("save");
        let loaded = load_policy(dir.path())
            .expect("load")
            .expect("a saved policy must be found");

        assert_eq!(loaded.body_observation, BodyObservation::Full);
        assert!(!loaded.redaction.enabled);
    }

    /// The upstream password is not serializable (it becomes `***`), so it must not be written at
    /// all: reloading the placeholder as a real password would break upstream auth. The rest of the
    /// upstream config still persists.
    #[test]
    fn the_upstream_password_is_never_written() {
        use relay_core_api::policy::{UpstreamAuth, UpstreamProxyConfig};

        let dir = tempfile::tempdir().expect("tempdir");
        let policy = ProxyPolicy {
            upstream: Some(UpstreamProxyConfig {
                proxy_url: "http://corp-proxy:8080".to_string(),
                auth: Some(UpstreamAuth::new("alice".to_string(), "s3cret".to_string())),
                bypass_hosts: vec!["*.internal".to_string()],
                fail_open: false,
            }),
            ..Default::default()
        };

        save_policy(dir.path(), &policy).expect("save");

        let raw = std::fs::read_to_string(policy_path(dir.path())).expect("read");
        assert!(!raw.contains("s3cret"), "the password must not reach disk");
        assert!(
            !raw.contains("***"),
            "the placeholder must not be written either"
        );

        let loaded = load_policy(dir.path())
            .expect("load")
            .expect("a saved policy must be found");
        let upstream = loaded.upstream.expect("upstream survives");
        assert!(upstream.auth.is_none(), "auth is not persisted");
        assert_eq!(upstream.proxy_url, "http://corp-proxy:8080");
        assert_eq!(upstream.bypass_hosts, vec!["*.internal".to_string()]);
    }

    #[test]
    fn a_malformed_policy_is_an_error_not_a_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(policy_path(dir.path()), b"{ not json").expect("write");

        assert!(load_policy(dir.path()).is_err());
        assert!(policy_path(dir.path()).ends_with(POLICY_FILE_NAME));
    }
}
