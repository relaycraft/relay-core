use std::collections::HashMap;

use chrono::Utc;
use relay_core_api::flow::{
    BodyData, Direction, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
    ResponseTiming, TransportProtocol, WebSocketLayer, WebSocketMessage,
};
use relay_core_lib::InterceptionResult;
use relay_core_lib::rule::{Action, Filter, Rule, RuleStage, RuleTermination, RuleTraceSummary};
use relay_core_runtime::CoreState;
use tokio::sync::oneshot;
use url::Url;
use uuid::Uuid;

fn create_test_flow(url: &str, method: &str) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_ip: "1.1.1.1".to_string(),
            server_port: 80,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::Http(HttpLayer {
            request: HttpRequest {
                method: method.to_string(),
                url: Url::parse(url).expect("invalid url"),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            response: None,
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
            resilience_trace: None,
    }
}

fn create_test_ws_flow(url: &str) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_ip: "1.1.1.1".to_string(),
            server_port: 80,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::WebSocket(WebSocketLayer {
            handshake_request: HttpRequest {
                method: "GET".to_string(),
                url: Url::parse(url).expect("invalid url"),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Host".to_string(), "example.com".to_string())],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            handshake_response: HttpResponse {
                status: 101,
                status_text: "Switching Protocols".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![],
                cookies: vec![],
                body: None,
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
            },
            messages: vec![],
            closed: false,
        }),
        tags: vec![],
        meta: HashMap::new(),
            resilience_trace: None,
    }
}

#[tokio::test]
async fn test_core_state_upsert_and_get_flow() {
    let state = CoreState::new(None).await;
    let flow = create_test_flow("http://example.com/api", "GET");
    let flow_id = flow.id.to_string();

    state.upsert_flow(Box::new(flow));

    // Actor messages are async; wait briefly for processing.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let found = state.get_flow(flow_id).await;
    assert!(
        found.is_some(),
        "flow should be available in FlowStoreActor"
    );
}

#[tokio::test]
async fn test_core_state_get_flow_missing_returns_none() {
    let state = CoreState::new(None).await;
    let found = state.get_flow("missing-flow-id".to_string()).await;
    assert!(found.is_none(), "missing flow should return none");
}

#[tokio::test]
async fn test_core_state_intercept_prefix_lookup() {
    let state = CoreState::new(None).await;
    let key = "flow-123:request_headers".to_string();

    let (tx, _rx) = oneshot::channel();
    state.register_intercept(key.clone(), tx).await;

    let pending = state.is_flow_intercepted("flow-123".to_string()).await;
    assert!(
        pending,
        "flow should be marked intercepted by flow id prefix"
    );

    let resolved = state
        .resolve_intercept(key, InterceptionResult::Continue)
        .await;
    assert!(resolved.is_ok(), "intercept should resolve successfully");

    let pending_after = state.is_flow_intercepted("flow-123".to_string()).await;
    assert!(
        !pending_after,
        "flow should not be marked intercepted after resolve"
    );
}

#[tokio::test]
async fn test_core_state_intercept_prefix_does_not_match_other_flows() {
    let state = CoreState::new(None).await;
    let key = "flow-a:request_headers".to_string();

    let (tx, _rx) = oneshot::channel();
    state.register_intercept(key, tx).await;

    assert!(
        !state.is_flow_intercepted("flow-b".to_string()).await,
        "other flow id should not be marked intercepted"
    );
}

#[tokio::test]
async fn test_core_state_set_and_get_rules() {
    let state = CoreState::new(None).await;
    let rule = Rule {
        id: "rule-1".to_string(),
        name: "test-rule".to_string(),
        active: true,
        priority: 100,
        stage: RuleStage::RequestHeaders,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "x-test".to_string(),
            value: "1".to_string(),
        }],
        termination: RuleTermination::Continue,
        constraints: None,
    };

    state.set_rules(vec![rule]).await;
    let rules = state.get_rules().await;
    assert_eq!(
        rules.len(),
        1,
        "RuleStoreActor should persist in-memory rules"
    );
    assert_eq!(rules[0].id, "rule-1");
}

#[tokio::test]
async fn test_core_state_pending_ws_message_lifecycle() {
    let state = CoreState::new(None).await;
    let flow_key = "flow-ws-1:ws_msg:msg-1".to_string();
    let msg = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "utf-8".to_string(),
            content: "hello".to_string(),
            size: 5,
        },
        opcode: "Text".to_string(),
    };

    state
        .set_pending_ws_message(flow_key.clone(), msg.clone())
        .await;

    let found = state.get_pending_ws_message(flow_key.clone()).await;
    assert!(
        found.is_some(),
        "pending websocket message should be retrievable"
    );
    assert_eq!(found.expect("msg").content.content, "hello");

    let (tx, _rx) = oneshot::channel();
    state.register_intercept(flow_key.clone(), tx).await;
    let resolved = state
        .resolve_intercept(flow_key.clone(), InterceptionResult::Continue)
        .await;
    assert!(resolved.is_ok(), "ws intercept should resolve");

    let after = state.get_pending_ws_message(flow_key).await;
    assert!(
        after.is_none(),
        "pending websocket message should be cleared on resolve"
    );
}

