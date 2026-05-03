use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use relay_core_api::flow::{Flow, Layer};
use relay_core_lib::rule::{Action, BodySource, Filter, Rule, RuleStage, RuleTermination, StringMatcher};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterceptRule {
    pub id: String,
    pub active: bool,
    pub url_pattern: String,
    pub method: Option<String>,
    pub phase: String, // "request", "response", "both", "ws_message"
}

#[derive(Debug, Clone)]
pub struct InterceptRuleConfig {
    pub rule_id: String,
    pub active: bool,
    pub url_pattern: String,
    pub method: Option<String>,
    pub phase: String,
    pub name: String,
    pub priority: i32,
    pub termination: RuleTermination,
}

#[derive(Debug, Clone)]
pub struct MockResponseRuleConfig {
    pub rule_id: String,
    pub url_pattern: String,
    pub name: String,
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

impl InterceptRule {
    pub fn matches(&self, flow: &Flow, phase: &str) -> bool {
        if !self.active {
            return false;
        }

        // Phase matching logic
        if self.phase == "both" {
            if phase == "ws_message" {
                return false;
            }
        } else if self.phase != phase {
            return false;
        }

        let url = match &flow.layer {
            Layer::Http(http) => Some(http.request.url.to_string()),
            Layer::WebSocket(ws) => Some(ws.handshake_request.url.to_string()),
            _ => None,
        };

        let method = match &flow.layer {
            Layer::Http(http) => Some(http.request.method.to_string()),
            Layer::WebSocket(ws) => Some(ws.handshake_request.method.to_string()),
            _ => None,
        };

        let url_str = url.as_deref().unwrap_or("");
        let method_str = method.as_deref().unwrap_or("");

        if let Some(m) = &self.method {
            if !m.eq_ignore_ascii_case(method_str) {
                return false;
            }
        }

        if let Ok(re) = Regex::new(&self.url_pattern) {
            if re.is_match(url_str) {
                return true;
            }
        } else if url_str.contains(&self.url_pattern) {
             return true;
        }

        false
    }

    pub fn to_rules(&self) -> Vec<Rule> {
        build_intercept_rules(InterceptRuleConfig {
            rule_id: self.id.clone(),
            active: self.active,
            url_pattern: self.url_pattern.clone(),
            method: self.method.clone(),
            phase: self.phase.clone(),
            name: format!("Legacy Rule {}", self.id),
            priority: 0,
            termination: RuleTermination::Continue,
        })
    }
}

pub fn build_intercept_rules(config: InterceptRuleConfig) -> Vec<Rule> {
    if !config.active {
        return vec![];
    }

    let InterceptRuleConfig {
        rule_id,
        url_pattern,
        method,
        phase,
        name,
        priority,
        termination,
        ..
    } = config;

    let stages = match phase.as_str() {
        "request" => vec![RuleStage::RequestHeaders],
        "response" => vec![RuleStage::ResponseHeaders],
        "ws_message" => vec![RuleStage::WebSocketMessage],
        "both" => vec![RuleStage::RequestHeaders, RuleStage::ResponseHeaders],
        _ => return vec![],
    };

    let url_filter = Filter::Url(build_url_matcher(url_pattern));
    let filter = if let Some(method) = method {
        Filter::And(vec![url_filter, Filter::Method(StringMatcher::Exact(method))])
    } else {
        url_filter
    };

    let stages_len = stages.len();
    stages
        .into_iter()
        .enumerate()
        .map(|(i, stage)| Rule {
            id: if stages_len > 1 {
                format!("{}-{}", rule_id, i)
            } else {
                rule_id.clone()
            },
            name: name.clone(),
            active: true,
            stage,
            priority,
            termination: termination.clone(),
            filter: filter.clone(),
            actions: vec![Action::Inspect],
            constraints: None,
        })
        .collect()
}

pub fn build_mock_response_rule(config: MockResponseRuleConfig) -> Rule {
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), config.content_type);

    Rule {
        id: config.rule_id,
        name: config.name,
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 200,
        termination: RuleTermination::Stop,
        filter: Filter::Url(build_url_matcher(config.url_pattern)),
        actions: vec![Action::MockResponse {
            status: config.status,
            headers,
            body: if config.body.is_empty() {
                None
            } else {
                Some(BodySource::Text(config.body))
            },
        }],
        constraints: None,
    }
}

