use anyhow::Result;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::rule::Rule;
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

const INITIAL_RECONNECT_DELAY_MS: u64 = 200;
const MAX_RECONNECT_DELAY_MS: u64 = 8_000;

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

fn parse_sse_frame(frame: &str) -> Option<FlowUpdate> {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in frame.lines() {
        if let Some(field) = line.strip_prefix("event:") {
            event_type = field.trim().to_string();
        } else if let Some(field) = line.strip_prefix("data:") {
            data = field.trim().to_string();
        }
    }

    if event_type == "flow"
        && !data.is_empty()
        && let Ok(update) = serde_json::from_str::<FlowUpdate>(&data)
    {
        return Some(update);
    }

    None
}
