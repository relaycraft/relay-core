//! Daemon registry: how a client finds the one daemon that owns the engine, and how that
//! daemon claims exclusive ownership of the data directory.
//!
//! The contract this module implements is decision [`0007`](../../../docs/decisions/0007-daemon-control-plane.md):
//! the engine has exactly one owner per data directory, client processes never start a second
//! one, and "no daemon" is a distinguishable state rather than an empty flow list.

use serde::{Deserialize, Serialize};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Version of the control protocol a manifest speaks.
///
/// A client must refuse to drive a daemon that reports a different value instead of guessing:
/// the failure mode of guessing is a surface that answers with stale or empty data, which is
/// exactly what this design exists to remove.
pub const CONTROL_API_VERSION: &str = "1";

pub const MANIFEST_FILE_NAME: &str = "daemon.json";
pub const LOCK_FILE_NAME: &str = "daemon.lock";

/// Everything a client needs to reach the daemon that owns the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonManifest {
    pub pid: u32,
    /// Control protocol version; compared against [`CONTROL_API_VERSION`] on discovery.
    pub api_version: String,
    /// Engine version, for diagnostics only — never for compatibility decisions.
    pub engine_version: String,
    /// Loopback port serving the control API.
    pub api_port: u16,
    /// Loopback port of the running proxy, when one is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<u16>,
    /// Loopback port serving the MCP endpoint, when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_port: Option<u16>,
    /// Whether the daemon serves the Web UI on its control API port.
    #[serde(default)]
    pub serve_webui: bool,
    /// Bearer token required by the control API. The manifest file is owner-only because of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub started_at_ms: u64,
    /// Data directory this daemon owns, so diagnostics can answer "which daemon is this".
    pub data_dir: PathBuf,
}

impl DaemonManifest {
    /// Base URL of the control API.
    pub fn control_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.api_port)
    }

    /// URL to open in a browser, with the token in the fragment.
    ///
    /// The fragment is the one part of a URL a browser never sends to a server, so the token does
    /// not end up in request logs. `None` when this daemon serves no Web UI.
    pub fn webui_url(&self) -> Option<String> {
        if !self.serve_webui {
            return None;
        }
        Some(match self.token.as_deref() {
            Some(token) => format!("{}/#token={token}", self.control_base_url()),
            None => format!("{}/", self.control_base_url()),
        })
    }
}

/// Outcome of looking for a daemon in a data directory.
///
/// `Absent` and `Stale` both mean "start one"; they are separate because only one of them
/// means the previous owner died without cleaning up. `Incompatible` means something *is*
/// running and must not be silently ignored or driven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Absent,
    Stale,
    Incompatible { found: String, expected: String },
    Running(DaemonManifest),
}

pub fn manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MANIFEST_FILE_NAME)
}

pub fn daemon_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOCK_FILE_NAME)
}

