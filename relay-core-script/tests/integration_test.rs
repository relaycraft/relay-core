use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use relay_core_api::flow::{
    BodyData, Direction, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo,
    ResponseTiming, TransportProtocol, WebSocketMessage,
};
use relay_core_lib::interceptor::{
    BoxError, Interceptor, RequestAction, ResponseAction, WebSocketMessageAction,
};
use relay_core_script::{ScriptFetchConfig, ScriptInterceptor};
use std::collections::{HashMap, HashSet};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn test_deno_script_on_request() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    // JS script that modifies a header - Deno style
    let script = r#"
        globalThis.onRequest = function(context, flow) {
            // flow.layer is { type: "Http", data: { request: ... } }
            if (flow.layer.type === "Http") {
                flow.layer.data.request.headers.push(["X-Deno-Scripted", "true"]);
            }
            
            return flow;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    let body = Full::new(Bytes::new())
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_request(&mut flow, body).await.unwrap() {
        RequestAction::Continue(_) => {
            if let Layer::Http(http) = &flow.layer {
                let headers = &http.request.headers;
                assert!(
                    headers
                        .iter()
                        .any(|(k, v)| k == "X-Deno-Scripted" && v == "true")
                );
            } else {
                panic!("Flow layer is not Http");
            }
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_deno_script_on_websocket_message() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onWebSocketMessage = function(context, flow, message) {
            if (message.content.encoding === "utf-8") {
                message.content.content += " [Modified]";
            }
            return message;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();

    let message = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "utf-8".to_string(),
            content: "Hello".to_string(),
            size: 5,
        },
        opcode: "Text".to_string(),
    };

    match interceptor
        .on_websocket_message(&mut flow, message.clone())
        .await
        .unwrap()
    {
        WebSocketMessageAction::Continue(mod_msg) => {
            assert_eq!(mod_msg.content.content, "Hello [Modified]");
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}

fn create_dummy_flow() -> Flow {
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
                url: Url::parse("http://example.com").unwrap(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("User-Agent".to_string(), "Test".to_string())],
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
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
    }
}

#[tokio::test]
async fn test_deno_script_on_websocket_binary_message() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onWebSocketMessage = function(context, flow, message) {
            if (message.content.encoding === "base64") {
                // Replace "Hello" (SGVsbG8=) with "Hello [BinMod]" (SGVsbG8gW0Jpbk1vZF0=)
                if (message.content.content === "SGVsbG8=") {
                    message.content.content = "SGVsbG8gW0Jpbk1vZF0=";
                }
            }
            return message;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();

    let message = WebSocketMessage {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        direction: Direction::ClientToServer,
        content: BodyData {
            encoding: "base64".to_string(),
            content: "SGVsbG8=".to_string(),
            size: 5,
        },
        opcode: "Binary".to_string(),
    };

    match interceptor
        .on_websocket_message(&mut flow, message.clone())
        .await
        .unwrap()
    {
        WebSocketMessageAction::Continue(mod_msg) => {
            assert_eq!(mod_msg.content.content, "SGVsbG8gW0Jpbk1vZF0=");
            assert_eq!(mod_msg.content.encoding, "base64");
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_deno_script_on_response() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onResponse = function(body, flow) {
            if (flow.layer.type === "Http" && flow.layer.data.response) {
                flow.layer.data.response.headers.push(["X-Resp-Scripted", "true"]);
            }
            return flow;
        }
    "#;
    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    if let Layer::Http(http) = &mut flow.layer {
        http.response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
            body: None,
            timing: ResponseTiming {
                time_to_first_byte: None,
                time_to_last_byte: None,
                connect_time_ms: None,
                ssl_time_ms: None,
            },
            cookies: vec![],
        });
    }

    let body = Full::new(Bytes::from("payload"))
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_response(&mut flow, body).await.unwrap() {
        ResponseAction::Continue(new_body) => {
            let bytes = new_body.collect().await.unwrap().to_bytes();
            assert_eq!(bytes.as_ref(), b"payload");
            if let Layer::Http(http) = &flow.layer {
                let response = http.response.as_ref().expect("response should exist");
                assert!(
                    response
                        .headers
                        .iter()
                        .any(|(k, v)| k == "X-Resp-Scripted" && v == "true")
                );
            } else {
                panic!("Flow layer is not Http");
            }
        }
        other => panic!("Expected Continue, got {:?}", other),
    }
}

#[tokio::test]
async fn test_deno_script_invalid_source_rejected() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let bad_script = "globalThis.onRequest = () => { invalid javascript !!!";
    let result = interceptor.load_script(bad_script).await;
    assert!(result.is_err(), "invalid script source should fail to load");
}

// ── S5: relay.env whitelist tests ────────────────────────────────

#[tokio::test]
async fn test_env_whitelist_allows_listed_key() {
    let mut allow = HashSet::new();
    allow.insert("PATH".to_string());
    let interceptor = ScriptInterceptor::new_with_env(allow).await.unwrap();

    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const p = relay.env("PATH");
            if (p === undefined) flow.tags.push("env-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"env-fail".to_string()),
        "PATH should be accessible"
    );
}

#[tokio::test]
async fn test_env_whitelist_blocks_unlisted_key() {
    let mut allow = HashSet::new();
    allow.insert("PATH".to_string());
    let interceptor = ScriptInterceptor::new_with_env(allow).await.unwrap();

    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const home = relay.env("HOME");
            if (home !== undefined) flow.tags.push("env-leak");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"env-leak".to_string()),
        "HOME should not be accessible"
    );
}

