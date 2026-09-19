//! Contract tests for the daemon registry: manifest discovery and single-instance locking.
//!
//! These pin the properties that make "connecting a client never starts an engine" safe:
//! a manifest must be owner-only, a stale manifest must self-heal, an incompatible control
//! protocol must be reported rather than silently used, and two daemons must not be able to
//! own the same data directory.

use relay_core_http::control::{
    CONTROL_API_VERSION, DaemonLock, DaemonManifest, Discovery, LockError, daemon_lock_path,
    discover, load, manifest_path, write_manifest,
};
use std::path::Path;

/// A pid that no process can hold: above every platform's pid ceiling, so `kill(pid, 0)` fails.
const DEAD_PID: u32 = 999_999_999;

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn manifest_for_current_process(dir: &Path) -> DaemonManifest {
    DaemonManifest {
        pid: std::process::id(),
        api_version: CONTROL_API_VERSION.to_string(),
        engine_version: "0.0.0-test".to_string(),
        api_port: 18082,
        proxy_port: Some(18080),
        mcp_port: Some(18083),
        token: Some("test-token".to_string()),
        serve_webui: false,
        started_at_ms: 1_700_000_000_000,
        data_dir: dir.to_path_buf(),
    }
}

/// The browser needs the token, and the only part of a URL a browser never sends to a server is the
/// fragment — which is what keeps the secret out of request logs.
#[test]
fn the_browser_url_carries_the_token_in_the_fragment() {
    let dir = temp_dir();
    let manifest = DaemonManifest {
        serve_webui: true,
        ..manifest_for_current_process(dir.path())
    };
    write_manifest(dir.path(), &manifest).expect("write manifest");

    assert_eq!(
        manifest.webui_url().as_deref(),
        Some("http://127.0.0.1:18082/#token=test-token")
    );

    let without_ui = DaemonManifest {
        serve_webui: false,
        ..manifest
    };
    assert_eq!(without_ui.webui_url(), None);
}

#[test]
fn manifest_round_trips_through_disk() {
    let dir = temp_dir();
    let manifest = manifest_for_current_process(dir.path());

    write_manifest(dir.path(), &manifest).expect("write manifest");

    let loaded = load(dir.path()).expect("manifest should be readable");
    assert_eq!(loaded, manifest);
    assert_eq!(loaded.control_base_url(), "http://127.0.0.1:18082");
}

#[cfg(unix)]
#[test]
fn manifest_file_is_owner_readable_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir();
    write_manifest(dir.path(), &manifest_for_current_process(dir.path())).expect("write manifest");

    let mode = std::fs::metadata(manifest_path(dir.path()))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;

    // The manifest carries the control API bearer token. Any other readable bit hands local
    // control of the proxy to another user account.
    assert_eq!(mode, 0o600, "manifest must be 0600, found {mode:o}");
}

#[test]
fn discovery_reports_absent_when_no_manifest_exists() {
    let dir = temp_dir();
    assert_eq!(discover(dir.path()), Discovery::Absent);
}

#[test]
fn discovery_reclaims_a_manifest_whose_process_is_gone() {
    let dir = temp_dir();
    let manifest = DaemonManifest {
        pid: DEAD_PID,
        ..manifest_for_current_process(dir.path())
    };
    write_manifest(dir.path(), &manifest).expect("write manifest");

    assert_eq!(discover(dir.path()), Discovery::Stale);
    assert!(
        !manifest_path(dir.path()).exists(),
        "a stale manifest must be removed so the next start is not blocked by a corpse"
    );
}

#[test]
fn discovery_finds_a_manifest_owned_by_a_live_process() {
    let dir = temp_dir();
    let manifest = manifest_for_current_process(dir.path());
    write_manifest(dir.path(), &manifest).expect("write manifest");

    assert_eq!(discover(dir.path()), Discovery::Running(manifest));
}

#[test]
fn discovery_reports_an_incompatible_control_protocol() {
    let dir = temp_dir();
    let manifest = DaemonManifest {
        api_version: "999".to_string(),
        ..manifest_for_current_process(dir.path())
    };
    write_manifest(dir.path(), &manifest).expect("write manifest");

    match discover(dir.path()) {
        Discovery::Incompatible { found, expected } => {
            assert_eq!(found, "999");
            assert_eq!(expected, CONTROL_API_VERSION);
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
}

#[test]
fn lock_is_exclusive_and_blocks_a_second_owner() {
    let dir = temp_dir();
    let first = DaemonLock::acquire(dir.path()).expect("first lock");

    match DaemonLock::acquire(dir.path()) {
        Err(LockError::AlreadyRunning { pid }) => assert_eq!(pid, std::process::id()),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }

    drop(first);
    DaemonLock::acquire(dir.path()).expect("lock must be reacquirable after release");
}

#[test]
fn lock_reclaims_a_dead_owner_instead_of_blocking_forever() {
    let dir = temp_dir();
    std::fs::write(daemon_lock_path(dir.path()), DEAD_PID.to_string()).expect("write stale lock");

    DaemonLock::acquire(dir.path())
        .expect("a lock file left by a dead process must not block startup");
}

#[test]
fn released_lock_leaves_no_file_behind() {
    let dir = temp_dir();
    let lock = DaemonLock::acquire(dir.path()).expect("lock");
    assert!(daemon_lock_path(dir.path()).exists());

    drop(lock);

    assert!(
        !daemon_lock_path(dir.path()).exists(),
        "a released lock must not leave a file that looks like a running daemon"
    );
}

#[test]
fn writing_a_manifest_replaces_a_previous_one_atomically() {
    let dir = temp_dir();
    let first = manifest_for_current_process(dir.path());
    write_manifest(dir.path(), &first).expect("first write");

    let second = DaemonManifest {
        api_port: 18099,
        proxy_port: None,
        ..first
    };
    write_manifest(dir.path(), &second).expect("second write");

    assert_eq!(load(dir.path()).expect("manifest"), second);
}
