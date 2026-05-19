use chrono::Utc;
use relay_core_api::flow::{
    BodyData, Direction, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
    ResponseTiming, TransportProtocol, WebSocketLayer, WebSocketMessage,
};
use relay_core_lib::InterceptionResult;
use relay_core_lib::rule::{
    Action, Filter, Rule, RuleOutcome, RuleStage, RuleTermination, RuleTraceSummary, StringMatcher,
    TerminalReason, WebSocketDirection,
};
use relay_core_lib::rule_engine::RuleEngine;
use relay_core_tauri::RelayCoreState;
use relay_core_tauri::commands::{Modification, resume_flow_impl, set_intercept_rule_impl};
use relay_core_tauri::interceptor::InterceptRule;
use std::collections::HashMap;
use tokio::sync::oneshot;
use url::Url;
use uuid::Uuid;

// Helper to create a dummy flow
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
                url: Url::parse(url).unwrap(),
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
    }
}

fn create_ws_flow(url: &str) -> Flow {
    let mut flow = create_test_flow(url, "GET");
    flow.layer = Layer::WebSocket(WebSocketLayer {
        handshake_request: HttpRequest {
            method: "GET".to_string(),
            url: Url::parse(url).unwrap(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("Upgrade".to_string(), "websocket".to_string())],
            body: None,
            cookies: vec![],
            query: vec![],
        },
        handshake_response: HttpResponse {
            status: 101,
            status_text: "Switching Protocols".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: None,
            timing: ResponseTiming {
                time_to_first_byte: None,
                time_to_last_byte: None,
                connect_time_ms: None,
                ssl_time_ms: None,
            },
            cookies: vec![],
        },
        messages: vec![],
        closed: false,
    });
    flow
}

#[test]
fn test_rule_matching_logic() {
    let rule = InterceptRule {
        id: "rule-1".to_string(),
        active: true,
        url_pattern: "example.com".to_string(),
        method: Some("POST".to_string()),
        phase: "request".to_string(),
    };

    // Case 1: Match
    let flow_match = create_test_flow("http://example.com/api", "POST");
    assert!(rule.matches(&flow_match, "request"));

    // Case 2: Mismatch URL
    let flow_mismatch_url = create_test_flow("http://google.com/api", "POST");
    assert!(!rule.matches(&flow_mismatch_url, "request"));

    // Case 3: Mismatch Method
    let flow_mismatch_method = create_test_flow("http://example.com/api", "GET");
    assert!(!rule.matches(&flow_mismatch_method, "request"));

    // Case 4: Mismatch Phase
    assert!(!rule.matches(&flow_match, "response"));

    // Case 5: Regex Match
    let regex_rule = InterceptRule {
        id: "rule-2".to_string(),
        active: true,
        url_pattern: r".*/api/v\d/.*".to_string(), // Matches /api/v1/, /api/v2/
        method: None,
        phase: "both".to_string(),
    };

    let flow_v1 = create_test_flow("http://test.com/api/v1/users", "GET");
    assert!(regex_rule.matches(&flow_v1, "request"));
    assert!(regex_rule.matches(&flow_v1, "response"));

    let flow_v3 = create_test_flow("http://test.com/api/v3/users", "GET");
    assert!(regex_rule.matches(&flow_v3, "request"));

    let flow_no_match = create_test_flow("http://test.com/api/users", "GET");
    assert!(!regex_rule.matches(&flow_no_match, "request"));
}

#[tokio::test]
async fn test_interception_workflow() {
    // 1. Setup State
    let state = RelayCoreState::new_async().await;

    // 2. Add Rule
    let rule = InterceptRule {
        id: "rule-1".to_string(),
        active: true,
        url_pattern: "example.com".to_string(),
        method: None,
        phase: "request".to_string(),
    };
    set_intercept_rule_impl(&state, rule).await.unwrap();

    // 3. Simulate Flow Arrival & Interception
    let flow = create_test_flow("http://example.com/target", "GET");
    let flow_id = flow.id.to_string();

    // Manually register flow in state (simulating proxy loop)
    state.core.upsert_flow(Box::new(flow.clone()));

    // Check if it should match (using the rule logic directly for now, simulating Interceptor::on_request)
    // We already tested rule matching logic above, so we assume the interceptor would catch it.
    // We manually trigger the "Pause" state.

    let (tx, rx) = oneshot::channel();
    let intercept_key = format!("{}:request", flow_id);

    state
        .core
        .register_intercept(intercept_key.clone(), tx)
        .await;

    // 4. Simulate Frontend Resume with Modification
    let mods = Modification {
        method: Some("DELETE".to_string()),
        url: None,
        request_headers: None,
        request_body: None,
        status_code: None,
        response_headers: None,
        response_body: None,
        message_content: None,
    };

    let resume_res = resume_flow_impl(
        &state,
        intercept_key.clone(),
        "continue".to_string(),
        Some(mods),
    )
    .await;
    assert!(resume_res.is_ok());

    // 5. Verify Interceptor received the result
    let result = rx.await.unwrap();
    match result {
        InterceptionResult::ModifiedRequest(req) => {
            assert_eq!(req.method, "DELETE");
        }
        _ => panic!("Expected ModifiedRequest"),
    }

    // 6. Verify pending map is cleared
    let is_pending = state.core.is_intercept_pending(intercept_key.clone()).await;
    assert!(!is_pending);
}

