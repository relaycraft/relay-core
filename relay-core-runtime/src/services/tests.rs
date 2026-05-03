use std::sync::Arc;
use relay_core_api::flow::{Flow, FlowUpdate, BodyData, Direction, Layer, HttpLayer, HttpRequest, HttpResponse, NetworkInfo, ResponseTiming, TransportProtocol};
use relay_core_api::modification::FlowQuery;
use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch, RedactionPolicy, RedactionPolicyPatch};
use relay_core_lib::rule::Rule;
use relay_core_lib::rule::{RuleStage, RuleTermination, Filter, StringMatcher};
use crate::audit::{AuditActor, AuditEventKind, AuditOutcome};
use crate::services::{
    FlowReadService, FlowEventHub, RuleService, InterceptService,
    PolicyService, AuditService, RuntimeStatusService,
};
#[cfg(feature = "script")]
use crate::services::ScriptService;
use crate::rule::{InterceptRuleConfig, MockResponseRuleConfig};
use crate::{CoreState, CoreAuditQuery};
use serde_json::json;
use url::Url;
use uuid::Uuid;
use chrono::Utc;

fn sample_flow(host: &str, path: &str) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: Some(Utc::now()),
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12000,
            server_ip: host.to_string(),
            server_port: 8080,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::Http(HttpLayer {
            request: HttpRequest {
                method: "GET".to_string(),
                url: Url::parse(&format!("http://{}{}", host, path)).unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                cookies: vec![],
                query: vec![],
                body: None,
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                cookies: vec![],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                },
            }),
            error: None,
        }),
        tags: vec![],
        meta: std::collections::HashMap::new(),
    }
}

fn sample_rule(id: &str) -> Rule {
    Rule {
        id: id.to_string(),
        name: format!("test-rule-{}", id),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains("example".to_string())),
        actions: vec![],
        constraints: None,
    }
}

#[tokio::test]
async fn flow_read_service_trait_dispatches_to_core_state() {
    let state = Arc::new(CoreState::new(None).await);
    let flow = sample_flow("trait-test.example.com", "/a");
    let flow_id = flow.id.to_string();
    state.upsert_flow(Box::new(flow));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let service: Arc<dyn FlowReadService> = state.clone();
    let loaded = service.get_flow(&flow_id).await;
    assert!(loaded.is_some());

    let results = service.search_flows(FlowQuery {
        host: Some("trait-test.example.com".to_string()),
        ..Default::default()
    }).await;
    assert!(!results.is_empty());
}

#[tokio::test]
async fn flow_event_hub_trait_dispatches_subscribe() {
    let state = Arc::new(CoreState::new(None).await);
    let hub: Arc<dyn FlowEventHub> = state.clone();

    let _rx = hub.subscribe_flow_updates();
    hub.record_flow_events_lagged(7);

    let metrics = state.get_metrics().await;
    assert_eq!(metrics.flow_events_lagged_total, 7);
}

#[tokio::test]
async fn flow_event_hub_trait_dispatches_redact() {
    let state = Arc::new(CoreState::new(None).await);
    state.update_policy_from(
        AuditActor::Runtime,
        "test".to_string(),
        ProxyPolicy {
            redaction: RedactionPolicy {
                enabled: true,
                redact_bodies: true,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let hub: Arc<dyn FlowEventHub> = state.clone();
    let update = FlowUpdate::HttpBody {
        flow_id: "f-1".to_string(),
        direction: Direction::ClientToServer,
        body: BodyData {
            encoding: "utf-8".to_string(),
            content: "secret".to_string(),
            size: 6,
        },
    };
    let redacted = hub.redact_flow_update_for_output(update);
    match redacted {
        FlowUpdate::HttpBody { body, .. } => assert_eq!(body.content, "[REDACTED]"),
        _ => panic!("expected http body"),
    }
}

#[tokio::test]
async fn rule_service_trait_dispatches_crud() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn RuleService> = state.clone();

    let rule = sample_rule("trait-rule-1");
    service
        .upsert_rule_from(
            AuditActor::Http,
            "test.upsert",
            "trait-rule-1".to_string(),
            json!({"test": true}),
            rule,
        )
        .await
        .expect("upsert should succeed");

    let rules = service.get_rules().await;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, "trait-rule-1");

    let engine = service.get_rule_engine().await;
    assert!(Arc::strong_count(&engine) >= 1);

    let deleted = service
        .delete_rule_from(
            AuditActor::Http,
            "test.delete",
            "trait-rule-1".to_string(),
            json!({}),
            "trait-rule-1",
        )
        .await
        .expect("delete should succeed");
    assert!(deleted);
    assert!(service.get_rules().await.is_empty());
}

#[tokio::test]
async fn rule_service_trait_dispatches_mock_and_intercept_creation() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn RuleService> = state.clone();

    let mock_id = service
        .create_mock_response_rule_from(
            AuditActor::Probe,
            "mock-1".to_string(),
            json!({}),
            MockResponseRuleConfig {
                rule_id: "mock-1".to_string(),
                url_pattern: "example.com".to_string(),
                name: "test-mock".to_string(),
                status: 200,
                content_type: "text/plain".to_string(),
                body: "ok".to_string(),
            },
        )
        .await
        .expect("mock create should succeed");
    assert_eq!(mock_id, "mock-1");

    let intercept_id = service
        .create_intercept_rule_from(
            AuditActor::Probe,
            "int-1".to_string(),
            json!({}),
            InterceptRuleConfig {
                rule_id: "int-1".to_string(),
                active: true,
                url_pattern: "example.com".to_string(),
                method: None,
                phase: "request".to_string(),
                name: "test-intercept".to_string(),
                priority: 100,
                termination: RuleTermination::Stop,
            },
        )
        .await
        .expect("intercept create should succeed");
    assert_eq!(intercept_id, "int-1");

    let rules = service.get_rules().await;
    assert_eq!(rules.len(), 2);
}

