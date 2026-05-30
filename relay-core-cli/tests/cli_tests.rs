use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_help() {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Start the proxy server"));
}

#[test]
fn test_ca_init() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("test_ca.pem");
    let key_path = dir.path().join("test_key.pem");

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("ca")
        .arg("init")
        .arg("--cert")
        .arg(&cert_path)
        .arg("--key")
        .arg(&key_path)
        .assert()
        .success();

    assert!(cert_path.exists());
    assert!(key_path.exists());

    // Test CA status
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("ca")
        .arg("status")
        .arg("--cert")
        .arg(&cert_path)
        .assert()
        .success();
}

#[test]
fn test_rules_validate_success() {
    let dir = tempdir().unwrap();
    let rule_path = dir.path().join("valid_rule.yaml");

    // Create a simple valid rule (Must be a list)
    let content = r#"
- id: "test-rule-1"
  active: true
  url_pattern: "example.com"
  phase: "request"
"#;
    fs::write(&rule_path, content).unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("rules")
        .arg("validate")
        .arg(&rule_path)
        .assert()
        .success();
}

#[test]
fn test_rules_validate_fail() {
    let dir = tempdir().unwrap();
    let rule_path = dir.path().join("invalid_rule.yaml");

    // Create an invalid rule (malformed YAML)
    let content = r#"
name: "Test Rule"
filters: [
  - type: host
    value: "example.com"
"#;
    fs::write(&rule_path, content).unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("rules")
        .arg("validate")
        .arg(&rule_path)
        .assert()
        .failure();
}

#[test]
fn test_analyze_basic() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test_flows.jsonl");

    // Create a sample JSONL flow file with a few HTTP flows
    let flow1 = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "start_time": "2026-05-20T10:00:00Z",
        "end_time": "2026-05-20T10:00:01Z",
        "network": {
            "client_ip": "127.0.0.1",
            "client_port": 12345,
            "server_ip": "93.184.216.34",
            "server_port": 443,
            "protocol": "TCP",
            "tls": true,
            "tls_version": "TLSv1.3",
            "sni": "example.com"
        },
        "layer": {
            "type": "Http",
            "data": {
                "request": {
                    "method": "GET",
                    "url": "https://example.com/index.html",
                    "version": "HTTP/1.1",
                    "headers": [["Host", "example.com"], ["Accept", "*/*"]],
                    "cookies": [],
                    "query": [],
                    "body": null
                },
                "response": {
                    "status": 200,
                    "status_text": "OK",
                    "version": "HTTP/1.1",
                    "headers": [["Content-Type", "text/html"]],
                    "cookies": [],
                    "body": {
                        "encoding": "utf-8",
                        "content": "<html><body>Hello</body></html>",
                        "size": 29
                    },
                    "timing": {
                        "time_to_first_byte": 42,
                        "time_to_last_byte": 50,
                        "connect_time_ms": null,
                        "ssl_time_ms": null
                    }
                },
                "error": null
            }
        },
        "tags": [],
        "meta": {}
    });
    let flow2 = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000002",
        "start_time": "2026-05-20T10:00:02Z",
        "end_time": "2026-05-20T10:00:04Z",
        "network": {
            "client_ip": "127.0.0.1",
            "client_port": 12346,
            "server_ip": "93.184.216.34",
            "server_port": 443,
            "protocol": "TCP",
            "tls": true,
            "tls_version": "TLSv1.3",
            "sni": "example.com"
        },
        "layer": {
            "type": "Http",
            "data": {
                "request": {
                    "method": "POST",
                    "url": "https://example.com/api/submit",
                    "version": "HTTP/1.1",
                    "headers": [["Host", "example.com"], ["Content-Type", "application/json"]],
                    "cookies": [],
                    "query": [],
                    "body": {
                        "encoding": "utf-8",
                        "content": "{\"key\":\"value\"}",
                        "size": 15
                    }
                },
                "response": {
                    "status": 500,
                    "status_text": "Internal Server Error",
                    "version": "HTTP/1.1",
                    "headers": [],
                    "cookies": [],
                    "body": {
                        "encoding": "utf-8",
                        "content": "Internal Error",
                        "size": 14
                    },
                    "timing": {
                        "time_to_first_byte": 1500,
                        "time_to_last_byte": 1500,
                        "connect_time_ms": null,
                        "ssl_time_ms": null
                    }
                },
                "error": null
            }
        },
        "tags": ["error"],
        "meta": {}
    });
    let flow3 = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000003",
        "start_time": "2026-05-20T10:00:05Z",
        "end_time": "2026-05-20T10:00:05Z",
        "network": {
            "client_ip": "127.0.0.1",
            "client_port": 12347,
            "server_ip": "142.250.80.46",
            "server_port": 443,
            "protocol": "TCP",
            "tls": true,
            "tls_version": "TLSv1.3",
            "sni": "google.com"
        },
        "layer": {
            "type": "Http",
            "data": {
                "request": {
                    "method": "GET",
                    "url": "https://google.com/search?q=rust",
                    "version": "HTTP/2",
                    "headers": [["Host", "google.com"]],
                    "cookies": [],
                    "query": [["q", "rust"]],
                    "body": null
                },
                "response": {
                    "status": 302,
                    "status_text": "Found",
                    "version": "HTTP/2",
                    "headers": [["Location", "/"]],
                    "cookies": [],
                    "body": null,
                    "timing": {
                        "time_to_first_byte": 100,
                        "time_to_last_byte": 100,
                        "connect_time_ms": 15,
                        "ssl_time_ms": 30
                    }
                },
                "error": null
            }
        },
        "tags": [],
        "meta": {}
    });

    let jsonl = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&flow1).unwrap(),
        serde_json::to_string(&flow2).unwrap(),
        serde_json::to_string(&flow3).unwrap()
    );
    fs::write(&file_path, jsonl).unwrap();

    // Test table output
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("analyze")
        .arg("--file")
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Host Histogram"))
        .stdout(predicate::str::contains("Method Histogram"))
        .stdout(predicate::str::contains("Status Code Distribution"))
        .stdout(predicate::str::contains("Slow Requests"))
        .stdout(predicate::str::contains("Error Clustering"))
        .stdout(predicate::str::contains("example.com"))
        .stdout(predicate::str::contains("GET"))
        .stdout(predicate::str::contains("POST"))
        .stdout(predicate::str::contains("5xx Server Error"))
        .stdout(predicate::str::contains("Total flows"))
        .stdout(predicate::str::contains("Error flows"));
}

