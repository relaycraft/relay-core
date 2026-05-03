const COMMANDS: &[&str] = &[
    "start_core_proxy",
    "stop_core_proxy",
    "get_core_status",
    "get_core_metrics",
    "get_pending_intercepts",
    "get_recent_audit",
    "get_flow_detail",
    "resume_flow",
    "set_intercept_rule",
    "load_script",
    "get_ca_cert_path",
    "install_ca_cert"
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .build();
}
