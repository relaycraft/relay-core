use crate::RelayCoreState;
use crate::commands::flow::TauriFlowSink;
use crate::interceptor::TauriInterceptor;
use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch};
use relay_core_runtime::audit::AuditActor;
use relay_core_runtime::{
    CoreAuditSnapshot, CoreInterceptSnapshot, CoreMetrics, CoreStatusSnapshot, ProxyConfig,
    ProxySpawnResult, ProxyStopResult,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager, Runtime, State};
use tokio::sync::mpsc;

fn resolve_proxy_port(port: Option<u16>) -> u16 {
    port.unwrap_or(8080)
}

fn build_proxy_config(app_data_dir: PathBuf, port: u16) -> Result<ProxyConfig, String> {
    ProxyConfig::from_app_data_dir(app_data_dir, port)
}

#[tauri::command]
pub fn get_core_status(state: State<'_, RelayCoreState>) -> CoreStatusSnapshot {
    state.ctx.status.status_snapshot()
}

#[tauri::command]
pub async fn get_core_metrics(state: State<'_, RelayCoreState>) -> Result<CoreMetrics, String> {
    Ok(state.ctx.status.get_metrics().await)
}

#[tauri::command]
pub async fn get_pending_intercepts(
    state: State<'_, RelayCoreState>,
) -> Result<CoreInterceptSnapshot, String> {
    Ok(get_pending_intercepts_impl(&state).await)
}

#[tauri::command]
pub fn get_recent_audit(state: State<'_, RelayCoreState>) -> CoreAuditSnapshot {
    get_recent_audit_impl(&state)
}

#[tauri::command]
pub fn get_policy(state: State<'_, RelayCoreState>) -> ProxyPolicy {
    get_policy_impl(&state)
}

#[tauri::command]
pub fn update_policy(state: State<'_, RelayCoreState>, policy: ProxyPolicy) -> Result<(), String> {
    update_policy_impl(&state, policy);
    Ok(())
}

#[tauri::command]
pub fn patch_policy(
    state: State<'_, RelayCoreState>,
    patch: ProxyPolicyPatch,
) -> Result<(), String> {
    patch_policy_impl(&state, patch);
    Ok(())
}

pub async fn get_pending_intercepts_impl(state: &RelayCoreState) -> CoreInterceptSnapshot {
    state.ctx.intercepts.intercept_snapshot().await
}

pub fn get_recent_audit_impl(state: &RelayCoreState) -> CoreAuditSnapshot {
    state.ctx.audit.audit_snapshot(50)
}

pub fn get_policy_impl(state: &RelayCoreState) -> ProxyPolicy {
    state.ctx.policy.policy_snapshot()
}

pub fn update_policy_impl(state: &RelayCoreState, policy: ProxyPolicy) {
    state
        .ctx
        .policy
        .update_policy_from(AuditActor::Tauri, "tauri.policy".to_string(), policy);
}

pub fn patch_policy_impl(state: &RelayCoreState, patch: ProxyPolicyPatch) {
    state
        .ctx
        .policy
        .patch_policy_from(AuditActor::Tauri, "tauri.policy.patch".to_string(), patch);
}

#[tauri::command]
pub async fn stop_core_proxy(state: State<'_, RelayCoreState>) -> Result<String, String> {
    stop_core_proxy_impl(&state).await
}

pub async fn stop_core_proxy_impl(state: &RelayCoreState) -> Result<String, String> {
    match state.core.stop_proxy()? {
        ProxyStopResult::NotRunning => Ok("Not running".to_string()),
        ProxyStopResult::Stopping => Ok("stopped".to_string()),
    }
}

#[tauri::command]
pub async fn load_script(state: State<'_, RelayCoreState>, script: String) -> Result<(), String> {
    state
        .ctx
        .script
        .load_script_from(AuditActor::Tauri, "tauri.load_script".to_string(), &script)
        .await
}

#[tauri::command]
pub fn get_ca_cert_path<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    let app_data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let ca_cert_path = app_data_dir.join("ca_cert.pem");
    if ca_cert_path.exists() {
        Ok(ca_cert_path.to_string_lossy().to_string())
    } else {
        Err("CA certificate not found. Start the proxy first.".to_string())
    }
}