fn build_url_matcher(url_pattern: String) -> StringMatcher {
    if Regex::new(&url_pattern).is_ok() {
        StringMatcher::Regex(url_pattern)
    } else {
        StringMatcher::Contains(url_pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InterceptRule, InterceptRuleConfig, MockResponseRuleConfig, build_intercept_rules,
        build_mock_response_rule,
    };
    use chrono::Utc;
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
        TransportProtocol, WebSocketLayer,
    };
    use relay_core_lib::rule::{Action, BodySource, Filter, RuleStage, RuleTermination, StringMatcher};
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn sample_http_flow(url: &str) -> Flow {
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
                    url: Url::parse(url).expect("url"),
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

    fn sample_ws_flow(url: &str) -> Flow {
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
                    url: Url::parse(url).expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
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
                    },
                    cookies: vec![],
                },
                messages: vec![],
                closed: false,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    #[test]
    fn test_to_rules_inactive_returns_empty() {
        let r = InterceptRule {
            id: "legacy-inactive".to_string(),
            active: false,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "request".to_string(),
        };
        assert!(r.to_rules().is_empty());
    }

    #[test]
    fn test_to_rules_invalid_phase_returns_empty() {
        let r = InterceptRule {
            id: "legacy-invalid".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "not-a-phase".to_string(),
        };
        assert!(r.to_rules().is_empty());
    }

    #[test]
    fn test_to_rules_both_phase_generates_two_stages_with_suffix_ids() {
        let r = InterceptRule {
            id: "legacy-both".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: Some("POST".to_string()),
            phase: "both".to_string(),
        };
        let rules = r.to_rules();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, "legacy-both-0");
        assert_eq!(rules[1].id, "legacy-both-1");
        assert_eq!(rules[0].stage, RuleStage::RequestHeaders);
        assert_eq!(rules[1].stage, RuleStage::ResponseHeaders);

        for rule in rules {
            match rule.filter {
                Filter::And(filters) => assert_eq!(filters.len(), 2),
                other => panic!("expected And filter for method+url, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_matches_both_phase_excludes_ws_message_phase() {
        let r = InterceptRule {
            id: "legacy-both-match".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "both".to_string(),
        };
        let http = sample_http_flow("http://example.com/path");
        let ws = sample_ws_flow("ws://example.com/socket");
        assert!(r.matches(&http, "request"));
        assert!(r.matches(&http, "response"));
        assert!(!r.matches(&ws, "ws_message"));
    }

    #[test]
    fn test_matches_invalid_regex_falls_back_to_contains() {
        let r = InterceptRule {
            id: "legacy-invalid-regex".to_string(),
            active: true,
            url_pattern: "[".to_string(),
            method: None,
            phase: "request".to_string(),
        };
        let flow_hit = sample_http_flow("http://example.com/x[1]");
        let flow_miss = sample_http_flow("http://example.com/x");
        assert!(r.matches(&flow_hit, "request"));
        assert!(!r.matches(&flow_miss, "request"));
    }

    #[test]
    fn test_build_intercept_rules_preserves_stop_and_priority() {
        let rules = build_intercept_rules(InterceptRuleConfig {
            rule_id: "probe-breakpoint".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "both".to_string(),
            name: "probe-intercept:example.com".to_string(),
            priority: 100,
            termination: RuleTermination::Stop,
        });

        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, "probe-breakpoint-0");
        assert_eq!(rules[1].id, "probe-breakpoint-1");
        assert_eq!(rules[0].priority, 100);
        assert!(matches!(rules[0].termination, RuleTermination::Stop));
        assert_eq!(rules[0].name, "probe-intercept:example.com");
    }

    #[test]
    fn test_build_intercept_rules_invalid_regex_falls_back_to_contains() {
        let rules = build_intercept_rules(InterceptRuleConfig {
            rule_id: "api-breakpoint".to_string(),
            active: true,
            url_pattern: "[".to_string(),
            method: Some("POST".to_string()),
            phase: "request".to_string(),
            name: "api-intercept:[".to_string(),
            priority: 100,
            termination: RuleTermination::Stop,
        });

        assert_eq!(rules.len(), 1);
        match &rules[0].filter {
            Filter::And(filters) => {
                assert!(matches!(filters[0], Filter::Url(StringMatcher::Contains(_))));
                assert!(matches!(filters[1], Filter::Method(StringMatcher::Exact(_))));
            }
            other => panic!("expected And filter for method+url, got {:?}", other),
        }
    }

    #[test]
    fn test_build_mock_response_rule_sets_mock_action_and_headers() {
        let rule = build_mock_response_rule(MockResponseRuleConfig {
            rule_id: "mock-rule".to_string(),
            url_pattern: "example.com".to_string(),
            name: "mock".to_string(),
            status: 201,
            content_type: "application/json".to_string(),
            body: "{\"ok\":true}".to_string(),
        });

        assert_eq!(rule.id, "mock-rule");
        assert_eq!(rule.stage, RuleStage::RequestHeaders);
        assert!(matches!(rule.termination, RuleTermination::Stop));
        match &rule.filter {
            Filter::Url(StringMatcher::Regex(pattern)) => assert_eq!(pattern, "example.com"),
            other => panic!("expected regex url filter, got {:?}", other),
        }
        match &rule.actions[0] {
            Action::MockResponse { status, headers, body } => {
                assert_eq!(*status, 201);
                assert_eq!(headers.get("Content-Type").map(String::as_str), Some("application/json"));
                match body {
                    Some(BodySource::Text(text)) => assert_eq!(text, "{\"ok\":true}"),
                    other => panic!("expected text body, got {:?}", other),
                }
            }
            other => panic!("expected mock response action, got {:?}", other),
        }
    }
}