#[tokio::test]
async fn test_websocket_interception_workflow() {
    // 1. Setup State
    let state = RelayCoreState::new_async().await;

    // 2. Add Rule (WebSocket Message Phase)
    let rule = InterceptRule {
        id: "ws-rule".to_string(),
        active: true,
        url_pattern: "socket".to_string(),
        method: None,
        phase: "ws_message".to_string(),
    };
    set_intercept_rule_impl(&state, rule.clone()).await.unwrap();

    // 3. Create WS Flow and Message
    let flow = create_ws_flow("ws://example.com/socket");
    let flow_id = flow.id.to_string();

    // Insert flow first
    state.core.upsert_flow(Box::new(flow.clone()));

    // Verify rule matches
    assert!(rule.matches(&flow, "ws_message"));

    let msg = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "text".to_string(),
            content: "ping".to_string(),
            size: 4,
        },
        opcode: "Text".to_string(),
    };
    let msg_id = msg.id.to_string();
    let intercept_key = format!("{}:ws_msg:{}", flow_id, msg_id);

    // 4. Simulate Pause State
    let (tx, rx) = oneshot::channel();
    state
        .core
        .register_intercept(intercept_key.clone(), tx)
        .await;

    // Also set pending ws message
    state
        .core
        .set_pending_ws_message(intercept_key.clone(), msg.clone())
        .await;

    // 5. Resume with Modification
    let mods = Modification {
        method: None,
        url: None,
        request_headers: None,
        request_body: None,
        status_code: None,
        response_headers: None,
        response_body: None,
        message_content: Some("pong-modified".to_string()),
    };

    let resume_res = resume_flow_impl(
        &state,
        intercept_key.clone(),
        "continue".to_string(),
        Some(mods),
    )
    .await;
    assert!(resume_res.is_ok());

    // 6. Verify Result
    let result = rx.await.unwrap();
    match result {
        InterceptionResult::ModifiedMessage(new_msg) => {
            assert_eq!(new_msg.content.content, "pong-modified");
        }
        _ => panic!("Expected ModifiedMessage"),
    }
}

#[tokio::test]
async fn test_rule_engine_full_link_assertion() {
    // 1. Setup Rule (Modify Header)
    let rule = Rule {
        id: "rule-full-link".to_string(),
        name: "Full Link Test".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains("example.com".to_string())),
        actions: vec![Action::AddRequestHeader {
            name: "X-Trace-Test".to_string(),
            value: "verified".to_string(),
        }],
        constraints: None,
    };

    // 2. Setup Engine
    let engine = RuleEngine::new(vec![rule], vec![], None, None);

    // 3. Setup Flow
    let mut flow = create_test_flow("http://example.com/api/test", "GET");
    let flow_id = flow.id;

    // 4. Execute
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    // 5. Verify Flow Mutation
    if let Layer::Http(http) = &flow.layer {
        let header_val = http
            .request
            .headers
            .iter()
            .find(|(k, _)| k == "X-Trace-Test")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            header_val,
            Some("verified"),
            "Flow mutation failed: Header not found or incorrect"
        );
    } else {
        panic!("Invalid flow layer");
    }

    // 6. Verify Trace (Full Link Assertion)
    // Expect: 1 event, Rule Match, Action Executed, Outcome Success
    assert_eq!(ctx.trace.len(), 1, "Expected 1 trace event");
    let event = &ctx.trace[0];
    assert_eq!(event.rule_id, "rule-full-link");
    assert_eq!(event.stage, RuleStage::RequestHeaders);

    if let RuleOutcome::MatchedAndExecuted = &event.outcome {
        // Success
    } else {
        panic!("Rule execution failed: {:?}", event.outcome);
    }

    match &ctx.summary {
        RuleTraceSummary::Modified { rule_ids } => {
            assert_eq!(rule_ids.len(), 1);
            assert_eq!(rule_ids[0], "rule-full-link");
        }
        _ => panic!("Expected Modified summary, got {:?}", ctx.summary),
    }

    println!(
        "Offline Link Assertion Passed: Flow mutated and Trace verified for Flow ID {}",
        flow_id
    );
}

