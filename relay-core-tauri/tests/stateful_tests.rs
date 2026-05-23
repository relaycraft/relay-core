use chrono::Utc;
use relay_core_api::flow::{
    Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
    TransportProtocol, WebSocketLayer,
};
use relay_core_lib::rule::engine::RuleEngine;
use relay_core_lib::rule::engine::state::InMemoryRuleStateStore;
use relay_core_lib::rule::model::{
    Action, Filter, Rule, RuleOutcome, RuleStage, RuleTermination, RuleTraceSummary, StringMatcher,
    TerminalReason,
};
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

fn create_test_flow(url: &str) -> Flow {
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
                method: "GET".to_string(),
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
        resilience_trace: None,
    }
}

fn create_ws_flow(url: &str) -> Flow {
    let mut flow = create_test_flow(url);
    flow.layer = Layer::WebSocket(WebSocketLayer {
        handshake_request: HttpRequest {
            method: "GET".to_string(),
            url: Url::parse(url).unwrap(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("Host".to_string(), "origin.example.com".to_string())],
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
    });
    flow
}

#[tokio::test]
async fn test_rate_limit_action() {
    // 1. Setup State Store
    let state_store = Arc::new(InMemoryRuleStateStore::new());

    // 2. Define Rule with RateLimit
    let rule = Rule {
        id: "rate-limit-rule".to_string(),
        name: "Test Rate Limit".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All, // Match everything
        actions: vec![Action::RateLimit {
            key: "ip:{{client.ip}}".to_string(), // Spec-compliant variable syntax
            limit: 2,
            window_ms: 1000,
        }],
        constraints: None,
    };

    // 3. Create Engine with State Store
    let engine = RuleEngine::new(vec![rule], vec![], None, Some(state_store));

    // 4. Execution Loop
    let mut flow = create_test_flow("http://example.com");

    // First request - should pass (count = 1)
    let ctx1 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    println!("Ctx1 summary: {:?}", ctx1.summary);
    if let RuleTraceSummary::Terminated { .. } = ctx1.summary {
        panic!("First request should pass");
    }

    // Second request - should pass (count = 2)
    let ctx2 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    println!("Ctx2 summary: {:?}", ctx2.summary);
    if let RuleTraceSummary::Terminated { .. } = ctx2.summary {
        panic!("Second request should pass");
    }

    // Third request - should fail (count = 3 > 2)
    let ctx3 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    println!("Ctx3 summary: {:?}", ctx3.summary);

    match ctx3.summary {
        RuleTraceSummary::Terminated { rule_id, reason } => {
            assert_eq!(rule_id, "rate-limit-rule");
            match reason {
                TerminalReason::RateLimited => {} // OK
                _ => panic!("Wrong terminal reason: {:?}", reason),
            }
        }
        _ => panic!("Third request should be terminated, got {:?}", ctx3.summary),
    }
}

#[tokio::test]
async fn test_rate_limit_window_resets() {
    let state_store = Arc::new(InMemoryRuleStateStore::new());
    let rule = Rule {
        id: "rate-limit-window-reset".to_string(),
        name: "Rate Limit Window Reset".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::RateLimit {
            key: "rl:{{client.ip}}".to_string(),
            limit: 1,
            window_ms: 50,
        }],
        constraints: None,
    };
    let engine = RuleEngine::new(vec![rule], vec![], None, Some(state_store));
    let mut flow = create_test_flow("http://example.com");

    let ctx1 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        !matches!(ctx1.summary, RuleTraceSummary::Terminated { .. }),
        "first request should pass"
    );

    let ctx2 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        matches!(
            ctx2.summary,
            RuleTraceSummary::Terminated {
                reason: TerminalReason::RateLimited,
                ..
            }
        ),
        "second request in window should be rate-limited"
    );

    tokio::time::sleep(std::time::Duration::from_millis(70)).await;

    let ctx3 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        !matches!(ctx3.summary, RuleTraceSummary::Terminated { .. }),
        "request after window should pass again"
    );
}

#[tokio::test]
async fn test_map_remote_rewrites_url_and_host() {
    let rule = Rule {
        id: "map-remote-rule".to_string(),
        name: "Map Remote Test".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains("origin.example.com".to_string())),
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com:9443".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com:8080/path?a=1");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com:8080".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        matches!(ctx.summary, RuleTraceSummary::Modified { .. }),
        "map remote should be treated as modification"
    );

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.url.as_str(),
        "https://mirror.example.com:9443/path?a=1"
    );
    let host = http
        .request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "mirror.example.com:9443");
}