#[test]
fn test_analyze_json_output() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test_flows.jsonl");

    let flow = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "start_time": "2026-05-20T10:00:00Z",
        "end_time": "2026-05-20T10:00:01Z",
        "network": {
            "client_ip": "127.0.0.1",
            "client_port": 12345,
            "server_ip": "93.184.216.34",
            "server_port": 443,
            "protocol": "TCP",
            "tls": true,
            "tls_version": null,
            "sni": null
        },
        "layer": {
            "type": "Http",
            "data": {
                "request": {
                    "method": "GET",
                    "url": "https://example.com/",
                    "version": "HTTP/1.1",
                    "headers": [],
                    "cookies": [],
                    "query": [],
                    "body": null
                },
                "response": {
                    "status": 200,
                    "status_text": "OK",
                    "version": "HTTP/1.1",
                    "headers": [],
                    "cookies": [],
                    "body": null,
                    "timing": {
                        "time_to_first_byte": null,
                        "time_to_last_byte": null,
                        "connect_time_ms": null,
                        "ssl_time_ms": null
                    }
                },
                "error": null
            }
        },
        "tags": [],
        "meta": {}
    });

    fs::write(&file_path, serde_json::to_string(&flow).unwrap() + "\n").unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("analyze")
        .arg("--file")
        .arg(&file_path)
        .arg("--output")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"summary\""))
        .stdout(predicate::str::contains("\"total_flows\""))
        .stdout(predicate::str::contains("\"host_histogram\""))
        .stdout(predicate::str::contains("\"method_histogram\""))
        .stdout(predicate::str::contains("\"status_histogram\""))
        .stdout(predicate::str::contains("\"slow_requests\""))
        .stdout(predicate::str::contains("\"error_clusters\""));
}

#[test]
fn test_analyze_empty_file() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("empty.jsonl");
    fs::write(&file_path, "").unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("analyze")
        .arg("--file")
        .arg(&file_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("No flows found"));
}

#[test]
fn test_analyze_help() {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("analyze")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyze offline flow data"));
}

#[test]
fn test_rules_parse_failure_exits_nonzero() {
    let dir = tempdir().unwrap();
    let invalid = dir.path().join("invalid.txt");
    fs::write(&invalid, "not valid json or yaml {{{").unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("rules")
        .arg("validate")
        .arg(&invalid)
        .assert()
        .failure();
}

#[test]
fn test_rules_list_help() {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("relay-core-cli");
    cmd.arg("rules")
        .arg("list")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--api-url"));
}
