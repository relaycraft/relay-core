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

/// The typed result of a tool call. Tools return both this and the serialized form as text; tests
/// assert on the structured half, which is what an agent reads.
fn structured(outcome: &relay_core_probe::tools::ToolOutcome) -> Value {
    outcome.structured.clone()
}

fn parse_resource_output(contents: &[rmcp::model::ResourceContents]) -> String {
    match &contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("unexpected resource contents: {:?}", other),
    }
}

fn text_of(outcome: &relay_core_probe::tools::ToolOutcome) -> String {
    outcome.text.clone()
}

// ── Tool schemas ──

#[test]
fn all_tool_schemas_registered() {
    let schemas = tools::tool_list();
    let names: Vec<&str> = schemas.iter().map(|t| t.name.as_ref()).collect();

    // 18 traffic/policy tools plus the 3 lifecycle tools an agent needs in order to control the
    // proxy it is reading traffic from.
    assert_eq!(schemas.len(), 21, "registered tools: {names:?}");

    for expected in [
        "proxy_status",
        "proxy_start",
        "proxy_stop",
        "search_flows",
        "get_flow",
        "get_metrics",
        "replay_flow",
        "export_har",
        "set_intercept",
        "get_pending_intercepts",
        "resume_flow",
        "set_rule",
        "list_rules",
        "delete_rule",
        "mock_url",
        "get_policy",
        "update_policy",
        "patch_policy",
        "clear_flows",
        "get_script",
        "set_script",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }

    // Lifecycle first: an agent that never learns whether a proxy is running reads every empty
    // result as "no traffic".
    assert_eq!(&names[..3], &["proxy_status", "proxy_start", "proxy_stop"]);
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
async fn search_flows_reports_an_empty_result_with_a_count() {
    let ctx = new_ctx().await;
    let result = tools::search_flows(&ctx, json!({})).await.unwrap();

    // An object rather than a bare array: `structuredContent` is an object, and the count saves an
    // agent from measuring the list itself.
    let json = structured(&result);
    assert_eq!(json["count"], 0);
    assert_eq!(json["flows"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn search_flows_with_filters() {
    let ctx = new_ctx().await;
    let result = tools::search_flows(&ctx, json!({"host": "x", "method": "GET", "limit": 5}))
        .await
        .unwrap();
    assert_eq!(structured(&result)["count"], 0);
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
    let json = structured(&result);
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
    assert_eq!(structured(&result)["transparent_enabled"], false);
}

#[tokio::test]
async fn update_policy_roundtrip() {
    let ctx = new_ctx().await;
    let policy =
        json!({"transparent_enabled": true, "redaction": {"enabled": true, "redact_bodies": true}});
    tools::update_policy(&ctx, json!({"policy": policy}))
        .await
        .unwrap();
    let json = structured(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(json["transparent_enabled"], true);
    assert_eq!(json["redaction"]["enabled"], true);
}

#[tokio::test]
async fn patch_policy_toggles_redaction() {
    let ctx = new_ctx().await;
    tools::patch_policy(&ctx, json!({"patch": {"redaction": {"enabled": true}}}))
        .await
        .unwrap();
    let json = structured(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(json["redaction"]["enabled"], true);
}

#[tokio::test]
async fn patch_policy_rejects_unknown_fields_without_changing_policy() {
    let ctx = new_ctx().await;
    let before = structured(&tools::get_policy(&ctx).await.unwrap());
    let err = tools::patch_policy(&ctx, json!({"patch": {"request_timeout_ms": 35000}}))
        .await
        .expect_err("unknown patch fields must fail");
    let ToolError::Internal(message) = err else {
        panic!("expected Internal, got {err:?}");
    };
    assert!(
        message.contains("request_timeout_ms"),
        "error should name the rejected field, got {message}"
    );
    let after = structured(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(after["request_timeout_ms"], before["request_timeout_ms"]);
}

#[tokio::test]
async fn patch_policy_updates_retention_without_clearing_the_other_bound() {
    let ctx = new_ctx().await;
    tools::patch_policy(&ctx, json!({"patch": {"retention": {"max_flows": 10}}}))
        .await
        .unwrap();
    let json = structured(&tools::get_policy(&ctx).await.unwrap());
    assert_eq!(json["retention"]["max_flows"], 10);
    assert_eq!(json["retention"]["max_age_secs"], 7 * 24 * 60 * 60);

    tools::patch_policy(&ctx, json!({"patch": {"retention": {"max_flows": null}}}))
        .await
        .unwrap();
    let json = structured(&tools::get_policy(&ctx).await.unwrap());
    assert!(json["retention"]["max_flows"].is_null());
    assert_eq!(json["retention"]["max_age_secs"], 7 * 24 * 60 * 60);
}

#[tokio::test]
async fn clear_flows_reports_an_empty_history() {
    let ctx = new_ctx().await;
    let outcome = tools::clear_flows(&ctx).await.unwrap();
    let json = structured(&outcome);
    assert_eq!(json["ok"], true);
    assert_eq!(json["flows"], 0);
    assert_eq!(json["flow_summaries"], 0);
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
    assert!(text_of(&result).contains("set successfully"));

    let result = tools::delete_rule(&ctx, json!({"id": "probe-test-rule"}))
        .await
        .unwrap();
    assert!(text_of(&result).contains("deleted"));
}

#[tokio::test]
async fn list_rules_returns_the_rule_that_was_set() {
    let ctx = new_ctx().await;
    let empty = structured(&tools::list_rules(&ctx).await.unwrap());
    assert_eq!(empty["count"], 0);

    let rule = Rule {
        id: "probe-listed-rule".to_string(),
        name: "Listed".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![],
        constraints: None,
    };
    tools::set_rule(&ctx, json!({"rule": serde_json::to_value(&rule).unwrap()}))
        .await
        .unwrap();

    let listed = structured(&tools::list_rules(&ctx).await.unwrap());
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["rules"][0]["id"], "probe-listed-rule");
}

#[tokio::test]
async fn get_script_reports_nothing_until_a_script_loads() {
    let ctx = new_ctx().await;
    let before = structured(&tools::get_script(&ctx).await.unwrap());
    assert_eq!(before["loaded"], false);
    assert!(before["script"].is_null());

    let source = "globalThis.onRequestHeaders = (_flow) => {};";
    tools::set_script(&ctx, json!({"script": source}))
        .await
        .unwrap();
    let after = structured(&tools::get_script(&ctx).await.unwrap());
    assert_eq!(after["loaded"], true);
    assert_eq!(after["script"], source);
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
    assert!(text_of(&result).contains("Mock created"));
}

// ── intercepts ──

#[tokio::test]
async fn pending_intercepts_initially_empty() {
    let ctx = new_ctx().await;
    let result = tools::get_pending_intercepts(&ctx).await.unwrap();
    assert_eq!(structured(&result)["pending_count"], 0);
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
    assert!(text_of(&result).contains("Intercept breakpoint set"));
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
fn constant_version_is_2() {
    // v2 introduced structuredContent, the {count, flows} envelope, typed write acknowledgements
    // and annotations on every tool. A bump here is a promise to consumers that the surface
    // changed in a way they must handle.
    assert_eq!(relay_core_probe::TOOL_CONTRACT_VERSION, 2);
}

// ── Lifecycle tools ──

/// A controller stub whose lifecycle state the test sets directly, so tool behaviour can be
/// asserted without binding ports.
struct StubProxyControl {
    lifecycle: relay_core_runtime::RuntimeLifecycle,
    started: std::sync::atomic::AtomicBool,
}

impl StubProxyControl {
    fn stopped() -> Arc<Self> {
        Arc::new(Self {
            lifecycle: relay_core_runtime::RuntimeLifecycle::created(),
            started: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

#[async_trait::async_trait]
impl relay_core_runtime::services::ProxyControlService for StubProxyControl {
    async fn proxy_start(
        &self,
        _requester: relay_core_runtime::services::Requester,
        request: relay_core_runtime::services::ProxyStartRequest,
    ) -> Result<
        relay_core_runtime::services::ProxyStartOutcome,
        relay_core_runtime::services::ProxyControlError,
    > {
        self.started
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(relay_core_runtime::services::ProxyStartOutcome::Started { port: request.port })
    }

    async fn proxy_stop(
        &self,
        _requester: relay_core_runtime::services::Requester,
    ) -> Result<
        relay_core_runtime::services::ProxyStopOutcome,
        relay_core_runtime::services::ProxyControlError,
    > {
        Ok(relay_core_runtime::services::ProxyStopOutcome::Stopped)
    }

    fn proxy_lifecycle(&self) -> relay_core_runtime::RuntimeLifecycle {
        self.lifecycle.clone()
    }
}

/// A controller that always fails, standing in for "the port is taken".
struct FailingProxyControl;

#[async_trait::async_trait]
impl relay_core_runtime::services::ProxyControlService for FailingProxyControl {
    async fn proxy_start(
        &self,
        _requester: relay_core_runtime::services::Requester,
        _request: relay_core_runtime::services::ProxyStartRequest,
    ) -> Result<
        relay_core_runtime::services::ProxyStartOutcome,
        relay_core_runtime::services::ProxyControlError,
    > {
        Err(
            relay_core_runtime::services::ProxyControlError::StartFailed(
                "Failed to bind to address 127.0.0.1:8080: Address already in use".to_string(),
            ),
        )
    }

    async fn proxy_stop(
        &self,
        _requester: relay_core_runtime::services::Requester,
    ) -> Result<
        relay_core_runtime::services::ProxyStopOutcome,
        relay_core_runtime::services::ProxyControlError,
    > {
        Ok(relay_core_runtime::services::ProxyStopOutcome::AlreadyStopped)
    }

    fn proxy_lifecycle(&self) -> relay_core_runtime::RuntimeLifecycle {
        relay_core_runtime::RuntimeLifecycle::created()
    }
}

async fn ctx_with_proxy(
    proxy: Arc<dyn relay_core_runtime::services::ProxyControlService>,
) -> Arc<ProbeContext> {
    Arc::new(ProbeContext::new(Arc::new(CoreState::new(None).await)).with_proxy_control(proxy))
}

#[tokio::test]
async fn proxy_status_reports_the_lifecycle() {
    let ctx = ctx_with_proxy(StubProxyControl::stopped()).await;

    let output = structured(
        &tools::dispatch(&ctx, "proxy_status", json!({}))
            .await
            .expect("proxy_status should succeed"),
    );

    assert_eq!(output["phase"], "created");
    assert_eq!(output["running"], false);
    assert!(
        output["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("proxy_start")),
        "the status must say how to start capturing: {output}"
    );
}

#[tokio::test]
async fn proxy_start_returns_the_port_and_clears_the_stop_state() {
    let ctx = ctx_with_proxy(StubProxyControl::stopped()).await;

    let output = structured(
        &tools::dispatch(&ctx, "proxy_start", json!({ "port": 9911 }))
            .await
            .expect("proxy_start should succeed"),
    );

    assert_eq!(output["outcome"], "started");
    assert_eq!(output["port"], 9911);
}

#[tokio::test]
async fn a_failed_start_is_reported_as_a_coded_error() {
    let ctx = ctx_with_proxy(Arc::new(FailingProxyControl)).await;

    let error = tools::dispatch(&ctx, "proxy_start", json!({}))
        .await
        .expect_err("a taken port must not look like success");

    assert_eq!(error.code(), Some("start_failed"));
    assert!(
        error.to_string().contains("Address already in use"),
        "the reason must reach the agent: {error}"
    );
}

#[tokio::test]
async fn lifecycle_tools_say_so_when_the_host_cannot_control_a_proxy() {
    let ctx = new_ctx().await;

    let error = tools::dispatch(&ctx, "proxy_start", json!({}))
        .await
        .expect_err("a host without a controller owns no lifecycle");

    assert_eq!(error.code(), Some("proxy_control_unavailable"));
}

#[tokio::test]
async fn empty_flow_results_say_the_proxy_is_not_running() {
    let ctx = ctx_with_proxy(StubProxyControl::stopped()).await;

    let error = tools::dispatch(&ctx, "search_flows", json!({}))
        .await
        .expect_err("an empty list with no proxy is not 'no traffic'");

    assert_eq!(error.code(), Some("proxy_not_running"));
    assert!(
        error.to_string().contains("proxy_start"),
        "the error must tell the agent what to do: {error}"
    );
}

#[tokio::test]
async fn lifecycle_tools_are_annotated_for_clients() {
    let schemas = tools::tool_list();
    let by_name = |name: &str| {
        schemas
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
            .annotations
            .clone()
            .unwrap_or_else(|| panic!("{name} must carry annotations"))
    };

    let status = by_name("proxy_status");
    assert_eq!(status.read_only_hint, Some(true));

    let start = by_name("proxy_start");
    assert_eq!(start.read_only_hint, Some(false));
    assert_eq!(start.idempotent_hint, Some(true), "start is idempotent");
    assert_eq!(start.destructive_hint, Some(false));

    let stop = by_name("proxy_stop");
    assert_eq!(
        stop.destructive_hint,
        Some(true),
        "stopping drops in-flight requests and must be advertised as such"
    );
}

// ── Tool contract v2: structured results and annotations ──

/// Every tool must say how it may be called. An agent that cannot tell a read-only query from one
/// that stops the proxy has to guess whether a call is safe, and guessing is how a stray `proxy_stop`
/// lands in the middle of a debugging session.
#[test]
fn every_tool_is_annotated() {
    for schema in tools::tool_list() {
        let annotations = schema
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", schema.name));
        assert!(
            annotations.read_only_hint.is_some(),
            "{} must declare readOnlyHint",
            schema.name
        );
        assert!(
            annotations.destructive_hint.is_some(),
            "{} must declare destructiveHint",
            schema.name
        );
        assert!(
            annotations.idempotent_hint.is_some(),
            "{} must declare idempotentHint",
            schema.name
        );
    }
}

/// Read-only tools must be advertised as such: that is what lets a client pre-approve them.
#[test]
fn observation_tools_are_marked_read_only() {
    let schemas = tools::tool_list();
    for name in [
        "proxy_status",
        "search_flows",
        "get_flow",
        "get_metrics",
        "export_har",
        "get_pending_intercepts",
        "get_policy",
        "list_rules",
        "get_script",
    ] {
        let schema = schemas
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        let annotations = schema.annotations.as_ref().expect("annotated");
        assert_eq!(
            annotations.read_only_hint,
            Some(true),
            "{name} only observes and must be advertised as read-only"
        );
    }
}

/// A declared output schema that lies is worse than no schema: clients validate against it. So every
/// declared schema is an object, and every write tool's schema requires the `ok` field its results
/// actually carry.
#[test]
fn declared_output_schemas_are_honest() {
    let schemas = tools::tool_list();
    let mut declared = 0;

    for schema in &schemas {
        let Some(output) = schema.output_schema.as_ref() else {
            continue;
        };
        declared += 1;

        let output = Value::Object(output.as_ref().clone());
        assert_eq!(
            output["type"], "object",
            "{}: structuredContent is a JSON object",
            schema.name
        );

        let read_only = schema
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.read_only_hint)
            .unwrap_or(false);
        if !read_only {
            let required = output["required"].as_array().cloned().unwrap_or_default();
            assert!(
                required.iter().any(|field| field == "ok"),
                "{} changes state, so its output schema must require `ok`",
                schema.name
            );
        }
    }

    assert!(
        declared >= 10,
        "only {declared} tools declare an output schema; the contract regressed"
    );
}

/// Read results are typed objects with a count, not bare arrays an agent has to measure.
#[tokio::test]
async fn read_results_are_typed_objects() {
    let ctx = new_ctx().await;

    let flows = tools::search_flows(&ctx, json!({})).await.expect("search");
    assert_eq!(flows.structured["count"], 0);
    assert!(flows.structured["flows"].is_array());
    assert_eq!(
        serde_json::from_str::<Value>(&flows.text).expect("text is json"),
        flows.structured,
        "the text fallback must be the structured result, so old and new clients cannot disagree"
    );

    let intercepts = tools::get_pending_intercepts(&ctx).await.expect("snapshot");
    assert_eq!(intercepts.structured["pending_count"], 0);
}

/// Write results report success in a field, so an agent never has to match a message string.
#[tokio::test]
async fn write_results_carry_ok_and_typed_fields() {
    let ctx = new_ctx().await;

    let mocked = tools::mock_url(&ctx, json!({ "url_pattern": "/api/ping", "status": 200 }))
        .await
        .expect("mock_url");

    assert_eq!(mocked.structured["ok"], true);
    assert_eq!(mocked.structured["url_pattern"], "/api/ping");
    assert_eq!(mocked.structured["status"], 200);
    assert!(
        mocked.structured["rule_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "a write must report what it created: {}",
        mocked.structured
    );
}
