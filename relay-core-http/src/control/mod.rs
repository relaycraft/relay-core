//! Control-plane contract shared by the daemon and its clients.
//!
//! See decision [`0007`](../../../docs/decisions/0007-daemon-control-plane.md).

pub mod bootstrap;
pub mod client;
pub mod config;
pub mod manifest;

/// Default port of the daemon's control API (and of the Web UI it can serve).
pub const DEFAULT_API_PORT: u16 = 8082;
/// Default port of the MCP endpoint the daemon serves.
pub const DEFAULT_MCP_PORT: u16 = 18083;

pub use bootstrap::{
    BootstrapError, HOST_BINARY_NAME, SpawnRequest, find_host_binary, spawn_detached,
    wait_until_ready,
};
pub use client::{ApiVersionInfo, ControlClient, ControlClientError, DaemonStatus, connect};
pub use config::{
    CONFIG_FILE_NAME, ClientSection, ConfigError, DaemonSection, ProxySection, RelayConfig,
    config_path,
};
pub use manifest::{
    CONTROL_API_VERSION, DaemonLock, DaemonManifest, Discovery, LOCK_FILE_NAME, LockError,
    MANIFEST_FILE_NAME, daemon_lock_path, discover, load, manifest_path, process_alive,
    remove_manifest, write_manifest,
};