#[tokio::test]
async fn test_env_empty_whitelist_blocks_all() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const p = relay.env("PATH");
            if (p !== undefined) flow.tags.push("env-leak");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"env-leak".to_string()),
        "nothing should be accessible with empty allowlist"
    );
}

#[tokio::test]
async fn test_env_case_sensitive_no_bypass() {
    let mut allow = HashSet::new();
    allow.insert("PATH".to_string());
    let interceptor = ScriptInterceptor::new_with_env(allow).await.unwrap();

    // Try lowercase bypass
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const p = relay.env("path");
            if (p !== undefined) flow.tags.push("env-case-bypass");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"env-case-bypass".to_string()),
        "case-variant should not bypass"
    );
}

#[tokio::test]
async fn test_env_access_counter_increments() {
    let mut allow = HashSet::new();
    allow.insert("PATH".to_string());
    let interceptor = ScriptInterceptor::new_with_env(allow).await.unwrap();

    let before = interceptor.metrics.prometheus_lines();
    let before_val: usize = before
        .lines()
        .find(|l| l.contains("script_env_access_total"))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            relay.env("PATH");
            relay.env("HOME");  // blocked, but still counts
            relay.env("PATH");  // second access
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;

    let after = interceptor.metrics.prometheus_lines();
    let after_val: usize = after
        .lines()
        .find(|l| l.contains("script_env_access_total"))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    assert!(
        after_val >= before_val + 3,
        "env access counter should increment by 3, got {} -> {}",
        before_val,
        after_val
    );
}

// ── S6: relay utility library tests ─────────────────────────────

#[tokio::test]
async fn test_relay_uuid_v4() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const id = relay.uuid();
            if (typeof id !== "string" || id.length !== 36) flow.tags.push("uuid-fail");
            if (!id.includes("-")) flow.tags.push("uuid-format-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"uuid-fail".to_string()),
        "relay.uuid should return valid UUID string"
    );
}

#[tokio::test]
async fn test_relay_hash_sha256() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const h = relay.hash("sha256", "hello");
            // SHA-256 of "hello" is 2cf24dba5fb0a30e26e83b2ac5b9e29e...
            if (h.length !== 64) flow.tags.push("hash-len-fail");
            if (!/^[0-9a-f]+$/.test(h)) flow.tags.push("hash-hex-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"hash-len-fail".to_string()),
        "sha256 should return 64 hex chars"
    );
    assert!(
        !flow.tags.contains(&"hash-hex-fail".to_string()),
        "sha256 should return lowercase hex"
    );
}

#[tokio::test]
async fn test_relay_hash_all_algorithms() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const algos = ["sha1", "sha256", "sha512", "md5"];
            for (const alg of algos) {
                const h = relay.hash(alg, "test");
                if (typeof h !== "string" || h.length === 0) {
                    flow.tags.push("hash-" + alg + "-fail");
                }
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    for algo in &["sha1", "sha256", "sha512", "md5"] {
        let tag = format!("hash-{}-fail", algo);
        assert!(!flow.tags.contains(&tag), "{} should produce output", algo);
    }
}