#[tokio::test]
async fn test_rule_engine_binary_websocket_mock() {
    // 1. Setup Rule (Mock WS Message)
    let rule = Rule {
        id: "rule-ws-mock".to_string(),
        name: "WS Mock Test".to_string(),
        active: true,
        stage: RuleStage::WebSocketMessage,
        priority: 1,
        termination: RuleTermination::Stop,
        filter: Filter::WebSocketMessage(StringMatcher::Contains("base64_content".to_string())),
        actions: vec![Action::MockWebSocketMessage {
            direction: WebSocketDirection::Incoming,
            message: "mocked_text".to_string(),
        }],
        constraints: None,
    }; // 2. Setup Engine
    let engine = RuleEngine::new(vec![rule], vec![], None, None);

    // 2. Setup WS Flow with Binary Message
    let mut flow = create_ws_flow("ws://example.com/socket");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.messages.push(WebSocketMessage {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            direction: Direction::ServerToClient,
            content: BodyData {
                encoding: "base64".to_string(),
                content: "base64_content".to_string(),
                size: 14,
            },
            opcode: "Binary".to_string(),
        });
    }

    // 3. Execute
    let ctx = engine.execute(RuleStage::WebSocketMessage, &mut flow).await;

    // 4. Verify Flow Mutation
    if let Layer::WebSocket(ws) = &flow.layer {
        let msg = ws.messages.last().unwrap();
        assert_eq!(msg.content.content, "mocked_text");
        assert_eq!(msg.content.encoding, "utf-8");
        assert_eq!(msg.opcode, "Text", "Opcode should be updated to Text");
    } else {
        panic!("Invalid flow layer");
    }

    // 5. Verify Trace
    assert_eq!(ctx.trace.len(), 1);
    let event = &ctx.trace[0];
    if let RuleOutcome::MatchedAndTerminated = &event.outcome {
        // Outcome is correct
    } else {
        panic!("Rule execution failed: {:?}", event.outcome);
    }

    match &ctx.summary {
        RuleTraceSummary::Terminated { reason, .. } => {
            assert_eq!(reason, &TerminalReason::Mock);
        }
        _ => panic!("Expected Terminated summary, got {:?}", ctx.summary),
    }
}

#[tokio::test]
async fn test_rule_engine_websocket_modification() {
    use relay_core_lib::rule::WebSocketDirection;

    // 1. Setup Rule (Mock WS Message)
    let rule = Rule {
        id: "rule-ws-mod".to_string(),
        name: "WS Modification Test".to_string(),
        active: true,
        stage: RuleStage::WebSocketMessage,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains("socket".to_string())),
        actions: vec![Action::MockWebSocketMessage {
            direction: WebSocketDirection::Outgoing,
            message: "mocked-message".to_string(),
        }],
        constraints: None,
    };

    // 2. Setup Engine
    let engine = RuleEngine::new(vec![rule], vec![], None, None);

    // 3. Setup Flow
    let mut flow = create_ws_flow("ws://example.com/socket");
    let flow_id = flow.id;

    // Add a message to be modified
    let msg = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "text".to_string(),
            content: "original".to_string(),
            size: 8,
        },
        opcode: "Text".to_string(),
    };
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.messages.push(msg);
    }

    // 4. Execute
    let ctx = engine.execute(RuleStage::WebSocketMessage, &mut flow).await;

    // 5. Verify Flow Mutation
    if let Layer::WebSocket(ws) = &flow.layer {
        let last_msg = ws.messages.last().unwrap();
        assert_eq!(last_msg.content.content, "mocked-message");
        // Direction should be ClientToServer (Outgoing)
        matches!(last_msg.direction, Direction::ClientToServer);
    } else {
        panic!("Invalid flow layer");
    }

    // 6. Verify Trace
    assert_eq!(ctx.trace.len(), 1, "Expected 1 trace event");
    let event = &ctx.trace[0];
    assert_eq!(event.rule_id, "rule-ws-mod");

    if let RuleOutcome::MatchedAndTerminated = &event.outcome {
        // Success
    } else {
        panic!("Rule execution failed: {:?}", event.outcome);
    }

    // Verify Termination Reason
    if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
        match reason {
            TerminalReason::Mock => {}
            _ => panic!("Expected Mock termination reason, got {:?}", reason),
        }
    } else {
        panic!("Expected Terminated summary, got {:?}", ctx.summary);
    }

    println!(
        "WebSocket Rule Engine Assertion Passed: Message mocked and Trace verified for Flow ID {}",
        flow_id
    );
}

