use std::sync::Arc;
use std::time::Duration;
use relay_core_tauri::RelayCoreState;
use relay_core_runtime::{ProxyConfig, ProxySpawnResult, ProxyStopResult, RuntimeLifecyclePhase};
use relay_core_lib::interceptor::NoOpInterceptor;
use tokio::sync::mpsc;
use std::sync::Once;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider().install_default().ok();
    });
}

#[tokio::test]
async fn test_proxy_lifecycle_10x() {
    init_crypto();
    // 1. Initialize State
    let state = Arc::new(RelayCoreState::new_async().await);
    let port = 18080; // Use a test port

    for i in 1..=10 {
        println!("Iteration {}: Starting proxy...", i);
        assert!(
            !state.core.lifecycle().is_active(),
            "Proxy should not be active at start of iteration {}",
            i
        );

        let app_data_dir = std::env::temp_dir().join("relaycraft_test");
        if !app_data_dir.exists() {
            std::fs::create_dir_all(&app_data_dir).unwrap();
        }
        let ca_cert_path = app_data_dir.join("ca_cert.pem");
        let ca_key_path = app_data_dir.join("ca_key.pem");

        // Ensure clean slate for CA if needed, or reuse
        // For speed, reuse is fine.

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

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(state.core.lifecycle().phase, RuntimeLifecyclePhase::Running);
        match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await {
            Ok(_) => panic!("Port {} should be in use!", port),
            Err(_) => println!("Port {} is correctly in use.", port),
        }

        println!("Iteration {}: Stopping proxy...", i);
        assert_eq!(
            state.core.stop_proxy().expect("stop request should succeed"),
            ProxyStopResult::Stopping
        );
        assert_eq!(state.core.lifecycle().phase, RuntimeLifecyclePhase::Stopping);

        let _ = handle.await;
        
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(state.core.lifecycle().phase, RuntimeLifecyclePhase::Stopped);
        match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await {
            Ok(_) => println!("Port {} released successfully.", port),
            Err(e) => panic!("Port {} failed to release: {}", port, e),
        }
        
        println!("Iteration {} completed successfully.\n", i);
    }
}
