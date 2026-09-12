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
