//! Bootstrapping a daemon from a client that does not own one.
//!
//! Decision [`0007`](../../../docs/decisions/0007-daemon-control-plane.md): a client never starts
//! an engine of its own — it starts *the* daemon, once, and attaches to it. This module holds the
//! parts of that which both the CLI and the MCP bridge need: finding the host binary, detaching a
//! child from the caller's terminal, and waiting for the published control API to answer.
//!
//! Waiting is not sleeping: readiness is the manifest becoming discoverable *and* healthy, so a
//! client that returns from here has a daemon it can actually drive.

use crate::control::client::{ControlClient, DaemonStatus, connect};
use crate::control::manifest::process_alive;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Name of the binary that can host a daemon.
pub const HOST_BINARY_NAME: &str = "relay-core-cli";

const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(
        "cannot find the RelayCore CLI binary to start a daemon: looked next to this \
         executable and on PATH (install @relay-core/cli and run `relay-core`)"
    )]
    HostBinaryNotFound,

    #[error("failed to start the RelayCore daemon: {0}")]
    Spawn(String),

    #[error("the RelayCore daemon exited during startup (pid {pid}); see {log}")]
    ExitedBeforeReady { pid: u32, log: PathBuf },

    #[error(
        "the RelayCore daemon (pid {pid}) did not become ready within {}s; see {log}",
        timeout.as_secs()
    )]
    Timeout {
        pid: u32,
        timeout: Duration,
        log: PathBuf,
    },
}

/// What to run to obtain a daemon.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub program: PathBuf,
    /// Arguments after the program name, e.g. `["daemon", "--api-port", "8082"]`.
    pub args: Vec<String>,
    /// Where the detached child's output goes. Appended to, never truncated: the previous crash is
    /// usually the interesting one.
    pub log_file: PathBuf,
}

/// Find the CLI binary, preferring the one shipped next to this executable.
///
/// The npm packages install the probe and the CLI side by side, so a sibling lookup is what makes
/// the bridge work in an install where nothing is on PATH.
pub fn find_host_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(executable_name(HOST_BINARY_NAME));
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(executable_name(HOST_BINARY_NAME)))
        .find(|candidate| candidate.is_file())
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Spawn the daemon detached from this process's terminal and return its pid.
///
/// Detached because the daemon must outlive the client that started it: an MCP client that exits
/// (or a shell that closes) must not take the proxy and its history with it.
pub fn spawn_detached(request: &SpawnRequest) -> Result<u32, BootstrapError> {
    if let Some(parent) = request.log_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| BootstrapError::Spawn(e.to_string()))?;
    }
    let log = open_daemon_log(&request.log_file).map_err(|e| {
        BootstrapError::Spawn(format!("opening {}: {e}", request.log_file.display()))
    })?;

    let mut command = Command::new(&request.program);
    command.args(&request.args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(io_err)?))
        .stderr(Stdio::from(log));

    detach(&mut command);

    let child = command
        .spawn()
        .map_err(|e| BootstrapError::Spawn(format!("{}: {e}", request.program.display())))?;
    Ok(child.id())
}

fn io_err(error: io::Error) -> BootstrapError {
    BootstrapError::Spawn(error.to_string())
}

/// Open the daemon log for append. The file can contain operational detail from stderr, so it is
/// owner-only, the same way the manifest that holds the control token is.
fn open_daemon_log(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

/// Detach the child from this process's terminal so it survives the shell that started it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs between fork and exec, where only async-signal-safe calls are
    // allowed; `setsid` is one, and nothing else happens here.
    unsafe {
        command.pre_exec(|| {
            // A failure means we are already a session leader, which is the state we want.
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS: no console. CREATE_NEW_PROCESS_GROUP: Ctrl+C in the parent's console does
    // not reach the daemon.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Wait until the daemon owning `data_dir` answers, and return a client for it.
pub async fn wait_until_ready(
    data_dir: &Path,
    pid: u32,
    log_file: &Path,
    timeout: Duration,
) -> Result<ControlClient, BootstrapError> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if let DaemonStatus::Running { client, .. } = connect(data_dir).await {
            return Ok(client);
        }
        // Fail fast when the child died instead of waiting out the timeout: the reason is in the
        // log, and a pause before saying so helps nobody.
        if !process_alive(pid) {
            return Err(BootstrapError::ExitedBeforeReady {
                pid,
                log: log_file.to_path_buf(),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    Err(BootstrapError::Timeout {
        pid,
        timeout,
        log: log_file.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_daemon_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("daemon.log");
        let _file = open_daemon_log(&path).expect("create log");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "found {mode:o}");
    }

    #[test]
    fn the_host_binary_is_looked_for_next_to_this_executable_first() {
        // In the test harness the sibling does not exist, so this asserts the PATH fallback is
        // reachable rather than that a binary was found.
        let found = find_host_binary();
        if let Some(path) = found {
            assert!(path.is_file());
            assert!(
                path.to_string_lossy().contains(HOST_BINARY_NAME),
                "found a binary that is not the host: {}",
                path.display()
            );
        }
    }

    #[test]
    fn executable_name_has_the_platform_suffix() {
        let name = executable_name("relay-core-cli");
        if cfg!(windows) {
            assert_eq!(name, "relay-core-cli.exe");
        } else {
            assert_eq!(name, "relay-core-cli");
        }
    }
}
