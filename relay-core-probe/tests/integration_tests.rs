use relay_core_api::rule::{Action, Filter, Rule, RuleStage, RuleTermination};
use relay_core_probe::{resources, server::ProbeContext, tools, tools::ToolError};
use relay_core_runtime::CoreState;
use rmcp::ServerHandler;
use serde_json::{Value, json};
use std::sync::Arc;

async fn new_ctx() -> Arc<ProbeContext> {
    Arc::new(ProbeContext::new(Arc::new(CoreState::new(None).await)))
}

fn new_server() -> relay_core_probe::ProbeServer {
    use relay_core_probe::{ProbeConfig, ProbeServer, ProbeTransport};
    ProbeServer::new(
        ProbeConfig {
            transport: ProbeTransport::Stdio,
        },
        Arc::new(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(CoreState::new(None)),
        ),
    )
}

fn parse_tool_output(content: &[rmcp::model::Content]) -> Value {
    let serialized = serde_json::to_value(&content[0]).expect("content should serialize");
    let text = serialized["text"]
        .as_str()
        .expect("content should have text field");
    serde_json::from_str(text).expect("tool output should be valid JSON")
}

fn parse_resource_output(contents: &[rmcp::model::ResourceContents]) -> String {
    match &contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("unexpected resource contents: {:?}", other),
    }
}

fn extract_text(content: &[rmcp::model::Content]) -> String {
    serde_json::to_value(&content[0]).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string()
}

// ── Tool schemas ──

#[test]
fn all_15_tool_schemas_registered() {
    let schemas = tools::tool_list();
    assert_eq!(schemas.len(), 15);
    let names: Vec<&str> = schemas.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "search_flows",
        "get_flow",
        "get_metrics",
        "replay_flow",
        "export_har",
        "set_intercept",
        "get_pending_intercepts",
        "resume_flow",
        "set_rule",
        "delete_rule",
        "mock_url",
        "get_policy",
        "update_policy",
        "patch_policy",
        "set_script",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
}

#[test]
fn tool_schemas_have_descriptions() {
    for tool in tools::tool_list() {
        assert!(
            tool.description.as_deref().is_some_and(|d| !d.is_empty()),
            "tool {} has no description",
            tool.name
        );
    }
}

// ── search_flows ──

#[tokio::test]
async fn search_flows_returns_empty_array() {
    let ctx = new_ctx().await;
    let result = tools::search_flows(&ctx, json!({})).await.unwrap();
    assert!(parse_tool_output(&result).is_array());
}

#[tokio::test]
async fn search_flows_with_filters() {
    let ctx = new_ctx().await;
    let result = tools::search_flows(&ctx, json!({"host": "x", "method": "GET", "limit": 5}))
        .await
        .unwrap();
    assert!(parse_tool_output(&result).is_array());
}

// ── get_flow ──

#[tokio::test]
async fn get_flow_nonexistent_errors() {
    let ctx = new_ctx().await;
    assert!(
        tools::get_flow(&ctx, json!({"id": "00000000-0000-0000-0000-000000000000"}))
            .await
            .is_err()
    );
}

// ── get_metrics ──

#[tokio::test]
async fn get_metrics_has_expected_keys() {
    let ctx = new_ctx().await;
    let result = tools::get_metrics(&ctx).await.unwrap();
    let json = parse_tool_output(&result);
    for key in [
        "flows_total",
        "intercepts_pending",
        "rule_exec_errors",
        "audit_events_total",
        "proxy_bytes_sent_total",
        "proxy_bytes_recv_total",
    ] {
        assert!(json.get(key).is_some(), "missing metrics key: {key}");
    }
}

// ── policy ──

#[tokio::test]
async fn get_policy_defaults() {
    let ctx = new_ctx().await;
    let result = tools::get_policy(&ctx).await.unwrap();
    assert_eq!(parse_tool_output(&result)["transparent_enabled"], false);
}

#[tokio::test]
async fn update_policy_roundtrip() {
    let ctx = new_ctx().await;
    let policy =
        json!({"transparent_enabled": true, "redaction": {"enabled": true, "redact_bodies": true}});
    tools::update_policy(&ctx, json!({"policy": policy}))
        .await
        .unwrap();
    let json = parse_tool_output(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(json["transparent_enabled"], true);
    assert_eq!(json["redaction"]["enabled"], true);
}

#[tokio::test]
async fn patch_policy_toggles_redaction() {
    let ctx = new_ctx().await;
    tools::patch_policy(&ctx, json!({"patch": {"redaction": {"enabled": true}}}))
        .await
        .unwrap();
    let json = parse_tool_output(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(json["redaction"]["enabled"], true);
}

// ── rule CRUD ──

#[tokio::test]
async fn set_and_delete_rule() {
    let ctx = new_ctx().await;
    let rule = Rule {
        id: "probe-test-rule".to_string(),
        name: "Probe Test".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 10,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "x-test".to_string(),
            value: "1".to_string(),
        }],
        constraints: None,
    };
    let result = tools::set_rule(&ctx, json!({"rule": serde_json::to_value(&rule).unwrap()}))
        .await
        .unwrap();
    assert!(extract_text(&result).contains("set successfully"));

    let result = tools::delete_rule(&ctx, json!({"id": "probe-test-rule"}))
        .await
        .unwrap();
    assert!(extract_text(&result).contains("deleted"));
}