#[tauri::command]
pub async fn install_ca_cert<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    let app_data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let ca_cert_path = app_data_dir.join("ca_cert.pem");

    if !ca_cert_path.exists() {
        return Err("CA certificate not found. Start the proxy first.".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(&ca_cert_path)
            .status()
            .map_err(|e| format!("Failed to open certificate file: {}", e))?;

        if status.success() {
            Ok(
                "Certificate file opened. Please follow system prompts to add to Keychain."
                    .to_string(),
            )
        } else {
            Err("Failed to open certificate file".to_string())
        }
    }

    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("cmd")
            .args(&["/C", "start", "", &ca_cert_path.to_string_lossy()])
            .status()
            .map_err(|e| format!("Failed to open certificate file: {}", e))?;

        if status.success() {
            Ok("Certificate file opened. Please follow system prompts to install.".to_string())
        } else {
            Err("Failed to open certificate file".to_string())
        }
    }

    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("xdg-open")
            .arg(&ca_cert_path)
            .status()
            .map_err(|e| format!("Failed to open certificate file: {}", e))?;

        if status.success() {
            Ok("Certificate file opened.".to_string())
        } else {
            Err("Failed to open certificate file".to_string())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("Unsupported operating system for auto-install".to_string())
    }
}

#[tauri::command]
pub async fn start_core_proxy<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, RelayCoreState>,
    port: Option<u16>,
) -> Result<String, String> {
    println!("Starting RelayCore Proxy from Tauri Command...");

    let port = resolve_proxy_port(port);

    let app_data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let config = build_proxy_config(app_data_dir, port)?;

    // Create Flow Sink (mpsc channel)
    let (proxy_tx, proxy_rx) = mpsc::channel(1000);

    let sink = TauriFlowSink {
        app_handle: app.clone(),
    };

    // Spawn sink processor
    tokio::spawn(async move {
        sink.run(proxy_rx).await;
    });

    let tauri_interceptor = Arc::new(TauriInterceptor {
        app_handle: app.clone(),
        rules: state.ctx.rules.clone(),
        intercepts: state.ctx.intercepts.clone(),
        policy: state.ctx.policy.clone(),
        flow_sender: proxy_tx.clone(),
    });

    match state
        .core
        .spawn_proxy(config, proxy_tx, Some(tauri_interceptor))
    {
        Ok(ProxySpawnResult::Started(_)) => {}
        Ok(ProxySpawnResult::AlreadyRunning) => return Ok("Already started".to_string()),
        Err(error) => return Err(error),
    }

    Ok("started".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_proxy_config, get_pending_intercepts_impl, get_policy_impl, get_recent_audit_impl,
        patch_policy_impl, resolve_proxy_port, stop_core_proxy_impl, update_policy_impl,
    };
    use crate::RelayCoreState;
    use relay_core_api::policy::ProxyPolicy;
    use relay_core_lib::interceptor::NoOpInterceptor;
    use relay_core_runtime::{
        ProxySpawnResult, RuntimeLifecyclePhase,
        audit::{AuditActor, AuditEventKind},
    };
    use serde_json::json;
    use std::{
        sync::{Arc, Once},
        time::Duration,
    };
    use tokio::sync::mpsc;

    static INIT: Once = Once::new();

    fn init_crypto() {
        INIT.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .ok();
        });
    }

    #[test]
    fn test_resolve_proxy_port_default() {
        assert_eq!(resolve_proxy_port(None), 8080);
    }

    #[test]
    fn test_resolve_proxy_port_custom() {
        assert_eq!(resolve_proxy_port(Some(8888)), 8888);
    }

    #[test]
    fn test_build_proxy_config_creates_paths_and_defaults() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join("test-tauri-config")
            .join(format!("run-{}", unique));

        let cfg = build_proxy_config(dir.clone(), 8899).expect("should build config");
        assert!(dir.exists(), "config builder should create app_data_dir");
        assert_eq!(cfg.port, 8899);
        assert_eq!(cfg.ca_cert_path, dir.join("ca_cert.pem"));
        assert_eq!(cfg.ca_key_path, dir.join("ca_key.pem"));
        assert!(!cfg.transparent);
        assert!(cfg.udp_tproxy_port.is_none());
    }

    #[tokio::test]
    async fn test_stop_core_proxy_impl_not_running() {
        let state = RelayCoreState::new_async().await;
        let result = stop_core_proxy_impl(&state)
            .await
            .expect("stop should succeed");
        assert_eq!(result, "Not running");
    }

    #[tokio::test]
    async fn test_stop_core_proxy_impl_running_resets_state_and_signals_shutdown() {
        init_crypto();
        let state = RelayCoreState::new_async().await;
        let port = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should reserve port")
            .local_addr()
            .expect("reserved listener should expose local addr")
            .port();
        let app_data_dir = std::env::temp_dir().join(format!("relaycraft-system-test-{}", port));
        let config = build_proxy_config(app_data_dir, port).expect("should build proxy config");
        let (proxy_tx, _proxy_rx) = mpsc::channel(1000);
        let ProxySpawnResult::Started(handle) = state
            .core
            .spawn_proxy(config, proxy_tx, Some(Arc::new(NoOpInterceptor {})))
            .expect("proxy should start")
        else {
            panic!("proxy should start");
        };

        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = stop_core_proxy_impl(&state)
            .await
            .expect("stop should succeed");
        assert_eq!(result, "stopped");
        let lifecycle = state.core.lifecycle();
        assert_eq!(lifecycle.phase, RuntimeLifecyclePhase::Stopping);
        assert_eq!(lifecycle.port, Some(port));

        handle.await.expect("proxy task should join");
        assert_eq!(state.core.lifecycle().phase, RuntimeLifecyclePhase::Stopped);
    }

    #[tokio::test]
    async fn test_get_pending_intercepts_uses_shared_snapshot() {
        let state = RelayCoreState::new_async().await;
        let snapshot = get_pending_intercepts_impl(&state).await;

        assert_eq!(snapshot.pending_count, 0);
        assert_eq!(snapshot.ws_pending_count, 0);
    }

    #[tokio::test]
    async fn test_get_recent_audit_uses_shared_snapshot() {
        let state = RelayCoreState::new_async().await;
        state.core.update_policy_from(
            AuditActor::Tauri,
            "tauri.policy".to_string(),
            ProxyPolicy::default(),
        );

        let snapshot = get_recent_audit_impl(&state);

        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].actor, AuditActor::Tauri);
        assert_eq!(snapshot.events[0].kind, AuditEventKind::PolicyUpdated);
        assert_eq!(
            snapshot.events[0].details["transparent_enabled"],
            json!(false)
        );
    }

    #[tokio::test]
    async fn test_update_policy_impl_supports_redaction_passthrough() {
        let state = RelayCoreState::new_async().await;
        update_policy_impl(
            &state,
            ProxyPolicy {
                redaction: relay_core_api::policy::RedactionPolicy {
                    enabled: true,
                    redact_bodies: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let policy = get_policy_impl(&state);
        assert!(policy.redaction.enabled);
        assert!(policy.redaction.redact_bodies);

        let snapshot = get_recent_audit_impl(&state);
        assert!(!snapshot.events.is_empty());
        assert_eq!(snapshot.events[0].actor, AuditActor::Tauri);
        assert_eq!(snapshot.events[0].details["redaction_enabled"], json!(true));
        assert_eq!(snapshot.events[0].details["redact_bodies"], json!(true));
    }

    #[tokio::test]
    async fn test_patch_policy_impl_updates_only_targeted_fields() {
        let state = RelayCoreState::new_async().await;
        let before = get_policy_impl(&state);

        patch_policy_impl(
            &state,
            relay_core_api::policy::ProxyPolicyPatch {
                redaction: Some(relay_core_api::policy::RedactionPolicyPatch {
                    enabled: Some(true),
                    ..Default::default()
                }),
                upstream: None,
            },
        );

        let after = get_policy_impl(&state);
        assert_eq!(after.request_timeout_ms, before.request_timeout_ms);
        assert!(after.redaction.enabled);
        assert_eq!(
            after.redaction.redact_bodies,
            before.redaction.redact_bodies
        );

        let snapshot = get_recent_audit_impl(&state);
        assert!(!snapshot.events.is_empty());
        assert_eq!(snapshot.events[0].actor, AuditActor::Tauri);
        assert_eq!(snapshot.events[0].details["redaction_enabled"], json!(true));
    }
}