#[tokio::test]
async fn test_map_remote_preserve_host_keeps_original_header() {
    let rule = Rule {
        id: "map-remote-preserve-host".to_string(),
        name: "Map Remote Preserve Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com".to_string(),
            preserve_host: true,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));
    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(http.request.url.as_str(), "https://mirror.example.com/path");
    let host = http
        .request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "origin.example.com");
}

#[tokio::test]
async fn test_map_remote_invalid_url_records_failed_outcome() {
    let rule = Rule {
        id: "map-remote-invalid-url".to_string(),
        name: "Map Remote Invalid Url".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "://bad-url".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    assert_eq!(ctx.trace.len(), 1, "expected one rule trace event");
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("invalid url")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(
        matches!(ctx.summary, RuleTraceSummary::NoMatch),
        "failed action should not mark flow modified"
    );
}

#[tokio::test]
async fn test_map_remote_missing_host_records_failed_outcome() {
    let rule = Rule {
        id: "map-remote-missing-host".to_string(),
        name: "Map Remote Missing Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "ws:no-host".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    assert_eq!(ctx.trace.len(), 1, "expected one rule trace event");
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("scheme://host")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(matches!(ctx.summary, RuleTraceSummary::NoMatch));
}

#[tokio::test]
async fn test_map_remote_unsupported_scheme_records_failed_outcome() {
    let rule = Rule {
        id: "map-remote-unsupported-scheme".to_string(),
        name: "Map Remote Unsupported Scheme".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "ftp://mirror.example.com/resource".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    assert_eq!(ctx.trace.len(), 1, "expected one rule trace event");
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("unsupported scheme")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(matches!(ctx.summary, RuleTraceSummary::NoMatch));
}

#[tokio::test]
async fn test_map_remote_userinfo_records_failed_outcome() {
    let rule = Rule {
        id: "map-remote-userinfo".to_string(),
        name: "Map Remote Userinfo".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://user:pass@mirror.example.com/path".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    assert_eq!(ctx.trace.len(), 1, "expected one rule trace event");
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("must not include userinfo")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(matches!(ctx.summary, RuleTraceSummary::NoMatch));
}

#[tokio::test]
async fn test_map_remote_websocket_userinfo_records_failed_outcome() {
    let rule = Rule {
        id: "map-remote-ws-userinfo".to_string(),
        name: "Map Remote WS Userinfo".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://user:pass@mirror.example.com/path".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/path");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    assert_eq!(ctx.trace.len(), 1, "expected one rule trace event");
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("must not include userinfo")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(matches!(ctx.summary, RuleTraceSummary::NoMatch));
}

#[tokio::test]
async fn test_map_remote_websocket_rewrites_handshake_url_and_host() {
    let rule = Rule {
        id: "map-remote-ws".to_string(),
        name: "Map Remote WS".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://mirror.example.com:9666".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/chat?room=1");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    assert_eq!(
        ws.handshake_request.url.as_str(),
        "wss://mirror.example.com:9666/chat?room=1"
    );
    let host = ws
        .handshake_request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "mirror.example.com:9666");
}

#[tokio::test]
async fn test_map_remote_websocket_preserve_host_keeps_handshake_host() {
    let rule = Rule {
        id: "map-remote-ws-preserve".to_string(),
        name: "Map Remote WS Preserve".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://mirror.example.com".to_string(),
            preserve_host: true,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/chat");
    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    assert_eq!(
        ws.handshake_request.url.as_str(),
        "wss://mirror.example.com/chat"
    );
    let host = ws
        .handshake_request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "origin.example.com");
}

#[tokio::test]
async fn test_set_variable_then_map_remote_across_rules_for_websocket() {
    let rule_set_var = Rule {
        id: "ws-setvar-high-priority".to_string(),
        name: "WS Set upstream from request host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::SetVariable {
            name: "upstream".to_string(),
            value: "{{request.host}}:9778".to_string(),
        }],
        constraints: None,
    };

    let rule_map_remote = Rule {
        id: "ws-mapremote-low-priority".to_string(),
        name: "WS Map using upstream variable".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 10,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://{{upstream}}".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule_map_remote, rule_set_var], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/chat?room=2");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.handshake_request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    assert_eq!(
        ws.handshake_request.url.as_str(),
        "wss://origin.example.com:9778/chat?room=2"
    );
    let host = ws
        .handshake_request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "origin.example.com:9778");
}

#[tokio::test]
async fn test_set_variable_then_map_remote_in_same_rule() {
    let rule = Rule {
        id: "setvar-mapremote".to_string(),
        name: "SetVariable + MapRemote".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![
            Action::SetVariable {
                name: "upstream".to_string(),
                value: "mirror.example.com:9777".to_string(),
            },
            Action::MapRemote {
                url: "https://{{upstream}}".to_string(),
                preserve_host: false,
            },
        ],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/v1/items?id=1");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.url.as_str(),
        "https://mirror.example.com:9777/v1/items?id=1"
    );
    let host = http
        .request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "mirror.example.com:9777");
}