#[tokio::test]
async fn test_core_state_ws_message_buffer_capped_at_2000() {
    let state = CoreState::new(None).await;
    let flow = create_test_ws_flow("ws://example.com/socket");
    let flow_id = flow.id.to_string();
    state.upsert_flow(Box::new(flow));

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    for i in 0..2005 {
        let msg = WebSocketMessage {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            direction: Direction::ClientToServer,
            content: BodyData {
                encoding: "utf-8".to_string(),
                content: format!("m{}", i),
                size: 2,
            },
            opcode: "Text".to_string(),
        };
        state.append_ws_message(flow_id.clone(), msg);
    }

    tokio::time::sleep(std::time::Duration::from_millis(60)).await;

    let found = state
        .get_flow(flow_id)
        .await
        .expect("flow should be available");
    let Layer::WebSocket(ws) = found.layer else {
        panic!("expected websocket flow");
    };
    assert_eq!(
        ws.messages.len(),
        2000,
        "ws message buffer should be capped"
    );
    assert_eq!(ws.messages[0].content.content, "m5");
    assert_eq!(ws.messages[1999].content.content, "m2004");
}

#[tokio::test]
async fn test_core_state_update_http_body_request_and_response() {
    let state = CoreState::new(None).await;
    let mut flow = create_test_flow("http://example.com/upload", "POST");
    if let Layer::Http(http) = &mut flow.layer {
        http.response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            cookies: vec![],
            body: None,
            timing: ResponseTiming {
                time_to_first_byte: None,
                time_to_last_byte: None,
                connect_time_ms: None,
                ssl_time_ms: None,
            },
        });
    }
    let flow_id = flow.id.to_string();
    state.upsert_flow(Box::new(flow));
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    state.update_http_body(
        flow_id.clone(),
        BodyData {
            encoding: "utf-8".to_string(),
            content: "request-body".to_string(),
            size: 12,
        },
        Direction::ClientToServer,
    );
    state.update_http_body(
        flow_id.clone(),
        BodyData {
            encoding: "utf-8".to_string(),
            content: "response-body".to_string(),
            size: 13,
        },
        Direction::ServerToClient,
    );
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    let found = state
        .get_flow(flow_id)
        .await
        .expect("flow should be available");
    let Layer::Http(http) = found.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.body.as_ref().map(|b| b.content.as_str()),
        Some("request-body")
    );
    assert_eq!(
        http.response
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .map(|b| b.content.as_str()),
        Some("response-body")
    );
}

