//! End-to-end tests of the control plane: a real HTTP API server on a real port, driven through
//! `ControlClient` exactly as the CLI and the MCP bridge drive the daemon.
//!
//! These are the tests that hold decision
//! [`0007`](../../docs/decisions/0007-daemon-control-plane.md) in place: a client attaches to a
//! running engine, and start/stop outcomes are reported rather than inferred.

use relay_core_http::control::{
    CONTROL_API_VERSION, ControlClient, ControlClientError, DaemonManifest, DaemonStatus, connect,
    write_manifest,
};
use relay_core_http::{HttpApiConfig, HttpApiServer};
use relay_core_runtime::CoreState;
use relay_core_runtime::RuntimeLifecyclePhase;
use relay_core_runtime::services::{CoreProxyController, ProxyStartOutcome, ProxyStartRequest};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Harness {
    api_port: u16,
    proxy_port: u16,
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// Start a daemon-shaped HTTP server: it owns a proxy controller and accepts shutdown requests.
async fn start_daemon(token: Option<&str>, dir: &Path) -> Harness {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let api_port = free_port();
    let proxy_port = free_port();
    let state = Arc::new(CoreState::new(None).await);
    let controller = Arc::new(CoreProxyController::new(state.clone()));
    let shutdown = Arc::new(tokio::sync::Notify::new());

    let mut config = HttpApiConfig::new(api_port);
    config.bearer_token = token.map(str::to_string);

    let server = HttpApiServer::new(config, state)
        .with_proxy_control(controller)
        .with_shutdown_signal(shutdown.clone());

    tokio::spawn(async move {
        let _ = server.run().await;
    });

    let manifest = DaemonManifest {
        pid: std::process::id(),
        api_version: CONTROL_API_VERSION.to_string(),
        engine_version: "0.0.0-test".to_string(),
        api_port,
        proxy_port: None,
        mcp_port: None,
        serve_webui: false,
        token: token.map(str::to_string),
        started_at_ms: now_ms(),
        data_dir: dir.to_path_buf(),
    };

    let harness = Harness {
        api_port,
        proxy_port,
    };

    // Wait for the listener rather than sleeping a guessed amount.
    let addr: SocketAddr = format!("127.0.0.1:{api_port}").parse().expect("addr");
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    write_manifest(dir, &manifest).expect("write manifest");
    harness
}

fn start_request(dir: &Path, port: u16) -> ProxyStartRequest {
    ProxyStartRequest {
        port,
        ca_cert: Some(dir.join("ca_cert.pem")),
        ca_key: Some(dir.join("ca_key.pem")),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_client_discovers_and_attaches_to_a_running_daemon() {
    let registry = tempfile::tempdir().expect("temp dir");
    let harness = start_daemon(None, registry.path()).await;

    match connect(registry.path()).await {
        DaemonStatus::Running { manifest, client } => {
            assert_eq!(manifest.api_port, harness.api_port);
            assert!(client.health().await);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[tokio::test]
async fn proxy_lifecycle_round_trips_through_the_control_plane() {
    let registry = tempfile::tempdir().expect("temp dir");
    let harness = start_daemon(None, registry.path()).await;
    let DaemonStatus::Running { client, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };

    let started = client
        .proxy_start(&start_request(registry.path(), harness.proxy_port))
        .await
        .expect("start over the wire");
    assert_eq!(
        started,
        ProxyStartOutcome::Started {
            port: harness.proxy_port
        }
    );

    let lifecycle = client.proxy_lifecycle().await.expect("lifecycle");
    assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Running);
    assert_eq!(lifecycle.port, Some(harness.proxy_port));

    client.proxy_stop().await.expect("stop over the wire");
    let lifecycle = client.proxy_lifecycle().await.expect("lifecycle");
    assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Stopped);
}

#[tokio::test]
async fn a_second_client_attaches_to_the_same_proxy_instead_of_starting_another() {
    let registry = tempfile::tempdir().expect("temp dir");
    let harness = start_daemon(None, registry.path()).await;

    let DaemonStatus::Running { client: first, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };
    let DaemonStatus::Running { client: second, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };

    first
        .proxy_start(&start_request(registry.path(), harness.proxy_port))
        .await
        .expect("first start");

    // The second client asks for a different port; the answer must be the one already listening.
    let outcome = second
        .proxy_start(&start_request(registry.path(), free_port()))
        .await
        .expect("second start");

    assert_eq!(
        outcome,
        ProxyStartOutcome::AlreadyRunning {
            port: harness.proxy_port
        }
    );
}

#[tokio::test]
async fn a_taken_port_is_rejected_with_a_code_not_an_empty_success() {
    let registry = tempfile::tempdir().expect("temp dir");
    let harness = start_daemon(None, registry.path()).await;
    let DaemonStatus::Running { client, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };
    let occupied = TcpListener::bind("127.0.0.1:0").expect("hold port");
    let port = occupied.local_addr().expect("addr").port();

    let error = client
        .proxy_start(&start_request(registry.path(), port))
        .await
        .expect_err("a taken port must fail");

    match error {
        ControlClientError::Rejected { status, code, .. } => {
            assert_eq!(status, 409);
            assert_eq!(code, "start_failed");
        }
        other => panic!("expected a coded rejection, got {other:?}"),
    }
    let _ = harness;
}

#[tokio::test]
async fn shutdown_stops_the_host() {
    let registry = tempfile::tempdir().expect("temp dir");
    let _harness = start_daemon(None, registry.path()).await;
    let DaemonStatus::Running { client, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };

    client.shutdown().await.expect("shutdown accepted");

    // Asserted on the observable outcome — the control API stops answering — rather than on the
    // notification itself: `Notify` wakes a single waiter, and the host's own graceful-shutdown
    // future is the one waiting.
    let mut stopped = false;
    for _ in 0..200 {
        if !client.health().await {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        stopped,
        "the host must stop serving after a shutdown request"
    );
}

#[tokio::test]
async fn the_manifest_token_is_required_and_used() {
    let registry = tempfile::tempdir().expect("temp dir");
    let _harness = start_daemon(Some("s3cret"), registry.path()).await;

    // A client built from the manifest carries the token and is accepted.
    let DaemonStatus::Running { client, .. } = connect(registry.path()).await else {
        panic!("daemon should be running");
    };
    assert!(client.health().await);

    // A client that drops the token is refused, and the refusal is reported as a rejection
    // rather than as "nothing is running".
    let untokened = ControlClient::new(client.base_url().to_string(), None);
    let error = untokened.status().await.expect_err("token is required");
    assert_eq!(error.code(), "missing_or_invalid_bearer_token");
}

#[tokio::test]
async fn a_live_pid_with_a_dead_api_is_reported_as_unresponsive() {
    let registry = tempfile::tempdir().expect("temp dir");
    let manifest = DaemonManifest {
        pid: std::process::id(),
        api_version: CONTROL_API_VERSION.to_string(),
        engine_version: "0.0.0-test".to_string(),
        // Nothing is listening here.
        api_port: free_port(),
        proxy_port: None,
        mcp_port: None,
        serve_webui: false,
        token: None,
        started_at_ms: now_ms(),
        data_dir: registry.path().to_path_buf(),
    };
    write_manifest(registry.path(), &manifest).expect("write manifest");

    match connect(registry.path()).await {
        DaemonStatus::Unresponsive {
            manifest: found,
            reason,
        } => {
            assert_eq!(found.api_port, manifest.api_port);
            assert!(
                reason.contains(&manifest.control_base_url()),
                "the failure must name the control URL, got {reason}"
            );
        }
        other => panic!("expected Unresponsive, got {other:?}"),
    }
}

#[tokio::test]
async fn a_manifest_for_a_dead_process_is_not_running() {
    let registry = tempfile::tempdir().expect("temp dir");
    let manifest = DaemonManifest {
        pid: 999_999_999,
        api_version: CONTROL_API_VERSION.to_string(),
        engine_version: "0.0.0-test".to_string(),
        api_port: free_port(),
        proxy_port: None,
        mcp_port: None,
        serve_webui: false,
        token: None,
        started_at_ms: now_ms(),
        data_dir: registry.path().to_path_buf(),
    };
    write_manifest(registry.path(), &manifest).expect("write manifest");

    assert!(matches!(
        connect(registry.path()).await,
        DaemonStatus::NotRunning
    ));
}

#[tokio::test]
async fn an_empty_registry_reports_not_running() {
    let registry = tempfile::tempdir().expect("temp dir");
    assert!(matches!(
        connect(registry.path()).await,
        DaemonStatus::NotRunning
    ));
}

/// A machine pointed at RelayCore has `HTTP_PROXY` (or the Windows system proxy) set.
/// The control call is to the daemon itself, so it must not be sent back through that proxy:
/// otherwise `relay-core status` reports a live pid as unreachable.
#[tokio::test]
async fn control_calls_reach_the_daemon_when_http_proxy_is_set() {
    let proxy = TcpListener::bind("127.0.0.1:0").expect("proxy port");
    let proxy_addr = proxy.local_addr().expect("proxy addr");
    std::thread::spawn(move || {
        for _ in 0..16 {
            let Ok((mut stream, _)) = proxy.accept() else {
                break;
            };
            use std::io::Write;
            let _ = stream.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });

    let _guard = ProxyEnv::set(&format!("http://{proxy_addr}"));
    let registry = tempfile::tempdir().expect("temp dir");
    let _harness = start_daemon(Some("s3cret"), registry.path()).await;

    match connect(registry.path()).await {
        DaemonStatus::Running { client, .. } => {
            assert!(
                client.health().await,
                "health must not go through HTTP_PROXY"
            );
        }
        other => panic!("expected Running through a direct connection, got {other:?}"),
    }
}

/// Sets the proxy variables reqwest consults, and removes them when the test ends.
struct ProxyEnv;

impl ProxyEnv {
    fn set(url: &str) -> Self {
        for key in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            // SAFETY: the test process is the only one reading these, and Drop clears them.
            unsafe { std::env::set_var(key, url) };
        }
        Self
    }
}

impl Drop for ProxyEnv {
    fn drop(&mut self) {
        for key in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            // SAFETY: paired with the set above; nothing else in this test binary reads them.
            unsafe { std::env::remove_var(key) };
        }
    }
}

#[tokio::test]
async fn a_foreign_control_protocol_is_reported_as_incompatible() {
    let registry = tempfile::tempdir().expect("temp dir");
    let manifest = DaemonManifest {
        pid: std::process::id(),
        api_version: "999".to_string(),
        engine_version: "0.0.0-test".to_string(),
        api_port: free_port(),
        proxy_port: None,
        mcp_port: None,
        serve_webui: false,
        token: None,
        started_at_ms: now_ms(),
        data_dir: registry.path().to_path_buf(),
    };
    write_manifest(registry.path(), &manifest).expect("write manifest");

    match connect(registry.path()).await {
        DaemonStatus::Incompatible { found, expected } => {
            assert_eq!(found, "999");
            assert_eq!(expected, CONTROL_API_VERSION);
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
}
