//! Internal persistence crate for [relay-core](https://crates.io/crates/relay-core).
//! SQLite-backed storage for rules, flows, and audit events.
//!
//! **This is not a user-facing crate.** Use `relay-core` instead.

pub mod error;
pub mod store;

#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// The database was written by a newer build than this one.
    #[error("Database schema version {found} is newer than the supported version {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    /// A recorded schema version has no migration registered.
    #[error("No migration registered for schema version {version}")]
    UnknownMigration { version: i64 },
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub fn init() {
    // tracing::info!("Relay Core Storage Initialized");
}

/// How much history the store is allowed to keep.
///
/// Nothing pruned the database before this existed: `flows`, `flow_summaries` and `audit_events` grew
/// for the life of the file, so a long-running instance would eventually exhaust the disk (roadmap
/// §24.8). Bounds are explicit and opt-in rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RetentionPolicy {
    /// Keep at most this many flows and summaries. `None` keeps everything.
    pub max_flows: Option<usize>,
    /// Drop flows older than this many seconds. `None` keeps everything.
    pub max_age_secs: Option<u64>,
    /// Keep at most this many audit events. Audit is a compliance record, so it is bounded
    /// separately from traffic history.
    pub max_audit_events: Option<usize>,
}

impl RetentionPolicy {
    /// A policy that keeps everything, which is the pre-existing behaviour made explicit.
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

/// What a prune pass removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrunedCounts {
    /// Flow rows deleted.
    pub flows: u64,
    /// Flow summary rows deleted.
    pub flow_summaries: u64,
    /// Audit event rows deleted.
    pub audit_events: u64,
}

impl PrunedCounts {
    /// Total rows removed.
    pub const fn total(&self) -> u64 {
        self.flows + self.flow_summaries + self.audit_events
    }
}

#[cfg(test)]
mod retention_tests {
    use super::RetentionPolicy;

    #[test]
    fn default_policy_is_unbounded_and_says_so() {
        let policy = RetentionPolicy::default();
        assert!(policy.is_unbounded());
        assert_eq!(policy, RetentionPolicy::unbounded());
    }

    #[test]
    fn any_bound_makes_the_policy_bounded() {
        for policy in [
            RetentionPolicy {
                max_flows: Some(1),
                ..Default::default()
            },
            RetentionPolicy {
                max_age_secs: Some(60),
                ..Default::default()
            },
            RetentionPolicy {
                max_audit_events: Some(1),
                ..Default::default()
            },
        ] {
            assert!(!policy.is_unbounded(), "{policy:?} should be bounded");
        }
    }
}
