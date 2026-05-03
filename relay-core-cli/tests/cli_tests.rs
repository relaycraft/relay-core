use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_help() {
    let mut cmd = Command::cargo_bin("relay-core-cli").unwrap();
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

    let mut cmd = Command::cargo_bin("relay-core-cli").unwrap();
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
    let mut cmd = Command::cargo_bin("relay-core-cli").unwrap();
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

    let mut cmd = Command::cargo_bin("relay-core-cli").unwrap();
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

    let mut cmd = Command::cargo_bin("relay-core-cli").unwrap();
    cmd.arg("rules")
        .arg("validate")
        .arg(&rule_path)
        .assert()
        .failure();
}