#[tokio::test]
async fn test_relay_hash_unsupported_algorithm() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            try {
                relay.hash("blake2", "test");
                flow.tags.push("hash-no-throw");
            } catch (e) {
                // Expected: unsupported algorithm error
                flow.tags.push("hash-threw");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    // The op error should propagate as a JS exception that the script catches
    assert!(
        flow.tags.contains(&"hash-threw".to_string())
            || flow.tags.contains(&"script-error".to_string()),
        "unsupported hash should cause error (got tags: {:?})",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_base64_encode_decode() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const encoded = relay.base64.encode("hello");
            if (encoded !== "aGVsbG8=") flow.tags.push("b64-encode-fail");
            const decoded = relay.base64.decode("aGVsbG8=");
            if (decoded !== "hello") flow.tags.push("b64-decode-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"b64-encode-fail".to_string()),
        "base64 encode should match"
    );
    assert!(
        !flow.tags.contains(&"b64-decode-fail".to_string()),
        "base64 decode should match"
    );
}

#[tokio::test]
async fn test_relay_base64_invalid_input() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            try {
                relay.base64.decode("!!!invalid!!!");
                flow.tags.push("b64-no-throw");
            } catch (e) {
                flow.tags.push("b64-threw");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    // Invalid base64 should cause an error (either caught in-script or as script-error)
    assert!(
        flow.tags.contains(&"b64-threw".to_string())
            || flow.tags.contains(&"script-error".to_string()),
        "invalid base64 should cause error (got tags: {:?})",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_json_parse_safe_valid_and_invalid() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const obj = relay.json.parseSafe('{"a":1}');
            if (obj === undefined || obj.a !== 1) flow.tags.push("json-valid-fail");
            const bad = relay.json.parseSafe('not json');
            if (bad !== null) flow.tags.push("json-invalid-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"json-valid-fail".to_string()),
        "valid JSON should parse"
    );
    assert!(
        !flow.tags.contains(&"json-invalid-fail".to_string()),
        "invalid JSON should return null"
    );
}

#[tokio::test]
async fn test_relay_json_stringify_pretty() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const s = relay.json.stringifyPretty({a:1});
            if (!s.includes("\n")) flow.tags.push("json-pretty-fail");
            if (!s.includes("  "  )) flow.tags.push("json-indent-fail");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    interceptor.on_request_headers(&mut flow).await;
    assert!(
        !flow.tags.contains(&"json-pretty-fail".to_string()),
        "pretty JSON should have newlines"
    );
    assert!(
        !flow.tags.contains(&"json-indent-fail".to_string()),
        "pretty JSON should have 2-space indent"
    );
}

// ── S7a: relay.fetch tests (M5: validation layer) ──────────────
// Actual HTTP execution deferred to M6 (S7b).

#[tokio::test]
async fn test_relay_fetch_default_disabled() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const r = relay.fetch("http://example.com");
            if (!r.ok && r.error === "script fetch disabled") {
                flow.tags.push("fetch-disabled-ok");
            } else {
                flow.tags.push("fetch-disabled-fail");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    assert!(
        flow.tags.contains(&"fetch-disabled-ok".to_string()),
        "relay.fetch should return disabled error by default, got tags: {:?}",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_fetch_rejects_non_allowlisted_host() {
    let fetch_config = ScriptFetchConfig {
        enabled: true,
        allow_hosts: HashSet::from(["trusted.example.com".to_string()]),
        ..Default::default()
    };
    let interceptor = ScriptInterceptor::new_with_env_and_fetch(HashSet::new(), fetch_config)
        .await
        .unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const r = relay.fetch("http://untrusted.example.com/api");
            if (!r.ok && r.error === "host not in allowlist") {
                flow.tags.push("fetch-allowlist-reject");
            } else {
                flow.tags.push("fetch-allowlist-fail");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    assert!(
        flow.tags.contains(&"fetch-allowlist-reject".to_string()),
        "relay.fetch should reject non-allowlisted host, got tags: {:?}",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_fetch_allows_allowlisted_host() {
    let fetch_config = ScriptFetchConfig {
        enabled: true,
        allow_hosts: HashSet::from(["trusted.example.com".to_string()]),
        ..Default::default()
    };
    let interceptor = ScriptInterceptor::new_with_env_and_fetch(HashSet::new(), fetch_config)
        .await
        .unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const r = relay.fetch("http://trusted.example.com/api");
            // Still returns error because actual fetch not implemented (M6),
            // but should NOT be "host not in allowlist" or "script fetch disabled"
            if (!r.ok && r.error !== "host not in allowlist" && r.error !== "script fetch disabled") {
                flow.tags.push("fetch-allowlist-pass");
            } else {
                flow.tags.push("fetch-allowlist-blocked");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    assert!(
        flow.tags.contains(&"fetch-allowlist-pass".to_string()),
        "relay.fetch should pass allowlist check for allowed host, got tags: {:?}",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_fetch_rejects_recursive_self() {
    let fetch_config = ScriptFetchConfig {
        enabled: true,
        proxy_listen_port: 8080,
        ..Default::default()
    };
    let interceptor = ScriptInterceptor::new_with_env_and_fetch(HashSet::new(), fetch_config)
        .await
        .unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            const r = relay.fetch("http://localhost:8080/api/test");
            if (!r.ok && r.error === "recursive fetch to self rejected") {
                flow.tags.push("fetch-recursion-reject");
            } else {
                flow.tags.push("fetch-recursion-fail");
            }
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;
    assert!(
        flow.tags.contains(&"fetch-recursion-reject".to_string()),
        "relay.fetch should reject recursive fetch to self, got tags: {:?}",
        flow.tags
    );
}

#[tokio::test]
async fn test_relay_fetch_rejected_counter_increments() {
    let interceptor = ScriptInterceptor::new().await.unwrap();
    let script = r#"
        globalThis.onRequestHeaders = (ctx, flow) => {
            relay.fetch("http://example.com");
            relay.fetch("http://other.com");
            return flow;
        };
    "#;
    interceptor.load_script(script).await.unwrap();
    let mut flow = create_dummy_flow();
    let _ = interceptor.on_request_headers(&mut flow).await;

    let metrics = interceptor.metrics.prometheus_lines();
    let fetch_total: usize = metrics
        .lines()
        .find(|l| l.contains("script_fetch_total"))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let fetch_rejected: usize = metrics
        .lines()
        .find(|l| l.contains("script_fetch_rejected_total"))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    assert!(
        fetch_total >= 2 && fetch_rejected >= 2,
        "fetch counters should increment (total={}, rejected={})",
        fetch_total,
        fetch_rejected
    );
}

#[tokio::test]
async fn test_script_reads_matched_rules_in_on_request() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onRequest = function(context, flow) {
            if (flow.matched_rules && flow.matched_rules.length > 0) {
                flow.layer.data.request.headers.push(["X-Matched-Rules", flow.matched_rules.join(",")]);
            }
            return flow;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    flow.matched_rules = vec!["rule-001".to_string(), "rule-002".to_string()];

    let body = Full::new(Bytes::new())
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_request(&mut flow, body).await.unwrap() {
        RequestAction::Continue(_) => {
            if let Layer::Http(http) = &flow.layer {
                let header = http
                    .request
                    .headers
                    .iter()
                    .find(|(k, _)| k == "X-Matched-Rules")
                    .map(|(_, v)| v.clone());
                assert_eq!(
                    header.as_deref(),
                    Some("rule-001,rule-002"),
                    "script should read matched_rules and inject header"
                );
            } else {
                panic!("Flow layer is not Http");
            }
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_script_reads_rule_variables_in_on_request() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onRequest = function(context, flow) {
            if (flow.rule_variables && flow.rule_variables["user_id"]) {
                flow.layer.data.request.headers.push(["X-User-Id", flow.rule_variables["user_id"]]);
            }
            return flow;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    let mut vars = HashMap::new();
    vars.insert("user_id".to_string(), "abc-123".to_string());
    flow.rule_variables = vars;

    let body = Full::new(Bytes::new())
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_request(&mut flow, body).await.unwrap() {
        RequestAction::Continue(_) => {
            if let Layer::Http(http) = &flow.layer {
                let header = http
                    .request
                    .headers
                    .iter()
                    .find(|(k, _)| k == "X-User-Id")
                    .map(|(_, v)| v.clone());
                assert_eq!(
                    header.as_deref(),
                    Some("abc-123"),
                    "script should read rule_variables and inject header"
                );
            } else {
                panic!("Flow layer is not Http");
            }
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}

#[tokio::test]
async fn test_script_empty_matched_rules_no_effect() {
    let interceptor = ScriptInterceptor::new().await.unwrap();

    let script = r#"
        globalThis.onRequest = function(context, flow) {
            let count = flow.matched_rules ? flow.matched_rules.length : 0;
            flow.layer.data.request.headers.push(["X-Matched-Count", String(count)]);
            return flow;
        }
    "#;

    interceptor.load_script(script).await.unwrap();

    let mut flow = create_dummy_flow();
    // matched_rules starts empty

    let body = Full::new(Bytes::new())
        .map_err(|e| Box::new(e) as BoxError)
        .boxed();

    match interceptor.on_request(&mut flow, body).await.unwrap() {
        RequestAction::Continue(_) => {
            if let Layer::Http(http) = &flow.layer {
                let header = http
                    .request
                    .headers
                    .iter()
                    .find(|(k, _)| k == "X-Matched-Count")
                    .map(|(_, v)| v.clone());
                assert_eq!(
                    header.as_deref(),
                    Some("0"),
                    "script should see empty matched_rules as zero-length"
                );
            }
        }
        res => panic!("Expected Continue, got {:?}", res),
    }
}
