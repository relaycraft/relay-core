//! The rule vocabulary an agent needs before it calls `set_rule`.
//!
//! The examples are serialized `Filter` and `Action` values, and the stage lists come from the
//! same validator the engine uses. Adding a variant fails to compile until it is added to the
//! catalog macro, which is what puts it in the guide.

use super::engine::compiler::compile_filter;
use super::engine::validator::{validate_action_stage, validate_filter_stage};
use super::{
    Action, BodySource, BodyTransform, Filter, Rule, RuleStage, RuleTermination, StringMatcher,
    WebSocketDirection,
};
use std::collections::HashMap;

/// Markdown an agent reads before writing a rule. The JSON blocks are the serde shape.
pub fn rule_api_guide() -> String {
    let mut out = String::new();
    out.push_str(
        "# Rule API\n\n\
         Pass one object as `set_rule`'s `rule`. `actions` is an array of the action objects below. \
         One action object is rejected. `filter` is one filter object. `constraints` may be null, \
         or `{ \"timeout_ms\": 1000 }`.\n\n\
         `termination`: `Continue` runs later rules in the same stage. `Stop` skips them. \
         A `MockResponse` at `RequestHeaders` ends the exchange and does not connect upstream, \
         so response-stage rules and `onResponseHeaders` / `onResponse` do not run. \
         Put further changes in the mock body and headers.\n\n\
         A string match is `{ \"mode\", \"value\" }`. Modes:\n\n",
    );
    for matcher in sample_matchers() {
        out.push_str(&format!(
            "- `{}`\n",
            serde_json::to_string(&matcher).unwrap()
        ));
    }
    out.push_str("\n## Envelope\n\n```json\n");
    out.push_str(&serde_json::to_string_pretty(&envelope()).unwrap());
    out.push_str("\n```\n\n## Filters\n\n");
    out.push_str(
        "The stages on each filter are the ones where that filter is valid. \
         `And` / `Or` `config` is an array of filters. `Not` `config` is one filter. \
         `All` has no config.\n\n",
    );
    for filter in sample_filters() {
        push_entry(
            &mut out,
            &filter_stages(&filter),
            &serde_json::to_string(&filter).unwrap(),
        );
    }
    out.push_str("\n## Actions\n\n");
    out.push_str(
        "Unit actions such as `Drop` and `Inspect` have no `config`. \
         `MockResponse` `headers` is an object. A body is one of:\n\n",
    );
    for body in sample_bodies() {
        out.push_str(&format!("- `{}`\n", serde_json::to_string(&body).unwrap()));
    }
    out.push_str("\nA body transform is one of:\n\n");
    for transform in sample_transforms() {
        out.push_str(&format!(
            "- `{}`\n",
            serde_json::to_string(&transform).unwrap()
        ));
    }
    out.push_str("\n`UpdateRequestHeader` `value` may contain `{{previous}}`.\n\n");
    for action in sample_actions() {
        push_entry(
            &mut out,
            &action_stages(&action),
            &serde_json::to_string(&action).unwrap(),
        );
    }
    out
}

fn push_entry(out: &mut String, stages: &str, json: &str) {
    let ty = serde_json::from_str::<serde_json::Value>(json).unwrap()["type"]
        .as_str()
        .unwrap()
        .to_string();
    out.push_str(&format!(
        "### {ty}\n\nStages: {stages}\n\n```json\n{json}\n```\n\n"
    ));
}

fn envelope() -> Rule {
    Rule {
        id: "r1".to_string(),
        name: "r1".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 200,
        termination: RuleTermination::Continue,
        filter: Filter::Url(StringMatcher::Contains("example".to_string())),
        actions: vec![Action::AddRequestHeader {
            name: "X-Test".to_string(),
            value: "1".to_string(),
        }],
        constraints: None,
    }
}

fn stage_name(stage: &RuleStage) -> &'static str {
    match stage {
        RuleStage::Connect => "Connect",
        RuleStage::RequestHeaders => "RequestHeaders",
        RuleStage::RequestBody => "RequestBody",
        RuleStage::ResponseHeaders => "ResponseHeaders",
        RuleStage::ResponseBody => "ResponseBody",
        RuleStage::WebSocketMessage => "WebSocketMessage",
    }
}

const STAGES: [RuleStage; 6] = [
    RuleStage::Connect,
    RuleStage::RequestHeaders,
    RuleStage::RequestBody,
    RuleStage::ResponseHeaders,
    RuleStage::ResponseBody,
    RuleStage::WebSocketMessage,
];

