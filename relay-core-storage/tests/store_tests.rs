use relay_core_storage::store::{AuditEventRecord, Store};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn sqlite_url() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock drift")
        .as_nanos();
    let pid = std::process::id();
    let seq = TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_dir = std::env::current_dir()
        .expect("cwd")
        .join("target")
        .join("test-dbs");
    std::fs::create_dir_all(&db_dir).expect("create test db dir");
    let db_path = db_dir.join(format!(
        "relay-core-storage-test-{}-{}-{}.db",
        pid, nanos, seq
    ));
    format!("sqlite://{}?mode=rwc", db_path.display())
}

#[tokio::test]
async fn test_rule_crud_and_replace() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .save_rule("rule-1", &json!({"name":"first","priority":1}))
        .await
        .expect("save rule-1 failed");
    store
        .save_rule("rule-2", &json!({"name":"second","priority":2}))
        .await
        .expect("save rule-2 failed");

    let mut rules = store.load_rules().await.expect("load rules failed");
    rules.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].0, "rule-1");
    assert_eq!(rules[1].0, "rule-2");

    store.delete_rule("rule-1").await.expect("delete rule-1 failed");
    let rules = store.load_rules().await.expect("load after delete failed");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].0, "rule-2");

    let replacement = vec![
        ("rule-3".to_string(), json!({"name":"third"})),
        ("rule-4".to_string(), json!({"name":"fourth"})),
    ];
    store
        .replace_rules(&replacement)
        .await
        .expect("replace rules failed");

    let mut rules = store.load_rules().await.expect("load after replace failed");
    rules.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].0, "rule-3");
    assert_eq!(rules[1].0, "rule-4");
}

#[tokio::test]
async fn test_delete_rule_nonexistent_is_idempotent() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .delete_rule("rule-missing")
        .await
        .expect("delete on missing row should not fail");
    let rules = store.load_rules().await.expect("load rules failed");
    assert!(rules.is_empty(), "table should remain empty");
}

#[tokio::test]
async fn test_save_rule_upsert_updates_existing_row() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .save_rule("rule-upsert", &json!({"name":"v1","priority":1}))
        .await
        .expect("first save failed");
    store
        .save_rule("rule-upsert", &json!({"name":"v2","priority":99}))
        .await
        .expect("upsert save failed");

    let rules = store.load_rules().await.expect("load rules failed");
    assert_eq!(rules.len(), 1, "upsert should keep single row for same id");
    assert_eq!(rules[0].0, "rule-upsert");
    assert_eq!(rules[0].1["name"], "v2");
    assert_eq!(rules[0].1["priority"], 99);
}

#[tokio::test]
async fn test_replace_rules_with_empty_clears_table() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .save_rule("rule-a", &json!({"name":"a"}))
        .await
        .expect("save rule-a failed");
    store
        .save_rule("rule-b", &json!({"name":"b"}))
        .await
        .expect("save rule-b failed");

    store
        .replace_rules(&[])
        .await
        .expect("replace with empty failed");
    let rules = store.load_rules().await.expect("load rules failed");
    assert!(rules.is_empty(), "replace with empty should clear all rules");
}

#[tokio::test]
async fn test_replace_rules_failure_rolls_back_previous_data() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");
    store
        .save_rule("rule-existing", &json!({"name":"existing"}))
        .await
        .expect("seed save failed");

    let replacement = vec![
        ("rule-dup".to_string(), json!({"name":"a"})),
        ("rule-dup".to_string(), json!({"name":"b"})),
    ];
    let result = store.replace_rules(&replacement).await;
    assert!(result.is_err(), "duplicate ids in replacement should fail");

    let rules = store.load_rules().await.expect("load rules failed");
    assert_eq!(rules.len(), 1, "failed replace should rollback transaction");
    assert_eq!(rules[0].0, "rule-existing");
    assert_eq!(rules[0].1["name"], "existing");
}

#[tokio::test]
async fn test_concurrent_rule_writes() {
    let store = Arc::new(Store::connect(&sqlite_url()).await.expect("connect failed"));
    store.init().await.expect("init failed");

    let mut handles = Vec::new();
    for i in 0..20u32 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let id = format!("rule-{}", i);
            let payload = json!({ "idx": i, "active": true });
            store.save_rule(&id, &payload).await
        }));
    }

    for handle in handles {
        handle.await.expect("task join failed").expect("save rule failed");
    }

    let rules = store.load_rules().await.expect("load rules failed");
    assert_eq!(rules.len(), 20, "all concurrently written rules should exist");
}

#[tokio::test]
async fn test_concurrent_upsert_same_rule_id_keeps_single_record() {
    let store = Arc::new(Store::connect(&sqlite_url()).await.expect("connect failed"));
    store.init().await.expect("init failed");

    let mut handles = Vec::new();
    for i in 0..16u32 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let payload = json!({ "idx": i, "active": true });
            store.save_rule("rule-shared", &payload).await
        }));
    }

    for handle in handles {
        handle.await.expect("task join failed").expect("save rule failed");
    }

    let rules = store.load_rules().await.expect("load rules failed");
    assert_eq!(rules.len(), 1, "same id concurrent upsert should keep one row");
    assert_eq!(rules[0].0, "rule-shared");
    let idx = rules[0].1["idx"]
        .as_u64()
        .expect("idx should be numeric");
    assert!(idx < 16, "final idx should come from one concurrent writer");
}

#[tokio::test]
async fn test_save_flow_conflict_behavior() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .save_flow("flow-1", &json!({"url":"http://example.com"}))
        .await
        .expect("first save_flow failed");

    let second = store
        .save_flow("flow-1", &json!({"url":"http://example.com/updated"}))
        .await;

    assert!(
        second.is_err(),
        "save_flow currently uses INSERT and should fail on duplicate id"
    );
}

