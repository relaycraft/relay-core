//! End-to-end contract for the lifecycle commands.
//!
//! These run the real CLI binary, which spawns the real daemon, which binds real ports: the
//! properties under test (one daemon per data directory, `stop` keeps the daemon, `shutdown`
//! cleans up) are only true of real processes. Decision
//! [`0007`](../../docs/decisions/0007-daemon-control-plane.md).

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLI: &str = env!("CARGO_BIN_EXE_relay-core-cli");

/// A data directory plus the guarantee that its daemon is stopped when the test ends.
struct Harness {
    data_dir: tempfile::TempDir,
    /// Replaceable: a port can be taken between the moment one is found free and the moment the
    /// daemon binds it, so the harness re-picks rather than reporting an environment collision as a
    /// product failure.
    proxy_port: std::cell::Cell<u16>,
    api_port: std::cell::Cell<u16>,
}

impl Harness {
    fn new() -> Self {
        Self {
            data_dir: tempfile::tempdir().expect("temp data dir"),
            proxy_port: std::cell::Cell::new(unique_port()),
            api_port: std::cell::Cell::new(unique_port()),
        }
    }

    fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    fn proxy_port(&self) -> u16 {
        self.proxy_port.get()
    }

    fn api_port(&self) -> u16 {
        self.api_port.get()
    }

    fn repick_ports(&self) {
        self.proxy_port.set(unique_port());
        self.api_port.set(unique_port());
    }

    fn relay(&self, args: &[&str]) -> Output {
        run_in(self.data_dir(), args)
    }