/// Read a manifest without judging whether its process is alive.
///
/// Returns `None` for a missing or unparseable file: a torn or hand-edited manifest carries no
/// authority, and the caller's next step (start a daemon) is the same either way.
pub fn load(data_dir: &Path) -> Option<DaemonManifest> {
    let raw = std::fs::read_to_string(manifest_path(data_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write the manifest atomically and owner-only.
///
/// Atomic because readers race with the daemon's own startup: a partially written manifest
/// would be parsed as a corrupt one, and "corrupt" and "absent" are indistinguishable to a
/// client that then decides to start a competing daemon.
pub fn write_manifest(data_dir: &Path, manifest: &DaemonManifest) -> io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;

    let final_path = manifest_path(data_dir);
    let tmp_path = data_dir.join(format!("{MANIFEST_FILE_NAME}.{}.tmp", std::process::id()));

    // Remove leftovers from a previous crash so create_new is a real exclusivity check.
    let _ = std::fs::remove_file(&tmp_path);
    let mut file = create_owner_only(&tmp_path)?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);

    // std::fs::rename replaces the destination on both Unix and Windows.
    std::fs::rename(&tmp_path, &final_path)
}

pub fn remove_manifest(data_dir: &Path) {
    let _ = std::fs::remove_file(manifest_path(data_dir));
}

/// Find the daemon owning `data_dir`.
///
/// Side effect: a manifest whose process is gone is removed, because leaving it behind would
/// make the next `start` believe an engine exists.
pub fn discover(data_dir: &Path) -> Discovery {
    let path = manifest_path(data_dir);
    if !path.exists() {
        return Discovery::Absent;
    }

    let Some(manifest) = load(data_dir) else {
        let _ = std::fs::remove_file(&path);
        return Discovery::Stale;
    };

    if !process_alive(manifest.pid) {
        let _ = std::fs::remove_file(&path);
        return Discovery::Stale;
    }

    // Checked after liveness on purpose: an incompatible manifest belonging to a *live* process
    // is that process's only record of itself, and deleting it would strand a running daemon.
    if manifest.api_version != CONTROL_API_VERSION {
        return Discovery::Incompatible {
            found: manifest.api_version,
            expected: CONTROL_API_VERSION.to_string(),
        };
    }

    Discovery::Running(manifest)
}

/// Whether `pid` names a live process this user can signal.
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs only the existence and permission check; no signal is delivered.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM means the process exists but belongs to another user.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether `pid` names a live process.
///
/// Windows reports `true` unconditionally: the check would need process-handle APIs, and the
/// cost of being wrong is bounded — discovery falls back to the control-API health check, which
/// reclaims a stale manifest on the next `start`.
#[cfg(not(unix))]
pub fn process_alive(pid: u32) -> bool {
    pid != 0
}

#[cfg(unix)]
fn create_owner_only(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Exclusive claim on a data directory by one daemon process.
///
/// Ownership is an **OS file lock**, not the pid written in the file. A pid file alone cannot be
/// made race-free: `create_new` proves only that the file did not exist yet, so two daemons
/// starting at the same instant can both "win" — the loser reads a file whose pid has not been
/// written yet, concludes the owner is dead, and reclaims it. That is not a theoretical race; it
/// reproduces every time two `relay start` commands run together.
///
/// With a real lock the kernel decides: a second daemon's `try_lock` fails immediately, and a
/// crashed daemon's lock is released by the OS rather than by pid-guessing. The pid stays in the
/// file for diagnostics only.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    pid: u32,
    /// Held for the process lifetime; dropping it releases the lock.
    file: std::fs::File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// Another live process owns the data directory.
    AlreadyRunning {
        pid: u32,
    },
    Io(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyRunning { pid } => match pid {
                0 => write!(
                    f,
                    "another RelayCore daemon already owns this data directory"
                ),
                pid => write!(
                    f,
                    "another RelayCore daemon (pid {pid}) already owns this data directory"
                ),
            },
            LockError::Io(msg) => write!(f, "daemon lock error: {msg}"),
        }
    }
}

impl std::error::Error for LockError {}

impl DaemonLock {
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        std::fs::create_dir_all(data_dir).map_err(|e| LockError::Io(e.to_string()))?;
        let path = daemon_lock_path(data_dir);
        let pid = std::process::id();

        let mut file = open_lock_file(&path)
            .map_err(|e| LockError::Io(format!("opening {}: {e}", path.display())))?;

        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                // The pid is a courtesy for the message; the lock is the authority.
                return Err(LockError::AlreadyRunning {
                    pid: read_lock_pid(&path).unwrap_or(0),
                });
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(LockError::Io(format!(
                    "locking {}: {error}",
                    path.display()
                )));
            }
        }

        write_lock(&mut file, pid).map_err(|e| LockError::Io(e.to_string()))?;
        Ok(Self { path, pid, file })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Removed while the lock is still held: a process that opened this file before the unlink
        // sees it locked and reports "already running", and one that opens it after gets a fresh
        // file — by which point this daemon has finished its cleanup and owns nothing.
        if read_lock_pid(&self.path) == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
        let _ = self.file.unlock();
    }
}

fn open_lock_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options.open(path)?;

    // A lock file created by an earlier version (or by a different umask) may be group- or
    // world-readable; it holds a pid, so tighten it while we are the one holding the lock.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }

    Ok(file)
}

fn write_lock(file: &mut std::fs::File, pid: u32) -> io::Result<()> {
    file.set_len(0)?;
    file.write_all(pid.to_string().as_bytes())?;
    file.flush()
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
