use std::sync::Arc;
use tauri::{Runtime, Manager};
use relay_core_runtime::CoreState;
pub use relay_core_runtime::rule::InterceptRule;

pub mod commands;
pub mod transport;
pub mod interceptor;

pub struct RelayCoreState {
    pub core: Arc<CoreState>,
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
        Self {
            core: Arc::new(CoreState::new(None).await),
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
