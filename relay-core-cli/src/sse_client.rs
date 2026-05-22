use anyhow::Result;
use relay_core_api::flow::{Flow, FlowUpdate};
use relay_core_api::rule::Rule;
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

const INITIAL_RECONNECT_DELAY_MS: u64 = 200;
const MAX_RECONNECT_DELAY_MS: u64 = 8_000;
const MAX_SSE_BUFFER_BYTES: usize = 1_048_576; // 1 MB

pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl ApiClient {
    pub fn new(base_url: String, token: Option<String>) -> Self {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
        if token.is_some() {
            builder = builder.danger_accept_invalid_certs(true);
        }
        Self {
            http: builder.build().expect("Failed to build reqwest client"),
            base_url,
            token,
        }
    }

    pub async fn fetch_rules(&self) -> Result<Vec<Rule>> {
        let mut req = self.http.get(format!("{}/api/v1/rules", self.base_url));
        if let Some(ref token) = self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let body: Value = resp.json().await?;
        let rules: Vec<Rule> = serde_json::from_value(body).unwrap_or_else(|_| Vec::new());
        Ok(rules)
    }

    pub async fn fetch_intercept_summary(&self) -> Result<Value> {
        let mut req = self
            .http
            .get(format!("{}/api/v1/intercepts", self.base_url));
        if let Some(ref token) = self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let body: Value = resp.json().await?;
        Ok(body)
    }

    pub async fn stream_events(&self, tx: mpsc::Sender<FlowUpdate>) -> Result<()> {
        let url = format!("{}/api/v1/events", self.base_url);
        let mut delay_ms = INITIAL_RECONNECT_DELAY_MS;

        loop {
            let mut req = self
                .http
                .get(&url)
                .header("Accept", "text/event-stream")
                .header("Cache-Control", "no-cache");

            if let Some(ref token) = self.token {
                req = req.bearer_auth(token);
            }

            match req.send().await {
                Ok(mut resp) => {
                    if resp.status() != StatusCode::OK {
                        tracing::warn!("SSE endpoint returned {}, retrying...", resp.status());
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        delay_ms = (delay_ms * 2).min(MAX_RECONNECT_DELAY_MS);
                        continue;
                    }

                    delay_ms = INITIAL_RECONNECT_DELAY_MS;
                    let mut buf = String::new();

                    loop {
                        match resp.chunk().await {
                            Ok(Some(chunk)) => {
                                buf.push_str(&String::from_utf8_lossy(&chunk));
                                // Cap buffer to prevent OOM on malformed stream
                                if buf.len() > MAX_SSE_BUFFER_BYTES {
                                    let _ = buf.drain(..buf.len() - MAX_SSE_BUFFER_BYTES / 2);
                                }
                                while let Some(pos) = buf.find("\n\n") {
                                    let frame = buf[..pos].to_string();
                                    buf = buf[pos + 2..].to_string();
                                    if let Some(update) = parse_sse_frame(&frame)
                                        && tx.send(update).await.is_err()
                                    {
                                        return Ok(());
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                tracing::warn!("SSE stream error: {}, reconnecting...", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "SSE connection failed: {}, retrying in {}ms...",
                        e,
                        delay_ms
                    );
                }
            }

            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(MAX_RECONNECT_DELAY_MS);
        }
    }
}

/// Parse one SSE frame from `/api/v1/events`.
///
/// The HTTP adapter emits raw [`Flow`] JSON for `event: flow`, not tagged [`FlowUpdate`].
pub fn parse_sse_frame(frame: &str) -> Option<FlowUpdate> {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in frame.lines() {
        if let Some(field) = line.strip_prefix("event:") {
            event_type = field.trim().to_string();
        } else if let Some(field) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(field.trim());
        }
    }

    if data.is_empty() {
        return None;
    }

    match event_type.as_str() {
        "flow" => {
            if let Ok(update) = serde_json::from_str::<FlowUpdate>(&data) {
                return Some(update);
            }
            serde_json::from_str::<Flow>(&data)
                .ok()
                .map(|flow| FlowUpdate::Full(Box::new(flow)))
        }
        "ws-message" => serde_json::from_str(&data).ok(),
        "http-body" => None, // TUI only needs full flow snapshots for the list view.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_sse_frame;
    use relay_core_api::flow::{FlowUpdate, Layer};

    #[test]
    fn parse_flow_event_accepts_raw_flow_json() {
        let frame = r#"event: flow
data: {"id":"00000000-0000-0000-0000-000000000001","start_time":"2026-05-20T10:00:00Z","end_time":null,"network":{"client_ip":"127.0.0.1","client_port":12345,"server_ip":"0.0.0.0","server_port":0,"protocol":"TCP","tls":true,"tls_version":null,"sni":null},"layer":{"type":"Http","data":{"request":{"method":"GET","url":"https://example.com/","version":"HTTP/1.1","headers":[],"cookies":[],"query":[],"body":null},"response":null,"error":null}},"tags":["proxy"],"meta":{}}"#;

        let update = parse_sse_frame(frame).expect("flow frame should parse");
        match update {
            FlowUpdate::Full(flow) => match &flow.layer {
                Layer::Http(http) => {
                    assert_eq!(http.request.method, "GET");
                    assert_eq!(http.request.url.as_str(), "https://example.com/");
                }
                other => panic!("expected http layer, got {other:?}"),
            },
            other => panic!("expected full flow update, got {other:?}"),
        }
    }

    #[test]
    fn parse_flow_event_accepts_tagged_flow_update_json() {
        let frame = r#"event: flow
data: {"type":"Full","data":{"id":"00000000-0000-0000-0000-000000000002","start_time":"2026-05-20T10:00:00Z","end_time":null,"network":{"client_ip":"127.0.0.1","client_port":12345,"server_ip":"0.0.0.0","server_port":0,"protocol":"TCP","tls":false,"tls_version":null,"sni":null},"layer":{"type":"Http","data":{"request":{"method":"POST","url":"https://example.com/submit","version":"HTTP/1.1","headers":[],"cookies":[],"query":[],"body":null},"response":null,"error":null}},"tags":[],"meta":{}}}"#;

        let update = parse_sse_frame(frame).expect("tagged flow update should parse");
        match update {
            FlowUpdate::Full(flow) => match &flow.layer {
                Layer::Http(http) => assert_eq!(http.request.method, "POST"),
                other => panic!("expected http layer, got {other:?}"),
            },
            other => panic!("expected full flow update, got {other:?}"),
        }
    }
}