#[tokio::test]
async fn test_rule_engine_websocket_mock() {
    use relay_core_lib::rule::WebSocketDirection;

    // 1. Setup Rule (Mock WS Message)
    let rule = Rule {
        id: "rule-ws-mock".to_string(),
        name: "WS Mock Test".to_string(),
        active: true,
        stage: RuleStage::WebSocketMessage,
        priority: 1,
        termination: RuleTermination::Stop,
        filter: Filter::WebSocketMessage(StringMatcher::Contains("ping".to_string())),
        actions: vec![Action::MockWebSocketMessage {
            direction: WebSocketDirection::Outgoing,
            message: "pong-mocked".to_string(),
        }],
        constraints: None,
    };

    // 2. Setup Engine
    let engine = RuleEngine::new(vec![rule], vec![], None, None);

    // 3. Setup Flow
    let mut flow = create_ws_flow("ws://example.com/socket");

    // Add a message to the flow
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.messages.push(WebSocketMessage {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            direction: Direction::ClientToServer,
            content: BodyData {
                encoding: "text".to_string(),
                content: "ping-original".to_string(),
                size: 13,
            },
            opcode: "Text".to_string(),
        });
    }

    // 4. Execute
    let ctx = engine.execute(RuleStage::WebSocketMessage, &mut flow).await;

    // 5. Verify Flow Mutation
    if let Layer::WebSocket(ws) = &flow.layer {
        let msg = ws.messages.last().expect("Message should exist");
        assert_eq!(
            msg.content.content, "pong-mocked",
            "Message content mismatch"
        );
        assert_eq!(
            msg.direction,
            Direction::ClientToServer,
            "Message direction mismatch"
        ); // Outgoing -> ClientToServer
    } else {
        panic!("Invalid flow layer");
    }

    // 6. Verify Trace
    assert_eq!(ctx.trace.len(), 1, "Expected 1 trace event");
    let event = &ctx.trace[0];
    assert_eq!(event.rule_id, "rule-ws-mock");
    assert_eq!(event.stage, RuleStage::WebSocketMessage);

    match &ctx.summary {
        RuleTraceSummary::Terminated { rule_id, reason } => {
            assert_eq!(rule_id, "rule-ws-mock");
            assert_eq!(reason, &relay_core_lib::rule::TerminalReason::Mock);
        }
        _ => panic!("Expected Terminated(Mock) summary, got {:?}", ctx.summary),
    }

    println!("WS Mock Assertion Passed");
}

#[test]
fn test_intercept_rule_conversion() {
    use relay_core_lib::rule::{Action, Filter, RuleStage, StringMatcher};
    use relay_core_runtime::rule::InterceptRule;

    let intercept_rule = InterceptRule {
        id: "legacy-rule-1".to_string(),
        active: true,
        url_pattern: "example.com".to_string(),
        method: Some("POST".to_string()),
        phase: "request".to_string(),
    };

    let rules = intercept_rule.to_rules();
    assert_eq!(rules.len(), 1);

    let rule = &rules[0];
    assert_eq!(rule.stage, RuleStage::RequestHeaders);
    assert_eq!(rule.actions.len(), 1);
    assert!(matches!(rule.actions[0], Action::Inspect));

    match &rule.filter {
        Filter::And(filters) => {
            assert_eq!(filters.len(), 2);
            // Check URL filter
            match &filters[0] {
                Filter::Url(StringMatcher::Regex(s)) => assert_eq!(s, "example.com"),
                _ => panic!("Expected URL Regex filter"),
            }
            // Check Method filter
            match &filters[1] {
                Filter::Method(StringMatcher::Exact(s)) => assert_eq!(s, "POST"),
                _ => panic!("Expected Method Exact filter"),
            }
        }
        _ => panic!("Expected And filter"),
    }
}

#[tokio::test]
async fn test_rule_engine_inspect_action() {
    use relay_core_lib::rule::{
        Action, Filter, Rule, RuleStage, RuleTermination, RuleTraceSummary, StringMatcher,
        TerminalReason,
    };
    use relay_core_lib::rule_engine::RuleEngine;

    // 1. Setup Rule (Inspect)
    let rule = Rule {
        id: "rule-inspect".to_string(),
        name: "Inspect Test".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue, // Inspect usually terminates, but let's see engine behavior
        filter: Filter::Url(StringMatcher::Contains("example.com".to_string())),
        actions: vec![Action::Inspect],
        constraints: None,
    };

    // 2. Setup Engine
    let engine = RuleEngine::new(vec![rule], vec![], None, None);

    // 3. Setup Flow
    let mut flow = create_test_flow("http://example.com/api/test", "GET");

    // 4. Execute
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    // 5. Verify Termination
    match &ctx.summary {
        RuleTraceSummary::Terminated { rule_id, reason } => {
            assert_eq!(rule_id, "rule-inspect");
            assert_eq!(reason, &TerminalReason::Inspect);
        }
        _ => panic!(
            "Expected Terminated(Inspect) summary, got {:?}",
            ctx.summary
        ),
    }
}
