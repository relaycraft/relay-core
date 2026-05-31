use relay_core_lib::interceptor::NoOpInterceptor;
use relay_core_runtime::{
    CoreState, ProxyConfig, ProxySpawnResult, ProxyStopResult, RuntimeLifecyclePhase,
};
use relay_core_tauri::RelayCoreState;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::mpsc;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}

async fn wait_until_phase(core: &CoreState, phase: RuntimeLifecyclePhase, timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if core.lifecycle().phase == phase {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "timed out waiting for {:?}, got {:?}",
        phase,
        core.lifecycle().phase
    );
}

#[tokio::test]
async fn test_proxy_lifecycle_10x() {
    init_crypto();
    // 1. Initialize State
    let state = Arc::new(RelayCoreState::new_async().await);
    let port = 18080; // Use a test port
    let ca_dir = tempdir().unwrap();

    for i in 1..=10 {
        println!("Iteration {}: Starting proxy...", i);
        assert!(
            !state.core.lifecycle().is_active(),
            "Proxy should not be active at start of iteration {}",
            i
        );

        let ca_cert_path = ca_dir.path().join(format!("ca_cert_{i}.pem"));
        let ca_key_path = ca_dir.path().join(format!("ca_key_{i}.pem"));

        let config = ProxyConfig {
            port,
            ca_cert_path,
            ca_key_path,
            transparent: false,
            udp_tproxy_port: None,
        };

        let (proxy_tx, _proxy_rx) = mpsc::channel(1000);
        let interceptor = Arc::new(NoOpInterceptor {});
        let ProxySpawnResult::Started(handle) = state
            .core
            .spawn_proxy(config, proxy_tx, Some(interceptor))
            .expect("proxy should start")
        else {
            panic!("proxy should start");
        };

        wait_until_phase(
            state.core.as_ref(),
            RuntimeLifecyclePhase::Running,
            Duration::from_secs(5),
        )
        .await;
        match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await {
            Ok(_) => panic!("Port {} should be in use!", port),
            Err(_) => println!("Port {} is correctly in use.", port),
        }

        println!("Iteration {}: Stopping proxy...", i);
        assert_eq!(
            state
                .core
                .stop_proxy()
                .expect("stop request should succeed"),
            ProxyStopResult::Stopping
        );
        assert_eq!(
            state.core.lifecycle().phase,
            RuntimeLifecyclePhase::Stopping
        );

        let _ = handle.await;

        wait_until_phase(
            state.core.as_ref(),
            RuntimeLifecyclePhase::Stopped,
            Duration::from_secs(5),
        )
        .await;
        match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await {
            Ok(_) => println!("Port {} released successfully.", port),
            Err(e) => panic!("Port {} failed to release: {}", port, e),
        }

        println!("Iteration {} completed successfully.\n", i);
    }
}