#[tokio::test]
async fn test_concurrent_save_flow_same_id_only_one_succeeds() {
    let store = Arc::new(Store::connect(&sqlite_url()).await.expect("connect failed"));
    store.init().await.expect("init failed");

    let mut handles = Vec::new();
    for i in 0..12u32 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .save_flow("flow-shared", &json!({"idx": i, "url":"http://example.com"}))
                .await
        }));
    }

    let mut ok_count = 0usize;
    let mut err_count = 0usize;
    for handle in handles {
        match handle.await.expect("task join failed") {
            Ok(_) => ok_count += 1,
            Err(_) => err_count += 1,
        }
    }

    assert_eq!(
        ok_count, 1,
        "primary key constraint should allow only one successful insert"
    );
    assert_eq!(err_count, 11, "remaining concurrent inserts should fail");
}

#[tokio::test]
async fn test_audit_event_save_and_query_with_filters() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .save_audit_event(AuditEventRecord {
            id: "audit-1",
            timestamp_ms: 1_700_000_000_000,
            actor: "http",
            kind: "rule_changed",
            target: "rule-1",
            outcome: "success",
            content: &json!({
                "id":"audit-1",
                "timestamp_ms":1_700_000_000_000u64,
                "actor":"http",
                "kind":"rule_changed",
                "target":"rule-1",
                "outcome":"success",
                "details":{"k":"v1"}
            }),
        })
        .await
        .expect("save audit-1 failed");
    store
        .save_audit_event(AuditEventRecord {
            id: "audit-2",
            timestamp_ms: 1_700_000_000_100,
            actor: "probe",
            kind: "policy_updated",
            target: "policy",
            outcome: "failed",
            content: &json!({
                "id":"audit-2",
                "timestamp_ms":1_700_000_000_100u64,
                "actor":"probe",
                "kind":"policy_updated",
                "target":"policy",
                "outcome":"failed",
                "details":{"k":"v2"}
            }),
        })
        .await
        .expect("save audit-2 failed");

    let all = store
        .query_audit_events(None, None, None, None, None, 50)
        .await
        .expect("query all failed");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["id"], "audit-2");
    assert_eq!(all[1]["id"], "audit-1");

    let filtered = store
        .query_audit_events(
            Some(1_700_000_000_050),
            None,
            Some("probe"),
            Some("policy_updated"),
            Some("failed"),
            10,
        )
        .await
        .expect("query filtered failed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["id"], "audit-2");
}

#[tokio::test]
async fn test_flow_upsert_and_load_roundtrip() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .upsert_flow("flow-1", &json!({"id":"flow-1","step":1}))
        .await
        .expect("first upsert failed");
    store
        .upsert_flow("flow-1", &json!({"id":"flow-1","step":2}))
        .await
        .expect("second upsert failed");

    let loaded = store
        .load_flow("flow-1")
        .await
        .expect("load flow failed")
        .expect("flow should exist");
    assert_eq!(loaded["step"], 2);
}

#[tokio::test]
async fn test_flow_summary_query_with_offset_and_filters() {
    let store = Store::connect(&sqlite_url()).await.expect("connect failed");
    store.init().await.expect("init failed");

    store
        .upsert_flow_summary(&relay_core_api::modification::FlowSummary {
            id: "flow-1".to_string(),
            method: "GET".to_string(),
            url: "http://a.example.com/a".to_string(),
            host: "a.example.com".to_string(),
            path: "/a".to_string(),
            status: Some(200),
            duration_ms: Some(10),
            tags: vec![],
            start_time_ms: 1000,
            has_error: false,
            is_websocket: false,
        })
        .await
        .expect("upsert summary 1 failed");
    store
        .upsert_flow_summary(&relay_core_api::modification::FlowSummary {
            id: "flow-2".to_string(),
            method: "POST".to_string(),
            url: "http://a.example.com/b".to_string(),
            host: "a.example.com".to_string(),
            path: "/b".to_string(),
            status: Some(503),
            duration_ms: Some(20),
            tags: vec!["error".to_string()],
            start_time_ms: 2000,
            has_error: true,
            is_websocket: false,
        })
        .await
        .expect("upsert summary 2 failed");
    store
        .upsert_flow_summary(&relay_core_api::modification::FlowSummary {
            id: "flow-3".to_string(),
            method: "GET".to_string(),
            url: "ws://ws.example.com/socket".to_string(),
            host: "ws.example.com".to_string(),
            path: "/socket".to_string(),
            status: Some(101),
            duration_ms: Some(30),
            tags: vec![],
            start_time_ms: 3000,
            has_error: false,
            is_websocket: true,
        })
        .await
        .expect("upsert summary 3 failed");

    let page = store
        .query_flow_summaries(&relay_core_api::modification::FlowQuery {
            host: Some("example.com".to_string()),
            path_contains: None,
            method: None,
            status_min: None,
            status_max: None,
            has_error: None,
            is_websocket: None,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("query page failed");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, "flow-2");

    let errored = store
        .query_flow_summaries(&relay_core_api::modification::FlowQuery {
            host: Some("a.example.com".to_string()),
            path_contains: None,
            method: None,
            status_min: Some(500),
            status_max: None,
            has_error: Some(true),
            is_websocket: Some(false),
            limit: Some(10),
            offset: Some(0),
        })
        .await
        .expect("query filters failed");
    assert_eq!(errored.len(), 1);
    assert_eq!(errored[0].id, "flow-2");
}
