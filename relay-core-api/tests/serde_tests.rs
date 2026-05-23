use chrono::Utc;
use relay_core_api::flow::{
    BodyData, Direction, Flow, FlowUpdate, HttpLayer, HttpRequest, Layer, NetworkInfo,
    TransportProtocol, WebSocketMessage,
};
use relay_core_api::policy::{
    ProxyPolicy, ProxyPolicyPatch, QuicMode, RedactionPolicyPatch, TransparentLogLevel,
};
use relay_core_api::rule::{Action, Filter, Rule, RuleStage, RuleTermination, StringMatcher};
use serde_json::json;
use url::Url;
use uuid::Uuid;

fn sample_flow() -> Flow {
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
                url: Url::parse("http://example.com/api").expect("invalid url"),
                version: "HTTP/1.1".to_string(),
                headers: vec![("accept".to_string(), "application/json".to_string())],
                cookies: vec![],
                query: vec![("q".to_string(), "1".to_string())],
                body: None,
            },
            response: None,
            error: None,
        }),
        tags: vec!["test".to_string()],
        meta: std::collections::HashMap::from([("internal".to_string(), "yes".to_string())]),
        resilience_trace: None,
    }
}

#[test]
fn test_flow_roundtrip_and_meta_skip() {
    let flow = sample_flow();
    let serialized = serde_json::to_value(&flow).expect("serialize flow failed");

    assert!(
        serialized.get("meta").is_none(),
        "meta should be skipped in serialization"
    );

    let parsed: Flow = serde_json::from_value(serialized).expect("deserialize flow failed");
    match parsed.layer {
        Layer::Http(http) => {
            assert_eq!(http.request.method, "GET");
            assert_eq!(http.request.url.as_str(), "http://example.com/api");
        }
        _ => panic!("expected http layer"),
    }
}

#[test]
fn test_flow_update_tagged_serialization() {
    let update = FlowUpdate::HttpBody {
        flow_id: "flow-1".to_string(),
        direction: relay_core_api::flow::Direction::ClientToServer,
        body: BodyData {
            encoding: "utf-8".to_string(),
            content: "hello".to_string(),
            size: 5,
        },
    };

    let value = serde_json::to_value(update).expect("serialize FlowUpdate failed");
    assert_eq!(value["type"], "HttpBody");
    assert_eq!(value["data"]["flow_id"], "flow-1");
}

#[test]
fn test_flow_update_websocket_message_roundtrip() {
    let msg = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ServerToClient,
        content: BodyData {
            encoding: "utf-8".to_string(),
            content: "pong".to_string(),
            size: 4,
        },
        opcode: "Text".to_string(),
    };
    let update = FlowUpdate::WebSocketMessage {
        flow_id: "flow-ws-1".to_string(),
        message: msg.clone(),
    };

    let value = serde_json::to_value(&update).expect("serialize ws update failed");
    assert_eq!(value["type"], "WebSocketMessage");
    assert_eq!(value["data"]["flow_id"], "flow-ws-1");
    assert_eq!(value["data"]["message"]["content"]["content"], "pong");

    let parsed: FlowUpdate = serde_json::from_value(value).expect("deserialize ws update failed");
    match parsed {
        FlowUpdate::WebSocketMessage { flow_id, message } => {
            assert_eq!(flow_id, "flow-ws-1");
            assert_eq!(message.direction, Direction::ServerToClient);
            assert_eq!(message.content.content, "pong");
            assert_eq!(message.opcode, "Text");
            assert_eq!(message.content.size, msg.content.size);
        }
        other => panic!("expected websocket message update, got {:?}", other),
    }
}

#[test]
fn test_proxy_policy_defaults_via_deserialize() {
    let policy: ProxyPolicy = serde_json::from_value(json!({})).expect("deserialize policy failed");

    assert!(policy.strict_http_semantics);
    assert!(!policy.enable_retry);
    assert_eq!(policy.max_retries, 3);
    assert_eq!(policy.request_timeout_ms, 30_000);
    assert_eq!(policy.max_body_size, 10 * 1024 * 1024);
    assert_eq!(policy.transparent_log_level, TransparentLogLevel::Info);
    assert_eq!(policy.quic_mode, QuicMode::Downgrade);
    assert!(!policy.redaction.enabled);
    assert!(!policy.redaction.redact_bodies);
    assert!(
        policy
            .redaction
            .sensitive_header_names
            .contains(&"authorization".to_string())
    );
    assert!(
        policy
            .redaction
            .sensitive_query_keys
            .contains(&"token".to_string())
    );
}