#[tokio::test]
async fn test_set_variable_then_map_remote_across_rules_by_priority() {
    let rule_set_var = Rule {
        id: "setvar-high-priority".to_string(),
        name: "Set upstream from host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::SetVariable {
            name: "upstream".to_string(),
            value: "{{request.host}}:9888".to_string(),
        }],
        constraints: None,
    };

    let rule_map_remote = Rule {
        id: "mapremote-low-priority".to_string(),
        name: "Map using upstream variable".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 10,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://{{upstream}}".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule_map_remote, rule_set_var], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/v2/list?page=1");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.url.as_str(),
        "https://origin.example.com:9888/v2/list?page=1"
    );
    let host = http
        .request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "origin.example.com:9888");
}

#[tokio::test]
async fn test_map_remote_with_stop_prevents_lower_priority_rule_execution() {
    let rule_primary = Rule {
        id: "mapremote-stop".to_string(),
        name: "MapRemote with Stop".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Stop,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://primary.example.com:9444".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let rule_secondary = Rule {
        id: "lower-priority-should-not-run".to_string(),
        name: "Lower priority marker".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "X-Should-Not-Exist".to_string(),
            value: "true".to_string(),
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule_secondary, rule_primary], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/data?q=1");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    match &ctx.summary {
        RuleTraceSummary::Modified { rule_ids } => {
            assert_eq!(rule_ids, &vec!["mapremote-stop".to_string()]);
        }
        other => panic!("expected Modified summary, got {:?}", other),
    }

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.url.as_str(),
        "https://primary.example.com:9444/data?q=1"
    );
    let blocked = http
        .request
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("X-Should-Not-Exist"));
    assert!(
        !blocked,
        "lower-priority rule should not execute after Stop"
    );
}

#[tokio::test]
async fn test_map_remote_websocket_with_stop_prevents_lower_priority_rule_execution() {
    let rule_primary = Rule {
        id: "mapremote-ws-stop".to_string(),
        name: "WS MapRemote with Stop".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Stop,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://primary.example.com:9444".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let rule_secondary = Rule {
        id: "ws-lower-priority-should-not-run".to_string(),
        name: "WS lower priority marker".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::AddRequestHeader {
            name: "X-WS-Should-Not-Exist".to_string(),
            value: "true".to_string(),
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule_secondary, rule_primary], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/chat?room=1");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.handshake_request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    match &ctx.summary {
        RuleTraceSummary::Modified { rule_ids } => {
            assert_eq!(rule_ids, &vec!["mapremote-ws-stop".to_string()]);
        }
        other => panic!("expected Modified summary, got {:?}", other),
    }

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    assert_eq!(
        ws.handshake_request.url.as_str(),
        "wss://primary.example.com:9444/chat?room=1"
    );
    let blocked = ws
        .handshake_request
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("X-WS-Should-Not-Exist"));
    assert!(
        !blocked,
        "lower-priority websocket rule should not execute after Stop"
    );
}

#[tokio::test]
async fn test_rate_limit_zero_window_is_effectively_min_window() {
    let state_store = Arc::new(InMemoryRuleStateStore::new());
    let rule = Rule {
        id: "rate-limit-zero-window".to_string(),
        name: "Rate Limit Zero Window".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::RateLimit {
            key: "zero:{{client.ip}}".to_string(),
            limit: 1,
            window_ms: 0,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, Some(state_store));
    let mut flow = create_test_flow("http://example.com");

    let ctx1 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(!matches!(ctx1.summary, RuleTraceSummary::Terminated { .. }));

    let ctx2 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        matches!(
            ctx2.summary,
            RuleTraceSummary::Terminated {
                reason: TerminalReason::RateLimited,
                ..
            }
        ),
        "second request should be limited even when window_ms=0"
    );

    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    let ctx3 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(
        !matches!(ctx3.summary, RuleTraceSummary::Terminated { .. }),
        "counter should reset after minimal fallback window"
    );
}

#[tokio::test]
async fn test_map_remote_target_path_does_not_override_original_path() {
    let rule = Rule {
        id: "map-remote-target-path".to_string(),
        name: "Map Remote Target Path".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com:9443/new/base?x=9".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/original/path?a=1");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    assert_eq!(
        http.request.url.as_str(),
        "https://mirror.example.com:9443/original/path?a=1"
    );
}