#[tokio::test]
async fn intercept_service_trait_resolves_with_audit() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn InterceptService> = state.clone();

    let snapshot = service.intercept_snapshot().await;
    assert_eq!(snapshot.pending_count, 0);

    let result = service
        .resolve_intercept_with_modifications_from(
            AuditActor::Probe,
            "nonexistent:request".to_string(),
            "drop",
            None,
        )
        .await;
    assert!(result.is_err());

    let intercepted = service.is_flow_intercepted("no-such-flow".to_string()).await;
    assert!(!intercepted);
}

#[tokio::test]
async fn policy_service_trait_read_and_write() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn PolicyService> = state.clone();

    let initial = service.policy_snapshot();
    assert!(!initial.transparent_enabled);

    service.update_policy_from(
        AuditActor::Http,
        "policy.test".to_string(),
        ProxyPolicy {
            transparent_enabled: true,
            ..Default::default()
        },
    );
    assert!(service.policy_snapshot().transparent_enabled);

    service.patch_policy_from(
        AuditActor::Http,
        "policy.patch".to_string(),
        ProxyPolicyPatch {
            redaction: Some(RedactionPolicyPatch {
                enabled: Some(true),
                redact_bodies: Some(true),
                ..Default::default()
            }),
        },
    );
    let patched = service.policy_snapshot();
    assert!(patched.transparent_enabled);
    assert!(patched.redaction.enabled);
}

#[tokio::test]
async fn audit_service_trait_reads_and_subscribes() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn AuditService> = state.clone();

    state.update_policy_from(
        AuditActor::Runtime,
        "test".to_string(),
        ProxyPolicy::default(),
    );

    let snapshot = service.audit_snapshot(10);
    assert!(!snapshot.events.is_empty());

    let queried = service
        .query_audit_snapshot(CoreAuditQuery {
            kind: Some(AuditEventKind::PolicyUpdated),
            limit: 10,
            ..Default::default()
        })
        .await;
    assert!(!queried.events.is_empty());

    let _rx = service.subscribe_audit_events();
    service.record_audit_events_lagged(3);

    let metrics = state.get_metrics().await;
    assert_eq!(metrics.audit_events_lagged_total, 3);
}

#[tokio::test]
async fn runtime_status_service_trait_returns_snapshots() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn RuntimeStatusService> = state.clone();

    let snapshot = service.status_snapshot();
    assert!(!snapshot.running);

    let report = service.status_report().await;
    assert!(!report.status.running);

    let metrics = service.get_metrics().await;
    assert_eq!(metrics.flows_total, 0);

    let prom = service.get_metrics_prometheus_text().await;
    assert!(prom.contains("relay_core_flows_total"));

    let _rx = service.subscribe_lifecycle();
}

#[cfg(feature = "script")]
#[tokio::test]
async fn script_service_trait_loads_script() {
    let state = Arc::new(CoreState::new(None).await);
    let service: Arc<dyn ScriptService> = state.clone();

    service
        .load_script_from(
            AuditActor::Tauri,
            "test.script".to_string(),
            "globalThis.onRequestHeaders = (_flow) => {};",
        )
        .await
        .expect("script load should succeed");

    let events = state.recent_audit_events();
    let event = events.last().expect("should have audit event");
    assert_eq!(event.kind, AuditEventKind::ScriptReloaded);
    assert_eq!(event.outcome, AuditOutcome::Success);
}