#[tokio::test]
async fn test_core_state_update_http_body_missing_flow_is_noop() {
    let state = CoreState::new(None).await;
    state.update_http_body(
        "missing-flow".to_string(),
        BodyData {
            encoding: "utf-8".to_string(),
            content: "ignored".to_string(),
            size: 7,
        },
        Direction::ClientToServer,
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let found = state.get_flow("missing-flow".to_string()).await;
    assert!(found.is_none(), "missing flow should remain missing");
}

#[tokio::test]
async fn test_core_state_intercept_pending_and_resolve_missing_key() {
    let state = CoreState::new(None).await;
    let key = "flow-321:request_headers".to_string();
    let (tx, _rx) = oneshot::channel();
    state.register_intercept(key.clone(), tx).await;

    assert!(
        state.is_intercept_pending(key.clone()).await,
        "exact key should be reported pending"
    );
    assert!(
        !state
            .is_intercept_pending("flow-321:response_headers".to_string())
            .await,
        "different key should not be pending"
    );

    let missing = state
        .resolve_intercept("flow-321:missing".to_string(), InterceptionResult::Continue)
        .await;
    assert!(
        missing.is_err(),
        "resolving missing key should return error"
    );
}

#[tokio::test]
async fn test_core_state_metrics_reflect_flow_and_intercept_lifecycle() {
    let state = CoreState::new(None).await;

    let flow = create_test_flow("http://example.com/metrics", "GET");
    let flow_id = flow.id.to_string();
    state.upsert_flow(Box::new(flow));
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let key = format!("{}:request_headers", flow_id);
    let (tx, _rx) = oneshot::channel();
    state.register_intercept(key.clone(), tx).await;
    state
        .set_pending_ws_message(
            key.clone(),
            WebSocketMessage {
                id: Uuid::new_v4(),
                timestamp: Utc::now(),
                direction: Direction::ClientToServer,
                content: BodyData {
                    encoding: "utf-8".to_string(),
                    content: "metric-msg".to_string(),
                    size: 10,
                },
                opcode: "Text".to_string(),
            },
        )
        .await;

    let m1 = state.get_metrics().await;
    assert!(
        m1.flows_total >= 1,
        "flows_total should include upserted flow"
    );
    assert!(m1.flows_in_memory >= 1, "flow should be stored in memory");
    assert_eq!(m1.intercepts_pending, 1, "one intercept should be pending");
    assert_eq!(
        m1.ws_pending_messages, 1,
        "one pending websocket message should be tracked"
    );
    assert!(
        m1.oldest_intercept_age_ms.is_some(),
        "oldest intercept age should be present when pending intercept exists"
    );
    assert!(
        m1.oldest_ws_message_age_ms.is_some(),
        "oldest websocket pending age should be present when pending websocket message exists"
    );

    let resolved = state
        .resolve_intercept(key, InterceptionResult::Continue)
        .await;
    assert!(resolved.is_ok(), "intercept resolve should succeed");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let m2 = state.get_metrics().await;
    assert_eq!(m2.intercepts_pending, 0, "intercept should be cleared");
    assert_eq!(
        m2.ws_pending_messages, 0,
        "pending websocket message should be cleared with resolve"
    );
    assert!(
        m2.oldest_intercept_age_ms.is_none(),
        "oldest intercept age should be absent after pending intercept cleared"
    );
    assert!(
        m2.oldest_ws_message_age_ms.is_none(),
        "oldest websocket pending age should be absent after pending message cleared"
    );
}

#[tokio::test]
async fn test_core_state_metrics_initially_zeroed() {
    let state = CoreState::new(None).await;
    let m = state.get_metrics().await;
    assert_eq!(m.flows_total, 0);
    assert_eq!(m.flows_in_memory, 0);
    assert_eq!(m.flows_dropped, 0);
    assert_eq!(m.intercepts_pending, 0);
    assert_eq!(m.ws_pending_messages, 0);
    assert_eq!(m.oldest_intercept_age_ms, None);
    assert_eq!(m.oldest_ws_message_age_ms, None);
    assert_eq!(m.rule_exec_errors, 0);
    assert_eq!(m.audit_events_total, 0);
    assert_eq!(m.audit_events_failed, 0);
    assert_eq!(m.flow_events_lagged_total, 0);
    assert_eq!(m.audit_events_lagged_total, 0);
}

#[tokio::test]
async fn test_core_state_set_rules_replaces_previous_set() {
    let state = CoreState::new(None).await;
    let rule1 = Rule {
        id: "rule-old".to_string(),
        name: "old".to_string(),
        active: true,
        priority: 100,
        stage: RuleStage::RequestHeaders,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "x-old".to_string(),
            value: "1".to_string(),
        }],
        termination: RuleTermination::Continue,
        constraints: None,
    };
    let rule2 = Rule {
        id: "rule-new".to_string(),
        name: "new".to_string(),
        active: true,
        priority: 100,
        stage: RuleStage::RequestHeaders,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "x-new".to_string(),
            value: "1".to_string(),
        }],
        termination: RuleTermination::Continue,
        constraints: None,
    };

    state.set_rules(vec![rule1]).await;
    let first = state.get_rules().await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, "rule-old");

    state.set_rules(vec![rule2]).await;
    let second = state.get_rules().await;
    assert_eq!(second.len(), 1, "set_rules should replace existing rules");
    assert_eq!(second[0].id, "rule-new");
}

#[tokio::test]
async fn test_core_state_rule_engine_reflects_latest_rules() {
    let state = CoreState::new(None).await;
    let rule = Rule {
        id: "engine-rule".to_string(),
        name: "engine rule".to_string(),
        active: true,
        priority: 100,
        stage: RuleStage::RequestHeaders,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "x-engine".to_string(),
            value: "applied".to_string(),
        }],
        termination: RuleTermination::Continue,
        constraints: None,
    };
    state.set_rules(vec![rule]).await;

    let engine = state.get_rule_engine().await;
    let mut flow = create_test_flow("http://example.com/engine", "GET");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert!(
        http.request
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-engine") && v == "applied"),
        "rule engine should apply latest rule set"
    );
    assert!(
        matches!(ctx.summary, RuleTraceSummary::Modified { .. }),
        "executing rule should mark flow as modified"
    );
}