#[tokio::test]
async fn test_map_remote_http_deduplicates_host_headers_stateful() {
    let rule = Rule {
        id: "map-remote-http-dedup".to_string(),
        name: "Map Remote HTTP Host Dedup".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com:9001".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/a");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![
            ("Host".to_string(), "origin.example.com".to_string()),
            ("host".to_string(), "origin2.example.com".to_string()),
            ("X-Keep".to_string(), "ok".to_string()),
        ];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    let host_values: Vec<_> = http
        .request
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(host_values, vec!["mirror.example.com:9001".to_string()]);
    assert!(
        http.request
            .headers
            .iter()
            .any(|(k, v)| k == "X-Keep" && v == "ok")
    );
}

#[tokio::test]
async fn test_map_remote_http_ipv6_target_sets_bracketed_host() {
    let rule = Rule {
        id: "map-remote-http-ipv6-host".to_string(),
        name: "Map Remote HTTP IPv6 Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://[2001:db8::2]:9443".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/a");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    let host = http
        .request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "[2001:db8::2]:9443");
}

#[tokio::test]
async fn test_map_remote_websocket_ipv6_target_sets_bracketed_host() {
    let rule = Rule {
        id: "map-remote-ws-ipv6-host".to_string(),
        name: "Map Remote WS IPv6 Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://[2001:db8::4]:9443".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/ws");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.handshake_request.headers = vec![("Host".to_string(), "origin.example.com".to_string())];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    let host = ws
        .handshake_request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .expect("host header should exist");
    assert_eq!(host, "[2001:db8::4]:9443");
}

#[tokio::test]
async fn test_map_remote_websocket_deduplicates_host_headers_stateful() {
    let rule = Rule {
        id: "map-remote-ws-dedup".to_string(),
        name: "Map Remote WS Host Dedup".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://mirror.example.com:9555".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/ws");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.handshake_request.headers = vec![
            ("Host".to_string(), "origin.example.com".to_string()),
            ("host".to_string(), "origin2.example.com".to_string()),
            ("Upgrade".to_string(), "websocket".to_string()),
        ];
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    let host_values: Vec<_> = ws
        .handshake_request
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(host_values, vec!["mirror.example.com:9555".to_string()]);
    assert!(
        ws.handshake_request
            .headers
            .iter()
            .any(|(k, v)| k == "Upgrade" && v == "websocket")
    );
}

#[tokio::test]
async fn test_rate_limit_legacy_client_ip_alias_still_works() {
    let state_store = Arc::new(InMemoryRuleStateStore::new());
    let rule = Rule {
        id: "rate-limit-legacy-alias".to_string(),
        name: "Rate Limit Legacy Alias".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::RateLimit {
            key: "legacy:{{client_ip}}".to_string(),
            limit: 1,
            window_ms: 1000,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, Some(state_store));
    let mut flow = create_test_flow("http://example.com");

    let ctx1 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(!matches!(ctx1.summary, RuleTraceSummary::Terminated { .. }));

    let ctx2 = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(
        ctx2.summary,
        RuleTraceSummary::Terminated {
            reason: TerminalReason::RateLimited,
            ..
        }
    ));
}

#[tokio::test]
async fn test_map_remote_http_preserve_host_does_not_insert_when_missing() {
    let rule = Rule {
        id: "map-remote-http-preserve-missing-host".to_string(),
        name: "Map Remote HTTP Preserve Missing Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com".to_string(),
            preserve_host: true,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_test_flow("http://origin.example.com/path");
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers.clear();
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::Http(http) = &flow.layer else {
        panic!("expected http flow");
    };
    let has_host = http
        .request
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("host"));
    assert!(
        !has_host,
        "preserve_host=true should not inject missing Host"
    );
}

#[tokio::test]
async fn test_map_remote_websocket_preserve_host_does_not_insert_when_missing() {
    let rule = Rule {
        id: "map-remote-ws-preserve-missing-host".to_string(),
        name: "Map Remote WS Preserve Missing Host".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![Action::MapRemote {
            url: "wss://mirror.example.com".to_string(),
            preserve_host: true,
        }],
        constraints: None,
    };

    let engine = RuleEngine::new(vec![rule], vec![], None, None);
    let mut flow = create_ws_flow("ws://origin.example.com/ws");
    if let Layer::WebSocket(ws) = &mut flow.layer {
        ws.handshake_request.headers.clear();
    }

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert!(matches!(ctx.summary, RuleTraceSummary::Modified { .. }));

    let Layer::WebSocket(ws) = &flow.layer else {
        panic!("expected websocket flow");
    };
    let has_host = ws
        .handshake_request
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("host"));
    assert!(
        !has_host,
        "preserve_host=true should not inject missing Host"
    );
}