#[tokio::test]
async fn delete_nonexistent_rule_errors() {
    let ctx = new_ctx().await;
    assert!(
        tools::delete_rule(&ctx, json!({"id": "never-exists"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mock_url_returns_success() {
    let ctx = new_ctx().await;
    let result = tools::mock_url(
        &ctx, json!({"url_pattern": "test.example.com/mock", "status": 200, "body": "{}", "content_type": "application/json"}),
    ).await.unwrap();
    assert!(extract_text(&result).contains("Mock created"));
}

// ── intercepts ──

#[tokio::test]
async fn pending_intercepts_initially_empty() {
    let ctx = new_ctx().await;
    let result = tools::get_pending_intercepts(&ctx).await.unwrap();
    assert_eq!(parse_tool_output(&result)["pending_count"], 0);
}

#[tokio::test]
async fn set_intercept_creates_breakpoint() {
    let ctx = new_ctx().await;
    let result = tools::set_intercept(
        &ctx,
        json!({"url_pattern": "example.com/api", "phase": "request"}),
    )
    .await
    .unwrap();
    assert!(extract_text(&result).contains("Intercept breakpoint set"));
}

// ── Resources ──

#[tokio::test]
async fn flows_resource_returns_markdown() {
    let ctx = new_ctx().await;
    let contents = resources::read_resource(&ctx, "flows://").await.unwrap();
    let md = parse_resource_output(&contents);
    assert!(md.contains("# Recent Flows"), "got: {md}");
}

#[tokio::test]
async fn rules_resource_is_valid_json() {
    let ctx = new_ctx().await;
    let contents = resources::read_resource(&ctx, "rules://").await.unwrap();
    let text = parse_resource_output(&contents);
    let _: Value = serde_json::from_str(&text).expect("valid JSON");
}

#[tokio::test]
async fn proxy_status_has_consistent_shape() {
    let ctx = new_ctx().await;
    let contents = resources::read_resource(&ctx, "proxy://status")
        .await
        .unwrap();
    let text = parse_resource_output(&contents);
    let json: Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(json["status"]["phase"], "created");
    assert!(json["metrics"].is_object());
}

#[tokio::test]
async fn audit_resource_events_is_array() {
    let ctx = new_ctx().await;
    let contents = resources::read_resource(&ctx, "audit://recent")
        .await
        .unwrap();
    let text = parse_resource_output(&contents);
    let json: Value = serde_json::from_str(&text).expect("valid JSON");
    assert!(json["events"].is_array());
}

#[tokio::test]
async fn ca_install_guide_has_os_sections() {
    let ctx = new_ctx().await;
    let contents = resources::read_resource(&ctx, "ca://install")
        .await
        .unwrap();
    let text = parse_resource_output(&contents);
    assert!(text.contains("Install RelayCore CA"));
    assert!(text.contains("macOS"));
    assert!(text.contains("Linux"));
}

#[tokio::test]
async fn unknown_resource_errors() {
    let ctx = new_ctx().await;
    assert!(resources::read_resource(&ctx, "unknown://x").await.is_err());
}

// ── Dispatch ──

#[tokio::test]
async fn dispatch_unknown_tool_not_found() {
    let ctx = new_ctx().await;
    let err = tools::dispatch(&ctx, "does_not_exist", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "got: {err:?}");
}

// ── export_har ──

#[tokio::test]
async fn export_har_missing_flow_errors() {
    let ctx = new_ctx().await;
    assert!(
        tools::export_har(&ctx, json!({"id": "00000000-0000-0000-0000-000000000000"}))
            .await
            .is_err()
    );
}

// ── ServerHandler smoke ──

#[test]
fn server_info_identity() {
    let info = new_server().get_info();
    assert_eq!(info.server_info.name, "relay-core-probe");
    assert!(!info.server_info.version.is_empty());
}

#[test]
fn constant_version_is_1() {
    assert_eq!(relay_core_probe::TOOL_CONTRACT_VERSION, 1);
}