fn action_stages(action: &Action) -> String {
    STAGES
        .iter()
        .filter(|stage| validate_action_stage(action, stage))
        .map(stage_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn filter_stages(filter: &Filter) -> String {
    let compiled = compile_filter(filter);
    STAGES
        .iter()
        .filter(|stage| validate_filter_stage(&compiled, stage))
        .map(stage_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn sample_matchers() -> Vec<StringMatcher> {
    vec![
        StringMatcher::Exact("example.com".to_string()),
        StringMatcher::Contains("example".to_string()),
        StringMatcher::Prefix("https://".to_string()),
        StringMatcher::Suffix(".js".to_string()),
        StringMatcher::Regex("api/v[0-9]+".to_string()),
        StringMatcher::Glob("*.example.com".to_string()),
    ]
}

#[allow(dead_code)]
fn cover_matcher(matcher: &StringMatcher) {
    match matcher {
        StringMatcher::Exact(_)
        | StringMatcher::Contains(_)
        | StringMatcher::Prefix(_)
        | StringMatcher::Suffix(_)
        | StringMatcher::Regex(_)
        | StringMatcher::Glob(_) => {}
    }
}

fn sample_bodies() -> Vec<BodySource> {
    vec![
        BodySource::Text("{\"ok\":true}".to_string()),
        BodySource::File("fixtures/health.json".to_string()),
        BodySource::Base64("aGk=".to_string()),
    ]
}

#[allow(dead_code)]
fn cover_body(body: &BodySource) {
    match body {
        BodySource::Text(_) | BodySource::File(_) | BodySource::Base64(_) => {}
    }
}

fn sample_transforms() -> Vec<BodyTransform> {
    vec![
        BodyTransform::RegexReplace {
            pattern: "foo".to_string(),
            replacement: "bar".to_string(),
        },
        BodyTransform::JsonPathSet {
            path: "$.n".to_string(),
            value: "1".to_string(),
        },
        BodyTransform::JsonPathDelete {
            path: "$.secret".to_string(),
        },
    ]
}

#[allow(dead_code)]
fn cover_transform(transform: &BodyTransform) {
    match transform {
        BodyTransform::RegexReplace { .. }
        | BodyTransform::JsonPathSet { .. }
        | BodyTransform::JsonPathDelete { .. } => {}
    }
}

macro_rules! catalog_actions {
    ($($pat:pat => $expr:expr,)*) => {
        fn sample_actions() -> Vec<Action> {
            vec![$($expr,)*]
        }
        #[allow(dead_code)]
        fn cover_action(action: &Action) {
            match action {
                $($pat => {},)*
            }
        }
    };
}

catalog_actions! {
    Action::Drop => Action::Drop,
    Action::Abort => Action::Abort,
    Action::Delay { .. } => Action::Delay { ms: 50 },
    Action::Throttle { .. } => Action::Throttle { kbps: 100 },
    Action::Tag { .. } => Action::Tag { key: "env".to_string(), value: "test".to_string() },
    Action::Inspect => Action::Inspect,
    Action::SetVariable { .. } => Action::SetVariable { name: "user".to_string(), value: "ada".to_string() },
    Action::RateLimit { .. } => Action::RateLimit { key: "ip".to_string(), limit: 10, window_ms: 1000 },
    Action::RedirectIp { .. } => Action::RedirectIp { target: "10.0.0.8".to_string() },
    Action::SetTtl { .. } => Action::SetTtl { ttl: 64 },
    Action::ForwardPort { .. } => Action::ForwardPort { target_host: "127.0.0.1".to_string(), target_port: 8081 },
    Action::MockResponse { .. } => Action::MockResponse {
        status: 200,
        headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        body: Some(BodySource::Text("{\"ok\":true}".to_string())),
    },
    Action::MapLocal { .. } => Action::MapLocal { path: "fixtures/health.json".to_string(), content_type: Some("application/json".to_string()) },
    Action::MapRemote { .. } => Action::MapRemote { url: "https://example.com/health".to_string(), preserve_host: false },
    Action::Redirect { .. } => Action::Redirect { location: "https://example.com/login".to_string(), status: 302 },
    Action::AddRequestHeader { .. } => Action::AddRequestHeader { name: "X-Test".to_string(), value: "1".to_string() },
    Action::UpdateRequestHeader { .. } => Action::UpdateRequestHeader { name: "User-Agent".to_string(), value: "relay {{previous}}".to_string(), add_if_missing: true },
    Action::DeleteRequestHeader { .. } => Action::DeleteRequestHeader { name: "Cookie".to_string() },
    Action::AddResponseHeader { .. } => Action::AddResponseHeader { name: "X-Test".to_string(), value: "1".to_string() },
    Action::UpdateResponseHeader { .. } => Action::UpdateResponseHeader { name: "Cache-Control".to_string(), value: "no-store".to_string(), add_if_missing: true },
    Action::DeleteResponseHeader { .. } => Action::DeleteResponseHeader { name: "Set-Cookie".to_string() },
    Action::SetRequestMethod { .. } => Action::SetRequestMethod { method: "POST".to_string() },
    Action::SetRequestUrl { .. } => Action::SetRequestUrl { url: "https://example.com/v2/health".to_string() },
    Action::SetRequestBody { .. } => Action::SetRequestBody { body: BodySource::Text("{\"n\":1}".to_string()) },
    Action::SetResponseStatus { .. } => Action::SetResponseStatus { status: 201 },
    Action::SetResponseBody { .. } => Action::SetResponseBody { body: BodySource::Base64("aGk=".to_string()) },
    Action::TransformRequestBody { .. } => Action::TransformRequestBody { transform: BodyTransform::RegexReplace { pattern: "foo".to_string(), replacement: "bar".to_string() } },
    Action::TransformResponseBody { .. } => Action::TransformResponseBody { transform: BodyTransform::JsonPathDelete { path: "$.secret".to_string() } },
    Action::MockWebSocketMessage { .. } => Action::MockWebSocketMessage { direction: WebSocketDirection::Incoming, message: "ping".to_string() },
    Action::DropWebSocketMessage => Action::DropWebSocketMessage,
}

macro_rules! catalog_filters {
    ($($pat:pat => $expr:expr,)*) => {
        fn sample_filters() -> Vec<Filter> {
            vec![$($expr,)*]
        }
        #[allow(dead_code)]
        fn cover_filter(filter: &Filter) {
            match filter {
                $($pat => {},)*
            }
        }
    };
}

catalog_filters! {
    Filter::All => Filter::All,
    Filter::SrcIp(_) => Filter::SrcIp("10.0.0.0/8".to_string()),
    Filter::DstPort(_) => Filter::DstPort(443),
    Filter::Protocol(_) => Filter::Protocol("TCP".to_string()),
    Filter::TransparentMode(_) => Filter::TransparentMode(true),
    Filter::Url(_) => Filter::Url(StringMatcher::Contains("example".to_string())),
    Filter::Host(_) => Filter::Host(StringMatcher::Exact("example.com".to_string())),
    Filter::Path(_) => Filter::Path(StringMatcher::Prefix("/v1/".to_string())),
    Filter::Method(_) => Filter::Method(StringMatcher::Exact("POST".to_string())),
    Filter::RequestHeader { .. } => Filter::RequestHeader {
        name: "Accept".to_string(),
        value: Some(StringMatcher::Contains("json".to_string())),
    },
    Filter::ResponseHeader { .. } => Filter::ResponseHeader {
        name: "Content-Type".to_string(),
        value: Some(StringMatcher::Contains("json".to_string())),
    },
    Filter::StatusCode(_) => Filter::StatusCode(404),
    Filter::ResponseBody(_) => Filter::ResponseBody(StringMatcher::Contains("error".to_string())),
    Filter::WebSocketMessage(_) => Filter::WebSocketMessage(StringMatcher::Contains("ping".to_string())),
    Filter::And(_) => Filter::And(vec![Filter::All, Filter::Method(StringMatcher::Exact("GET".to_string()))]),
    Filter::Or(_) => Filter::Or(vec![Filter::Host(StringMatcher::Exact("a.test".to_string())), Filter::Host(StringMatcher::Exact("b.test".to_string()))]),
    Filter::Not(_) => Filter::Not(Box::new(Filter::Method(StringMatcher::Exact("OPTIONS".to_string())))),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_is_built_from_every_action_and_filter() {
        let guide = rule_api_guide();
        for action in sample_actions() {
            cover_action(&action);
            let json = serde_json::to_string(&action).unwrap();
            assert!(guide.contains(&json), "missing action {json}");
            let back: Action = serde_json::from_str(&json).unwrap();
            cover_action(&back);
        }
        for filter in sample_filters() {
            cover_filter(&filter);
            let json = serde_json::to_string(&filter).unwrap();
            assert!(guide.contains(&json), "missing filter {json}");
        }
        for matcher in sample_matchers() {
            cover_matcher(&matcher);
            let json = serde_json::to_string(&matcher).unwrap();
            assert!(guide.contains(&json), "missing matcher {json}");
        }
        for body in sample_bodies() {
            cover_body(&body);
            let json = serde_json::to_string(&body).unwrap();
            assert!(guide.contains(&json), "missing body {json}");
        }
        for transform in sample_transforms() {
            cover_transform(&transform);
            let json = serde_json::to_string(&transform).unwrap();
            assert!(guide.contains(&json), "missing transform {json}");
        }
    }

    #[test]
    fn mock_response_is_not_offered_for_the_response_stages() {
        let guide = rule_api_guide();
        let section = guide
            .split("### MockResponse")
            .nth(1)
            .expect("MockResponse section");
        let stages = section
            .lines()
            .find(|line| line.starts_with("Stages:"))
            .expect("stage line");
        assert!(stages.contains("RequestHeaders"));
        assert!(!stages.contains("ResponseHeaders"));
        assert!(!stages.contains("ResponseBody"));
    }
}
