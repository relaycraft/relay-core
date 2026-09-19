use anyhow::{Context, Result};
use relay_core_api::flow::Flow;
use relay_core_runtime::rule::InterceptRule;
use std::path::PathBuf;

pub fn load_rules(path: &PathBuf) -> Result<Vec<InterceptRule>> {
    let content = std::fs::read_to_string(path)?;
    let rules: Vec<InterceptRule> = if path
        .extension()
        .is_some_and(|ext| ext == "yaml" || ext == "yml")
    {
        serde_yaml::from_str(&content)?
    } else {
        serde_json::from_str(&content)?
    };
    Ok(rules)
}

pub fn load_flow(path: &PathBuf) -> Result<Flow> {
    let content = std::fs::read_to_string(path)?;
    let flow: Flow = serde_json::from_str(&content)?;
    Ok(flow)
}

/// Read a recorded flow stream (JSONL).
///
/// Two shapes are accepted because both exist: a bare `Flow` per line (what a stream file used to
/// hold) and a `FlowUpdate` per line (what the engine records now, so the recording is not lossy).
/// Making the reader care which one it was produced by is how a stream file and `relay analyze`
/// drift apart.
pub fn load_flows_jsonl(path: &PathBuf) -> Result<Vec<Flow>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    let flows: Vec<Flow> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match parse_flow_line(line) {
            Ok(Some(flow)) => Some(Ok(flow)),
            // Incremental updates describe a flow already present earlier in the file.
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(flows)
}

/// One line of a recorded stream: a whole flow, or the full-flow variant of an update.
fn parse_flow_line(line: &str) -> Result<Option<Flow>> {
    let value: serde_json::Value = serde_json::from_str(line)
        .with_context(|| format!("Failed to parse flow JSONL line: {line}"))?;

    if value.get("type").is_some() {
        let update: relay_core_api::flow::FlowUpdate = serde_json::from_value(value)
            .with_context(|| format!("Failed to parse flow update line: {line}"))?;
        return Ok(match update {
            relay_core_api::flow::FlowUpdate::Full(flow) => Some(*flow),
            _ => None,
        });
    }

    let flow: Flow = serde_json::from_value(value)
        .with_context(|| format!("Failed to parse flow JSONL line: {line}"))?;
    Ok(Some(flow))
}