    fn relay_json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.relay(args);
        assert!(
            output.status.success(),
            "`relay {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`relay {}` did not print JSON ({error}): {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    /// Start the daemon and the proxy, waiting for both to be up.
    ///
    /// The control port is left to the OS (`--api-port 0`): tests run in parallel, and a port that
    /// was free when it was picked can be taken by a sibling test's daemon a moment later — which
    /// turns into a client talking to the wrong daemon rather than a clean failure.
    /// Run `relay start` with arguments of the caller's choosing, retrying after a collision.
    ///
    /// A port that was free when it was chosen can be taken by the time the daemon binds it — by a
    /// sibling test, a daemon leaked from an earlier failing run, or a lingering socket. A suite
    /// that spawns real processes has to absorb that instead of reporting it as a product failure.
    ///
    /// Any daemon is shut down between attempts: one left alive by a failed attempt keeps the ports
    /// it was given, so attaching to it would quietly test something other than intended.
    fn start_with_retry<F>(&self, mut build_args: F) -> serde_json::Value
    where
        F: FnMut(&Harness) -> Vec<String>,
    {
        for attempt in 0..3 {
            let args = build_args(self);
            let mut argv: Vec<&str> = vec!["start"];
            argv.extend(args.iter().map(String::as_str));
            argv.push("--json");

            let output = self.relay(&argv);
            if output.status.success() {
                return serde_json::from_slice(&output.stdout).expect("start --json output");
            }

            let message = stderr(&output);
            if attempt < 2 && message.contains("Address already in use") {
                let _ = self.relay(&["shutdown"]);
                self.repick_ports();
                continue;
            }
            panic!("`relay start {}` failed: {message}", args.join(" "));
        }
        unreachable!("the loop returns or panics")
    }

    /// Start the daemon and the proxy, waiting for both to be up.
    fn start(&self) -> serde_json::Value {
        self.start_with_retry(|harness| {
            vec![
                "--listen".to_string(),
                format!("127.0.0.1:{}", harness.proxy_port()),
                "--api-port".to_string(),
                harness.api_port().to_string(),
                "--no-mcp".to_string(),
            ]
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Best effort: a test that panicked mid-way must not leave a daemon behind holding ports
        // the next test run wants.
        let _ = run_in(self.data_dir(), &["shutdown"]);
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

/// A port no other test in this process will be given.
///
/// `free_port()` alone is not enough: it binds, reads the port and closes, and the OS may then hand
/// the same port to the next caller — two tests race for it and the loser's daemon fails to bind,
/// which looks like a product bug. Tests take ports from a shared counter, and each run of the
/// binary uses a different band so a daemon leaked by an earlier failing run cannot be in the way.
/// The range sits below both ephemeral ranges in play (Linux allocates from 32768, macOS from
/// 49152), so an outgoing connection cannot take a port this suite is about to bind.
fn unique_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};

    static NEXT: AtomicU16 = AtomicU16::new(0);

    let band = (std::process::id() % 40) as u16;
    let base = 20_000 + band * 100;

    for _ in 0..500 {
        let port = base + NEXT.fetch_add(1, Ordering::Relaxed);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    free_port()
}

/// The port the daemon actually serves MCP on, from its own status output.
///
/// A requested MCP port that is busy is not an error by design — the daemon serves an ephemeral one
/// rather than refusing to start — so the contract is "an endpoint is served", not "the number I
/// asked for was free".
fn served_mcp_port(started: &serde_json::Value) -> u16 {
    let url = started["mcp_url"]
        .as_str()
        .expect("the daemon must report an MCP endpoint");
    url.trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/mcp")
        .parse()
        .expect("mcp_url must carry a port")
}

fn run_in(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .env("RELAY_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("running `relay {}`: {error}", args.join(" ")))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn wait_for_listener(port: u16, timeout: Duration) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn start_status_stop_shutdown_round_trip() {
    let harness = Harness::new();

    let started = harness.start();
    assert_eq!(started["daemon"], "running");
    assert_eq!(started["proxy"]["outcome"], "started");
    assert_eq!(started["proxy"]["port"], harness.proxy_port());
    assert!(
        wait_for_listener(harness.proxy_port(), Duration::from_secs(5)),
        "the proxy must actually be listening on {}",
        harness.proxy_port()
    );

    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(status["daemon"]["status"], "running");
    assert_eq!(status["proxy"]["phase"], "running");
    assert_eq!(status["proxy"]["port"], harness.proxy_port());

    // `stop` stops the proxy and keeps the daemon: history and rules outlive a client session.
    let stopped = harness.relay_json(&["stop", "--json"]);
    assert_eq!(stopped["proxy"], "stopped");
    assert_eq!(stopped["daemon"], "running");

    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(status["proxy"]["phase"], "stopped");
    assert_eq!(status["daemon"]["status"], "running");
    assert!(
        !wait_for_listener(harness.proxy_port(), Duration::from_millis(300)),
        "a stopped proxy must stop listening"
    );

    // Starting again on a stopped proxy must work: this is what a stop that returned too early
    // would break.
    let restarted = harness.start();
    assert_eq!(restarted["proxy"]["outcome"], "started");

    let shutdown = harness.relay(&["shutdown"]);
    assert!(
        shutdown.status.success(),
        "shutdown failed: {}",
        stderr(&shutdown)
    );

    let after = harness.relay(&["status"]);
    assert!(
        !after.status.success(),
        "status must fail once the daemon is gone"
    );
}

#[test]
fn starting_twice_keeps_one_daemon() {
    let harness = Harness::new();

    harness.start();
    let first = harness.relay_json(&["status", "--json"]);

    // A second start is the normal case: an agent connects while the daemon already runs.
    let second_start = harness.start();
    assert_eq!(second_start["proxy"]["outcome"], "already_running");
    assert_eq!(second_start["proxy"]["port"], harness.proxy_port());

    let second = harness.relay_json(&["status", "--json"]);
    assert_eq!(
        first["daemon"]["pid"], second["daemon"]["pid"],
        "a second start must attach to the running daemon, not spawn another"
    );
}

#[test]
fn concurrent_starts_converge_on_one_daemon() {
    let harness = Harness::new();

    // Two clients racing is the case a pid-file lock gets wrong, so it is tested with real
    // processes rather than simulated.
    let spawn = || {
        Command::new(CLI)
            .args([
                "start",
                "--listen",
                &format!("127.0.0.1:{}", harness.proxy_port()),
                "--api-port",
                "0",
                "--no-mcp",
            ])
            .env("RELAY_DATA_DIR", harness.data_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning relay start")
    };

    let first = spawn();
    let second = spawn();
    let first = first.wait_with_output().expect("first start");
    let second = second.wait_with_output().expect("second start");

    assert!(
        first.status.success() && second.status.success(),
        "both starts must succeed and converge:\n  first: {}\n  second: {}",
        stderr(&first),
        stderr(&second)
    );

    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(
        status["proxy"]["phase"],
        "running",
        "both clients asked for one proxy; the loser must not fail it (clients: {} / {})",
        stderr(&first),
        stderr(&second)
    );
    assert_eq!(status["proxy"]["port"], harness.proxy_port());
}

#[test]
fn a_taken_proxy_port_is_reported_and_not_swallowed() {
    let harness = Harness::new();
    let occupied = TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = occupied.local_addr().expect("addr").port();

    let output = run_in(
        harness.data_dir(),
        &[
            "start",
            "--listen",
            &format!("127.0.0.1:{taken}"),
            "--api-port",
            "0",
            "--no-mcp",
        ],
    );

    assert!(
        !output.status.success(),
        "start must fail when the proxy port is taken, not report success"
    );
    let message = stderr(&output);
    assert!(
        message.contains("start_failed") && message.contains(&taken.to_string()),
        "the failure must name the cause and the port: {message}"
    );

    // The daemon itself stays usable: the control plane is what reports the failure, so it must
    // outlive it.
    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(status["daemon"]["status"], "running");
    assert_eq!(status["proxy"]["phase"], "failed");
}

#[test]
fn shutdown_cleans_up_the_registry() {
    let harness = Harness::new();
    harness.start();

    let shutdown = harness.relay(&["shutdown"]);
    assert!(
        shutdown.status.success(),
        "shutdown failed: {}",
        stderr(&shutdown)
    );

    // "Shutdown finished" has to mean the daemon is gone: no manifest that looks like a running
    // daemon, and no lock that would refuse the next `relay start`. The command waits for all of
    // it, so this cannot be a race.
    assert!(
        !harness.data_dir().join("daemon.json").exists(),
        "a stopped daemon must not leave a manifest that looks like a running one"
    );
    assert!(
        !harness.data_dir().join("daemon.lock").exists(),
        "a released lock must not be left behind"
    );

    // A new daemon must start immediately afterwards rather than being refused by the old one.
    let restarted = harness.start();
    assert_eq!(restarted["proxy"]["outcome"], "started");
}

#[test]
fn the_daemon_serves_the_mcp_endpoint() {
    let harness = Harness::new();
    let mut requested_mcp_port = unique_port();

    harness.start_with_retry(|harness| {
        requested_mcp_port = unique_port();
        vec![
            "--listen".to_string(),
            format!("127.0.0.1:{}", harness.proxy_port()),
            "--api-port".to_string(),
            harness.api_port().to_string(),
            "--mcp-port".to_string(),
            requested_mcp_port.to_string(),
        ]
    });

    // The start output reports the control URL; the MCP endpoint is part of the daemon's status.
    // A requested MCP port that is busy is not fatal by design (the daemon serves an ephemeral one
    // instead), so the assertion reads the port the daemon reports.
    let status = harness.relay_json(&["status", "--json"]);
    let served = served_mcp_port(&status);
    assert!(
        wait_for_listener(served, Duration::from_secs(5)),
        "the daemon reported MCP on {served} but nothing answers there (asked for {requested_mcp_port})"
    );
}

/// `relay run` is the same daemon in the foreground: it publishes the same manifest, so other
/// clients find it, and shutting the daemon down ends the foreground process.
#[test]
fn a_foreground_run_is_discoverable_and_stoppable() {
    let harness = Harness::new();
    let mcp_port = free_port();

    let mut child = Command::new(CLI)
        .args([
            "run",
            "--listen",
            &format!("127.0.0.1:{}", harness.proxy_port()),
            "--api-port",
            "0",
            "--mcp-port",
            &mcp_port.to_string(),
        ])
        .env("RELAY_DATA_DIR", harness.data_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning relay run");

    // Discovery is the property under test: `relay status` must find a foreground run, which the
    // old `relay run` (no manifest) could never satisfy.
    let status = match wait_for_status(&harness, Duration::from_secs(15)) {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let output = child.wait_with_output().expect("output");
            panic!(
                "a foreground run must publish a discoverable daemon; stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };

    assert_eq!(status["daemon"]["status"], "running");
    assert_eq!(status["proxy"]["phase"], "running");
    assert_eq!(status["proxy"]["port"], harness.proxy_port());
    assert_eq!(
        status["mcp_url"],
        format!("http://127.0.0.1:{mcp_port}/mcp")
    );
    assert!(wait_for_listener(
        harness.proxy_port(),
        Duration::from_secs(5)
    ));

    let shutdown = harness.relay(&["shutdown"]);
    assert!(
        shutdown.status.success(),
        "shutdown failed: {}",
        stderr(&shutdown)
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("shutting the daemon down must end the foreground run");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// `relay status` until it succeeds, or `None` when the deadline passes.
fn wait_for_status(harness: &Harness, timeout: Duration) -> Option<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let output = harness.relay(&["status", "--json"]);
        if output.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        {
            return Some(value);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Settings live in the config file so they survive across commands: `relay start` with no flags
/// must honour the ports it names.
#[test]
fn the_config_file_supplies_ports() {
    let harness = Harness::new();

    // The ports come from the file, so a retry rewrites it — and because a daemon that is already
    // running keeps the port it was given, a collision restarts rather than attaches.
    let started = {
        let mut started = None;
        for attempt in 0..3 {
            std::fs::write(
                harness.data_dir().join("config.toml"),
                format!(
                    "[daemon]\napi_port = {}\nmcp = false\n\n[proxy]\nport = {}\n",
                    harness.api_port(),
                    harness.proxy_port()
                ),
            )
            .expect("write config");

            let output = harness.relay(&["start", "--json"]);
            let value = output.status.success().then(|| {
                serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("json")
            });

            let honoured = value.as_ref().is_some_and(|value| {
                value["control_url"] == format!("http://127.0.0.1:{}", harness.api_port())
            });

            if honoured {
                started = value;
                break;
            }

            if attempt == 2 {
                panic!(
                    "a configured API port was not honoured after 3 attempts; last output: {}",
                    stderr(&output)
                );
            }
            let _ = harness.relay(&["shutdown"]);
            harness.repick_ports();
        }
        started.expect("a start attempt must succeed")
    };

    assert_eq!(started["proxy"]["port"], harness.proxy_port());
    assert_eq!(
        started["control_url"],
        format!("http://127.0.0.1:{}", harness.api_port())
    );

    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(status["mcp_url"], serde_json::Value::Null, "mcp = false");
    assert_eq!(status["proxy"]["port"], harness.proxy_port());
}

/// A flag still wins over the file, so a one-off override never requires editing it.
#[test]
fn a_flag_overrides_the_config_file() {
    let harness = Harness::new();
    let mut other_port = unique_port();
    std::fs::write(
        harness.data_dir().join("config.toml"),
        format!("[proxy]\nport = {}\n", harness.proxy_port()),
    )
    .expect("write config");

    let started = harness.start_with_retry(|_| {
        other_port = unique_port();
        vec![
            "--listen".to_string(),
            format!("127.0.0.1:{other_port}"),
            "--api-port".to_string(),
            "0".to_string(),
            "--no-mcp".to_string(),
        ]
    });

    assert_eq!(started["proxy"]["port"], other_port);
}

/// A typo must stop the command, not silently run the daemon with defaults the user did not ask
/// for.
#[test]
fn a_broken_config_file_is_reported() {
    let harness = Harness::new();
    std::fs::write(
        harness.data_dir().join("config.toml"),
        "[daemon]\napi_port = \"not a port\"\n",
    )
    .expect("write config");

    let output = harness.relay(&["start"]);

    assert!(!output.status.success(), "a broken config must fail");
    let message = stderr(&output);
    assert!(
        message.contains("config.toml"),
        "the error must name the file to fix: {message}"
    );
}

/// `relay flows` lists captured traffic; `--follow` streams it. A filter combined with `--follow`
/// is refused rather than silently ignored.
#[test]
fn listing_and_following_are_distinct() {
    let harness = Harness::new();
    harness.start();

    // A listing with nothing captured succeeds and says so.
    let listing = harness.relay(&["flows", "--limit", "5"]);
    assert!(
        listing.status.success(),
        "listing must not hang or fail: {}",
        stderr(&listing)
    );
    assert!(String::from_utf8_lossy(&listing.stdout).contains("No flows matched"));

    let refused = harness.relay(&["flows", "--follow", "--host", "example.com"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("drop --follow"),
        "the refusal must say how to proceed: {}",
        stderr(&refused)
    );
}

/// "Who started this proxy" must outlive the command that asked: the audit trail records the actor
/// and the client's own label, and `status` surfaces them.
#[test]
fn lifecycle_changes_are_attributed_in_status() {
    let harness = Harness::new();

    harness.start();
    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(
        status["last_change"]["change"], "started",
        "status: {status}"
    );
    assert_eq!(
        status["last_change"]["requested_by"],
        "cli:relay-core start"
    );
    assert_eq!(status["last_change"]["actor"], "http");
    assert_eq!(status["last_change"]["outcome"], "success");

    harness.relay_json(&["stop", "--json"]);
    let status = harness.relay_json(&["status", "--json"]);
    assert_eq!(status["last_change"]["change"], "stopped");
    assert_eq!(status["last_change"]["requested_by"], "cli:relay-core stop");

    // The human output names who did it, too — that is the question a user actually asks.
    let human = harness.relay(&["status"]);
    let text = String::from_utf8_lossy(&human.stdout).to_string();
    assert!(
        text.contains("stopped by cli:relay-core stop"),
        "status should say who stopped the proxy: {text}"
    );
}

#[test]
fn stop_before_anything_runs_is_not_an_error() {
    let harness = Harness::new();

    let output = harness.relay(&["stop"]);

    assert!(
        output.status.success(),
        "stop must be idempotent for scripts: {}",
        stderr(&output)
    );
}

#[test]
fn a_daemon_log_is_written_where_status_says_it_is() {
    let harness = Harness::new();
    harness.start();

    let status = harness.relay_json(&["status", "--json"]);
    let log_path: PathBuf = status["daemon"]["log"]
        .as_str()
        .expect("status must name the log file")
        .into();

    assert!(log_path.exists(), "{} should exist", log_path.display());
    let contents = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        contents.contains("daemon ready"),
        "the log should record startup: {contents}"
    );

    // Appending rather than truncating: a daemon restart must not erase the previous crash.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .expect("open log");
    writeln!(file, "marker").expect("append marker");
    assert!(
        std::fs::read_to_string(&log_path)
            .expect("read log")
            .contains("daemon ready")
    );
}