#[test]
fn test_proxy_policy_default_matches_empty_deserialize() {
    let from_default = ProxyPolicy::default();
    let from_deser: ProxyPolicy =
        serde_json::from_value(json!({})).expect("deserialize policy failed");

    assert_eq!(
        from_default.strict_http_semantics,
        from_deser.strict_http_semantics
    );
    assert_eq!(
        from_default.allow_fallback_method,
        from_deser.allow_fallback_method
    );
    assert_eq!(
        from_default.allow_fallback_status,
        from_deser.allow_fallback_status
    );
    assert_eq!(from_default.enable_retry, from_deser.enable_retry);
    assert_eq!(
        from_default.retry_idempotent_only,
        from_deser.retry_idempotent_only
    );
    assert_eq!(from_default.max_retries, from_deser.max_retries);
    assert_eq!(from_default.sandbox_root, from_deser.sandbox_root);
    assert_eq!(
        from_default.max_local_file_bytes,
        from_deser.max_local_file_bytes
    );
    assert_eq!(from_default.max_body_size, from_deser.max_body_size);
    assert_eq!(
        from_default.request_timeout_ms,
        from_deser.request_timeout_ms
    );
    assert_eq!(
        from_default.transparent_enabled,
        from_deser.transparent_enabled
    );
    assert_eq!(
        from_default.transparent_require_original_dst,
        from_deser.transparent_require_original_dst
    );
    assert_eq!(
        from_default.transparent_allow_host_fallback,
        from_deser.transparent_allow_host_fallback
    );
    assert_eq!(
        from_default.transparent_reject_loopback_target,
        from_deser.transparent_reject_loopback_target
    );
    assert_eq!(
        from_default.transparent_log_level,
        from_deser.transparent_log_level
    );
    assert_eq!(from_default.quic_mode, from_deser.quic_mode);
    assert_eq!(
        from_default.quic_downgrade_clear_cache,
        from_deser.quic_downgrade_clear_cache
    );
    assert_eq!(from_default.redaction, from_deser.redaction);
}

#[test]
fn test_proxy_policy_patch_deserialize_and_apply_redaction() {
    let patch: ProxyPolicyPatch = serde_json::from_value(json!({
        "redaction": {
            "enabled": true,
            "redact_bodies": true,
            "sensitive_query_keys": ["token", "secret"]
        }
    }))
    .expect("deserialize policy patch failed");

    let mut policy = ProxyPolicy::default();
    policy.apply_patch(patch);

    assert!(policy.redaction.enabled);
    assert!(policy.redaction.redact_bodies);
    assert_eq!(
        policy.redaction.sensitive_query_keys,
        vec!["token".to_string(), "secret".to_string()]
    );
}

#[test]
fn test_redaction_patch_partial_update_keeps_unspecified_fields() {
    let mut policy = ProxyPolicy::default();
    let original_headers = policy.redaction.sensitive_header_names.clone();
    policy.redaction.enabled = true;
    policy.redaction.redact_bodies = false;

    policy.apply_patch(ProxyPolicyPatch {
        redaction: Some(RedactionPolicyPatch {
            redact_bodies: Some(true),
            ..Default::default()
        }),
    });

    assert!(policy.redaction.enabled);
    assert!(policy.redaction.redact_bodies);
    assert_eq!(policy.redaction.sensitive_header_names, original_headers);
}

#[test]
fn test_rule_priority_defaults_to_zero_when_missing() {
    let value = json!({
        "id": "rule-default-priority",
        "name": "Rule Default Priority",
        "active": true,
        "stage": "RequestHeaders",
        "termination": "Continue",
        "filter": {"type": "All"},
        "actions": []
    });
    let rule: Rule = serde_json::from_value(value).expect("deserialize rule failed");
    assert_eq!(rule.priority, 0, "priority should default to zero");
}

#[test]
fn test_rule_roundtrip_with_map_remote_action() {
    let rule = Rule {
        id: "rule-map-remote".to_string(),
        name: "Rule Map Remote".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 88,
        termination: RuleTermination::Stop,
        filter: Filter::Url(StringMatcher::Contains("example.com".to_string())),
        actions: vec![Action::MapRemote {
            url: "https://mirror.example.com:9443".to_string(),
            preserve_host: false,
        }],
        constraints: None,
    };

    let value = serde_json::to_value(&rule).expect("serialize rule failed");
    let parsed: Rule = serde_json::from_value(value).expect("deserialize rule failed");
    assert_eq!(parsed.id, "rule-map-remote");
    assert_eq!(parsed.priority, 88);
    match parsed.actions.first() {
        Some(Action::MapRemote { url, preserve_host }) => {
            assert_eq!(url, "https://mirror.example.com:9443");
            assert!(!preserve_host);
        }
        other => panic!("expected map remote action, got {:?}", other),
    }
}

#[test]
fn test_flow_custom_layer_deserializes_to_generic_protocol_layer() {
    let mut value = serde_json::to_value(sample_flow()).expect("serialize flow failed");
    value["layer"] = json!({
        "type": "Custom",
        "data": {
            "protocol": "mqtt",
            "data": {
                "topic": "hello/world",
                "qos": 1
            }
        }
    });

    let parsed: Flow = serde_json::from_value(value).expect("deserialize flow failed");
    match parsed.layer {
        Layer::Custom(layer) => {
            assert_eq!(layer.protocol_name(), "mqtt");
            assert_eq!(layer.to_json()["topic"], "hello/world");
            assert_eq!(layer.to_json()["qos"], 1);
        }
        _ => panic!("expected custom layer"),
    }
}