pub fn load_flows_har(path: &PathBuf) -> Result<Vec<Flow>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read HAR file: {}", path.display()))?;
    let har: serde_json::Value =
        serde_json::from_str(&content).with_context(|| "Failed to parse HAR JSON")?;

    let entries = har["log"]["entries"]
        .as_array()
        .with_context(|| "HAR file missing 'log.entries' array")?;

    let flows: Vec<Flow> = entries
        .iter()
        .filter_map(|entry| {
            let flow_id = entry["_relaycore"]["flow_id"].as_str();
            let request = &entry["request"];
            let response = &entry["response"];
            let method = request["method"].as_str().unwrap_or("GET");
            let url_str = request["url"].as_str().unwrap_or("http://unknown/");
            let url = url::Url::parse(url_str).ok()?;
            let status = response["status"].as_u64().map(|s| s as u16).unwrap_or(0);
            let status_text = response["statusText"].as_str().unwrap_or("").to_string();

            let timing = &entry["timings"];
            let ttl = timing["time"].as_u64();

            let flow = Flow {
                id: flow_id
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .unwrap_or_else(uuid::Uuid::new_v4),
                start_time: entry["startedDateTime"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(chrono::Utc::now),
                end_time: ttl.map(|_ms| {
                    chrono::Utc::now() // HAR doesn't have end_time; approximate for analysis
                }),
                close_reason: None,
                network: relay_core_api::flow::NetworkInfo {
                    client_ip: entry["_relaycore"]["client_ip"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    client_port: 0,
                    server_ip: entry["_relaycore"]["server_ip"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    server_port: 0,
                    server_host: None,
                    protocol: relay_core_api::flow::TransportProtocol::TCP,
                    tls: url_str.starts_with("https://"),
                    tls_version: None,
                    sni: None,
                },
                layer: relay_core_api::flow::Layer::Http(relay_core_api::flow::HttpLayer {
                    request: relay_core_api::flow::HttpRequest {
                        method: method.to_string(),
                        url,
                        version: request["httpVersion"]
                            .as_str()
                            .unwrap_or("HTTP/1.1")
                            .to_string(),
                        headers: request["headers"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|h| {
                                        Some((
                                            h["name"].as_str()?.to_string(),
                                            h["value"].as_str()?.to_string(),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        cookies: vec![],
                        query: request["queryString"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|q| {
                                        Some((
                                            q["name"].as_str()?.to_string(),
                                            q["value"].as_str()?.to_string(),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        body: request["postData"]["text"].as_str().map(|text| {
                            relay_core_api::flow::BodyData {
                                encoding: "utf-8".to_string(),
                                content: text.to_string(),
                                size: text.len() as u64,
                                grpc: None,
                            }
                        }),
                    },
                    response: Some(relay_core_api::flow::HttpResponse {
                        status,
                        status_text,
                        version: response["httpVersion"]
                            .as_str()
                            .unwrap_or("HTTP/1.1")
                            .to_string(),
                        headers: response["headers"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|h| {
                                        Some((
                                            h["name"].as_str()?.to_string(),
                                            h["value"].as_str()?.to_string(),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        cookies: vec![],
                        body: response["content"]["text"].as_str().map(|text| {
                            relay_core_api::flow::BodyData {
                                encoding: response["content"]["encoding"]
                                    .as_str()
                                    .unwrap_or("utf-8")
                                    .to_string(),
                                content: text.to_string(),
                                size: response["content"]["size"].as_u64().unwrap_or(0),
                                grpc: None,
                            }
                        }),
                        trailers: vec![],
                        timing: relay_core_api::flow::ResponseTiming {
                            time_to_first_byte: timing["wait"].as_u64(),
                            time_to_last_byte: timing["time"].as_u64(),
                            connect_time_ms: if timing["connect"].as_i64().unwrap_or(-1) >= 0 {
                                Some(timing["connect"].as_u64().unwrap_or(0))
                            } else {
                                None
                            },
                            ssl_time_ms: if timing["ssl"].as_i64().unwrap_or(-1) >= 0 {
                                Some(timing["ssl"].as_u64().unwrap_or(0))
                            } else {
                                None
                            },
                        },
                    }),
                    error: if status >= 400 {
                        Some(format!("HTTP {}", status))
                    } else {
                        None
                    },
                }),
                tags: entry["_relaycore"]["tags"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                meta: std::collections::HashMap::new(),
                resilience_trace: None,
                rule_variables: std::collections::HashMap::new(),
                matched_rules: vec![],
            };
            Some(flow)
        })
        .collect();

    Ok(flows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_flow_json() -> serde_json::Value {
        serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "start_time": "2026-05-20T10:00:00Z",
            "end_time": null,
            "network": {
                "client_ip": "127.0.0.1",
                "client_port": 1234,
                "server_ip": "0.0.0.0",
                "server_port": 0,
                "protocol": "TCP",
                "tls": false
            },
            "layer": {
                "type": "Http",
                "data": {
                    "request": {
                        "method": "GET",
                        "url": "http://example.com/",
                        "version": "HTTP/1.1",
                        "headers": [],
                        "cookies": [],
                        "query": [],
                        "body": null
                    },
                    "response": null,
                    "error": null
                }
            },
            "tags": [],
            "meta": {}
        })
    }

    fn write_lines(lines: &[String]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("stream.jsonl");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        (dir, path)
    }

    #[test]
    fn a_recorded_stream_reads_both_flow_and_update_lines() {
        let flow = sample_flow_json();
        let full_update = serde_json::json!({ "type": "Full", "data": flow.clone() });

        // Built from the real type rather than hand-written JSON: a hand-written shape is a second
        // definition of the wire format, which is how readers and writers drift apart.
        let body_update =
            serde_json::to_string(&relay_core_api::flow::FlowUpdate::BodyBudgetExceeded {
                flow_id: "00000000-0000-0000-0000-000000000001".to_string(),
                direction: relay_core_api::flow::Direction::ClientToServer,
            })
            .expect("serialize update");

        let (_dir, path) = write_lines(&[flow.to_string(), full_update.to_string(), body_update]);

        let flows = load_flows_jsonl(&path).expect("both shapes must be readable");
        assert_eq!(
            flows.len(),
            2,
            "the incremental update is not a flow of its own"
        );
        assert!(
            flows
                .iter()
                .all(|flow| flow.id.to_string().ends_with("0001"))
        );
    }

    #[test]
    fn a_broken_line_names_the_line() {
        let (_dir, path) = write_lines(&["{ this is not json".to_string()]);
        let error = load_flows_jsonl(&path).expect_err("a broken line must be reported");
        assert!(
            error.to_string().contains("JSONL line"),
            "the error must point at the line: {error}"
        );
    }
}
