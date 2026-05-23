use chrono::Utc;
use relay_core_api::flow::{Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol};
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::rule::engine::RuleEngine;
use relay_core_lib::rule::model::{
    Action, BodySource, Filter, Rule, RuleOutcome, RuleStage, RuleTermination, RuleTraceSummary,
};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::Arc;
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
        resilience_trace: None,
    }
}

fn create_rule(action: Action, policy: Arc<ProxyPolicy>) -> RuleEngine {
    let rule = Rule {
        id: "test-rule".to_string(),
        name: "Test Rule".to_string(),
        active: true,
        stage: RuleStage::RequestHeaders,
        priority: 1,
        termination: RuleTermination::Continue,
        filter: Filter::All,
        actions: vec![action],
        constraints: None,
    };
    RuleEngine::new(vec![rule], vec![], Some(policy), None)
}

#[tokio::test]
async fn test_sandbox_allows_valid_file_in_action() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("allowed.txt");
    fs::write(&file_path, "allowed content").unwrap();

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(temp_dir.clone()),
        ..Default::default()
    });

    let action = Action::SetRequestBody {
        body: BodySource::File(file_path.to_str().unwrap().to_string()),
    };

    let engine = create_rule(action, policy);
    let mut flow = create_test_flow("http://example.com", "POST");

    engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    if let Layer::Http(http) = &flow.layer {
        assert!(http.request.body.is_some(), "Body should be set");
        let body = http.request.body.as_ref().unwrap();
        if body.encoding == "utf-8" {
            assert_eq!(body.content, "allowed content");
        } else {
            // base64 check if needed
        }
    } else {
        panic!("Invalid layer");
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_map_local_respects_sandbox_and_size_limit() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();

    // 1. Create a large file
    let large_file = temp_dir.join("large.txt");
    let limit = 100;
    let content = vec![b'a'; limit + 10];
    fs::write(&large_file, &content).unwrap();

    // 2. Create a normal file
    let normal_file = temp_dir.join("normal.txt");
    fs::write(&normal_file, "normal").unwrap();

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(temp_dir.clone()),
        max_local_file_bytes: limit,
        ..Default::default()
    });

    // Case A: Large file should fail (ActionOutcome::Failed or just no body?)
    // MapLocal returns Failed if body loading fails.
    let action_large = Action::MapLocal {
        path: large_file.to_str().unwrap().to_string(),
        content_type: None,
    };
    let engine_large = create_rule(action_large, policy.clone());
    let mut flow_large = create_test_flow("http://example.com", "GET");

    // We need to check the outcome, but engine.execute consumes the result internally?
    // Wait, engine.execute returns `RuleTraceSummary`.
    // Let's check if the flow response is set.
    engine_large
        .execute(RuleStage::RequestHeaders, &mut flow_large)
        .await;

    if let Layer::Http(http) = &flow_large.layer {
        assert!(
            http.response.is_none(),
            "Response should NOT be set for large file MapLocal"
        );
    }

    // Case B: Normal file should succeed
    let action_normal = Action::MapLocal {
        path: normal_file.to_str().unwrap().to_string(),
        content_type: Some("text/plain".to_string()),
    };
    let engine_normal = create_rule(action_normal, policy.clone());
    let mut flow_normal = create_test_flow("http://example.com", "GET");

    engine_normal
        .execute(RuleStage::RequestHeaders, &mut flow_normal)
        .await;

    if let Layer::Http(http) = &flow_normal.layer {
        assert!(
            http.response.is_some(),
            "Response SHOULD be set for normal file MapLocal"
        );
        let res = http.response.as_ref().unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body.as_ref().unwrap().content, "normal");
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_sandbox_rejects_path_traversal() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();

    // Create a file outside the sandbox (in parent of temp_dir, which is likely allowed by OS but logically outside sandbox)
    // Actually, to be safe and cross-platform, we create a nested structure:
    // /tmp/uuid/root/  <- sandbox root
    // /tmp/uuid/outside.txt
    let root_dir = temp_dir.join("root");
    fs::create_dir_all(&root_dir).unwrap();
    let outside_file = temp_dir.join("outside.txt");
    fs::write(&outside_file, "secret").unwrap();

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(root_dir.clone()),
        ..Default::default()
    });

    let action = Action::SetRequestBody {
        body: BodySource::File("../outside.txt".to_string()),
    };

    let engine = create_rule(action, policy);
    let mut flow = create_test_flow("http://example.com", "POST");

    engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    if let Layer::Http(http) = &flow.layer {
        assert!(
            http.request.body.is_none(),
            "Body should NOT be set for traversal"
        );
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_sandbox_rejects_large_file() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("large.txt");

    // Create a file slightly larger than limit (e.g., limit 100 bytes)
    let limit = 100;
    let content = vec![b'a'; limit + 10];
    {
        let mut f = fs::File::create(&file_path).unwrap();
        f.write_all(&content).unwrap();
    }

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(temp_dir.clone()),
        max_local_file_bytes: limit,
        ..Default::default()
    });

    let action = Action::SetRequestBody {
        body: BodySource::File(file_path.to_str().unwrap().to_string()),
    };

    let engine = create_rule(action, policy);
    let mut flow = create_test_flow("http://example.com", "POST");

    engine.execute(RuleStage::RequestHeaders, &mut flow).await;

    if let Layer::Http(http) = &flow.layer {
        assert!(
            http.request.body.is_none(),
            "Body should NOT be set for large file"
        );
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_map_local_large_file_records_failed_trace() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();
    let large_file = temp_dir.join("too-large.txt");
    let limit = 64usize;
    fs::write(&large_file, vec![b'x'; limit + 1]).unwrap();

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(temp_dir.clone()),
        max_local_file_bytes: limit,
        ..Default::default()
    });
    let action = Action::MapLocal {
        path: large_file.to_str().unwrap().to_string(),
        content_type: None,
    };
    let engine = create_rule(action, policy);
    let mut flow = create_test_flow("http://example.com", "GET");

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert_eq!(ctx.trace.len(), 1);
    match &ctx.trace[0].outcome {
        RuleOutcome::Failed(msg) => assert!(msg.contains("Failed to load local file")),
        other => panic!("expected failed outcome, got {:?}", other),
    }
    assert!(matches!(ctx.summary, RuleTraceSummary::NoMatch));
    if let Layer::Http(http) = &flow.layer {
        assert!(http.response.is_none(), "response must not be mocked");
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_set_request_body_traversal_is_noop_and_continues() {
    let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).unwrap();
    let root_dir = temp_dir.join("root");
    fs::create_dir_all(&root_dir).unwrap();
    let outside_file = temp_dir.join("outside.txt");
    fs::write(&outside_file, "secret").unwrap();

    let policy = Arc::new(ProxyPolicy {
        sandbox_root: Some(root_dir.clone()),
        ..Default::default()
    });
    let action = Action::SetRequestBody {
        body: BodySource::File("../outside.txt".to_string()),
    };
    let engine = create_rule(action, policy);
    let mut flow = create_test_flow("http://example.com", "POST");

    let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
    assert_eq!(ctx.trace.len(), 1);
    assert!(
        matches!(ctx.trace[0].outcome, RuleOutcome::MatchedAndExecuted),
        "SetRequestBody uses Continue on unreadable file and should not fail the rule"
    );
    if let Layer::Http(http) = &flow.layer {
        assert!(
            http.request.body.is_none(),
            "request body should remain empty"
        );
    }

    let _ = fs::remove_dir_all(&temp_dir);
}
