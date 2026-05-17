use std::sync::Arc;
use tauri::{Runtime, Manager};
use relay_core_runtime::CoreState;
use relay_core_runtime::services::{
    FlowReadService, RuleService, InterceptService, PolicyService,
    AuditService, RuntimeStatusService, ScriptService,
};
pub use relay_core_runtime::rule::InterceptRule;

pub mod commands;
pub mod transport;
pub mod interceptor;

/// Narrow-trait context for Tauri commands — most data operations go through here.
pub struct TauriContext {
    pub flows: Arc<dyn FlowReadService>,
    pub rules: Arc<dyn RuleService>,
    pub intercepts: Arc<dyn InterceptService>,
    pub policy: Arc<dyn PolicyService>,
    pub audit: Arc<dyn AuditService>,
    pub status: Arc<dyn RuntimeStatusService>,
    pub script: Arc<dyn ScriptService>,
}

impl TauriContext {
    pub fn new(core: Arc<CoreState>) -> Self {
        Self {
            flows: core.clone(),
            rules: core.clone(),
            intercepts: core.clone(),
            policy: core.clone(),
            audit: core.clone(),
            status: core.clone(),
            script: core.clone(),
        }
    }
}

pub struct RelayCoreState {
    pub core: Arc<CoreState>,
    pub ctx: Arc<TauriContext>,
}

impl Default for RelayCoreState {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayCoreState {
    pub fn new() -> Self {
        tauri::async_runtime::block_on(async {
            Self::new_async().await
        })
    }

    pub async fn new_async() -> Self {
        let core = Arc::new(CoreState::new(None).await);
        Self {
            ctx: Arc::new(TauriContext::new(core.clone())),
            core,
        }
    }
}

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::<R>::new("relay-core-tauri")
        .setup(|app, _api| {
            println!("RelayCore Adapter Initialized");
            app.manage(RelayCoreState::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_core_proxy,
            commands::stop_core_proxy,
            commands::get_core_status,
            commands::get_core_metrics,
            commands::get_policy,
            commands::get_pending_intercepts,
            commands::get_recent_audit,
            commands::get_flow_detail,
            commands::resume_flow,
            commands::set_intercept_rule,
            commands::update_policy,
            commands::patch_policy,
            commands::load_script,
            commands::get_ca_cert_path,
            commands::install_ca_cert
        ])
        .build()
}
