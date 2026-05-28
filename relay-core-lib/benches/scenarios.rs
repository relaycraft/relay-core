use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use relay_core_api::flow::{
    BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
    TransportProtocol,
};
use relay_core_api::rule::{
    Action, Filter, Rule, RuleStage, RuleTermination, StringMatcher, WebSocketDirection,
};
use relay_core_lib::rule::RuleEngine;

use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

fn make_flow(url: &str) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        matched_rules: vec![],
        rule_variables: HashMap::new(),
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_ip: "93.184.216.34".to_string(),
            server_port: 80,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::Http(HttpLayer {
            request: HttpRequest {
                method: "GET".to_string(),
                url: url.parse().unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![
                    ("Host".to_string(), "example.com".to_string()),
                    ("User-Agent".to_string(), "bench/1.0".to_string()),
                ],
                body: None,
                cookies: vec![],
                query: vec![],
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: Some(BodyData {
                    encoding: "utf-8".to_string(),
                    size: 1024,
                    content: "x".repeat(1024),
                }),
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                cookies: vec![],
            }),
            error: None,
        }),
    }
}

fn make_rule(id: &str, pattern: &str, action: Action, stage: RuleStage) -> Rule {
    Rule {
        id: id.to_string(),
        name: format!("bench-{}", id),
        active: true,
        stage,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains(pattern.to_string())),
        actions: vec![action],
        constraints: None,
    }
}

// ── Rule evaluation: has_rules_for_stage ─────────────────────────────────

fn bench_rule_has_rules(c: &mut Criterion) {
    let mut group = c.benchmark_group("scenarios/has_rules");

    for n_rules in [1u32, 10, 50, 100] {
        let rules: Vec<Rule> = (0..n_rules)
            .map(|i| {
                make_rule(
                    &format!("r{}", i),
                    &format!("/path/{}", i),
                    Action::AddRequestHeader {
                        name: "X-Bench".to_string(),
                        value: "1".to_string(),
                    },
                    RuleStage::RequestHeaders,
                )
            })
            .collect();
        let engine = RuleEngine::new(rules, vec![], None, None);

        group.bench_with_input(
            BenchmarkId::new("request_headers", n_rules),
            &n_rules,
            |b, _| {
                b.iter(|| {
                    engine.has_rules_for_stage(black_box(RuleStage::RequestHeaders));
                });
            },
        );
    }
    group.finish();
}

// ── Rule execution: execute with MapRemote (variable substitution) ────────

fn bench_rule_execute(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("scenarios/execute");

    let actions = vec![Action::MapRemote {
        url: "https://backend-{{host}}/v2{{request.path}}".to_string(),
        preserve_host: false,
    }];

    for n_rules in [1u64, 5, 20] {
        let rules: Vec<Rule> = (0..n_rules)
            .map(|i| Rule {
                id: format!("r{}", i),
                name: format!("bench-{}", i),
                active: true,
                stage: RuleStage::RequestHeaders,
                priority: (100 - i as i32),
                termination: RuleTermination::Continue,
                filter: Filter::Url(StringMatcher::Contains("/api".to_string())),
                actions: actions.clone(),
                constraints: None,
            })
            .collect();
        let engine = RuleEngine::new(rules, vec![], None, None);

        group.bench_with_input(BenchmarkId::new("map_remote", n_rules), &n_rules, |b, _| {
            b.iter(|| {
                let mut flow = make_flow("http://example.com/api/users");
                let _ = rt.block_on(
                    engine.execute(black_box(RuleStage::RequestHeaders), black_box(&mut flow)),
                );
            });
        });
    }
    group.finish();
}

// ── WebSocket stage: has_rules check ─────────────────────────────────────

fn bench_ws_has_rules(c: &mut Criterion) {
    let mut group = c.benchmark_group("scenarios/ws_has_rules");

    for n_rules in [1u32, 5] {
        let rules: Vec<Rule> = (0..n_rules)
            .map(|i| {
                make_rule(
                    &format!("ws{}", i),
                    "ws",
                    Action::MockWebSocketMessage {
                        direction: WebSocketDirection::Incoming,
                        message: format!("bench-{}", i),
                    },
                    RuleStage::WebSocketMessage,
                )
            })
            .collect();
        let engine = RuleEngine::new(rules, vec![], None, None);

        group.bench_with_input(BenchmarkId::new("ws_message", n_rules), &n_rules, |b, _| {
            b.iter(|| {
                engine.has_rules_for_stage(black_box(RuleStage::WebSocketMessage));
            });
        });
    }
    group.finish();
}

// ── Connect stage: L3/L4 action ──────────────────────────────────────────

fn bench_connect_has_rules(c: &mut Criterion) {
    let rules = vec![make_rule(
        "connect",
        "192.168",
        Action::ForwardPort {
            target_host: "127.0.0.1".to_string(),
            target_port: 8443,
        },
        RuleStage::Connect,
    )];
    let engine = RuleEngine::new(rules, vec![], None, None);

    c.bench_function("scenarios/connect_has_rules", |b| {
        b.iter(|| {
            engine.has_rules_for_stage(black_box(RuleStage::Connect));
        });
    });
}

criterion_group!(
    benches,
    bench_rule_has_rules,
    bench_rule_execute,
    bench_ws_has_rules,
    bench_connect_has_rules,
);
criterion_main!(benches);
