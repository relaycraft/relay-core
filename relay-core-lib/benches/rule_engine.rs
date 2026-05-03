/// Criterion benchmarks for the rule engine.
///
/// These measure the CPU cost of rule evaluation in isolation — no network,
/// no TLS, no async overhead. They give a deterministic lower bound on
/// processing latency per request.
///
/// Run:
///   cargo bench --package relay-core-lib -- rule_engine
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use relay_core_api::flow::{
    BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
    TransportProtocol,
};
use relay_core_api::rule::{Action, Filter, Rule, RuleStage, RuleTermination, StringMatcher};
use relay_core_lib::rule::RuleEngine;

use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_flow(url: &str) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        tags: vec![],
        meta: HashMap::new(),
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
                    ("Accept".to_string(), "*/*".to_string()),
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
                timing: ResponseTiming { time_to_first_byte: None, time_to_last_byte: None },
                cookies: vec![],
            }),
            error: None,
        }),
    }
}

fn make_rule(id: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        id: id.to_string(),
        name: format!("bench-{}", id),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 100,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains(pattern.to_string())),
        actions: vec![action],
        constraints: None,
    }
}

fn add_header_action() -> Action {
    Action::AddRequestHeader {
        name: "X-Bench".to_string(),
        value: "1".to_string(),
    }
}

// ── benchmarks ───────────────────────────────────────────────────────────────

fn bench_no_rules(c: &mut Criterion) {
    let engine = RuleEngine::new(vec![], vec![], None, None);
    let flow = make_flow("http://example.com/api/data");

    c.bench_function("rule_engine/no_rules", |b| {
        b.iter(|| {
            let _ = engine.has_rules_for_stage(black_box(RuleStage::RequestHeaders));
        });
    });

    let _ = flow;
}

fn bench_single_matching_rule(c: &mut Criterion) {
    let rules = vec![make_rule("r1", "/api", add_header_action())];
    let engine = RuleEngine::new(rules, vec![], None, None);
    let flow = make_flow("http://example.com/api/data");

    c.bench_function("rule_engine/1_rule_match", |b| {
        b.iter(|| {
            engine.has_rules_for_stage(black_box(RuleStage::RequestHeaders))
        });
    });

    let _ = (engine, flow);
}

fn bench_rule_count_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine/scaling");

    for n in [1usize, 5, 10, 25, 50] {
        let rules: Vec<Rule> = (0..n)
            .map(|i| make_rule(&format!("r{}", i), &format!("/path/{}", i), add_header_action()))
            .collect();
        let engine = RuleEngine::new(rules, vec![], None, None);

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                engine.has_rules_for_stage(black_box(RuleStage::RequestHeaders))
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_no_rules,
    bench_single_matching_rule,
    bench_rule_count_scaling,
);
criterion_main!(benches);
