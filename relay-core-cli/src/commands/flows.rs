use crate::args::InterceptAction;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use relay_core_api::modification::{parse_flow_filter, FlowQuery, FlowSummary};
use serde::Deserialize;
use tokio_tungstenite::connect_async;
use tracing::info;

#[derive(Debug, Deserialize)]
struct FlowSearchResponse {
    items: Vec<FlowSummary>,
}

/// CLI flags for `flows` search mode (`GET /api/v1/flows`).
pub struct FlowsOptions {
    pub control_url: String,
    pub api_url: String,
    pub output: String,
    pub filter: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub method: Option<String>,
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
    pub has_error: bool,
    pub websocket: bool,
    pub limit: usize,
}

pub async fn execute(opts: FlowsOptions) -> Result<()> {
    if opts.is_search_mode() {
        return execute_search(opts).await;
    }
    execute_stream(opts.control_url, opts.output).await
}

impl FlowsOptions {
    fn is_search_mode(&self) -> bool {
        self.filter.is_some()
            || self.host.is_some()
            || self.path.is_some()
            || self.method.is_some()
            || self.status_min.is_some()
            || self.status_max.is_some()
            || self.has_error
            || self.websocket
    }

    fn to_flow_query(&self) -> (FlowQuery, Vec<String>) {
        let parsed = self
            .filter
            .as_deref()
            .map(parse_flow_filter)
            .unwrap_or_default();
        let mut query = parsed.query;
        if let Some(h) = &self.host {
            query.host = Some(h.clone());
        }
        if let Some(p) = &self.path {
            query.path_contains = Some(p.clone());
        }
        if let Some(m) = &self.method {
            query.method = Some(m.clone());
        }
        if self.status_min.is_some() {
            query.status_min = self.status_min;
        }
        if self.status_max.is_some() {
            query.status_max = self.status_max;
        }
        if self.has_error {
            query.has_error = Some(true);
        }
        if self.websocket {
            query.is_websocket = Some(true);
        }
        query.limit = Some(self.limit.clamp(1, 200));
        query.offset = Some(0);
        (query, parsed.text_tokens)
    }
}

async fn execute_search(opts: FlowsOptions) -> Result<()> {
    let (query, text_tokens) = opts.to_flow_query();

    let base = opts.api_url.trim_end_matches('/');
    let mut url = reqwest::Url::parse(&format!("{base}/api/v1/flows"))
        .context("invalid --api-url")?;
    {
        let mut pairs = url.query_pairs_mut();
        if let Some(h) = &query.host {
            pairs.append_pair("host", h);
        }
        if let Some(p) = &query.path_contains {
            pairs.append_pair("path_contains", p);
        }
        if let Some(m) = &query.method {
            pairs.append_pair("method", m);
        }
        if let Some(min) = query.status_min {
            pairs.append_pair("status_min", &min.to_string());
        }
        if let Some(max) = query.status_max {
            pairs.append_pair("status_max", &max.to_string());
        }
        if let Some(v) = query.has_error {
            pairs.append_pair("has_error", if v { "true" } else { "false" });
        }
        if let Some(v) = query.is_websocket {
            pairs.append_pair("is_websocket", if v { "true" } else { "false" });
        }
        if let Some(l) = query.limit {
            pairs.append_pair("limit", &l.to_string());
        }
        if let Some(o) = query.offset {
            pairs.append_pair("offset", &o.to_string());
        }
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .send()
        .await
        .context("GET /api/v1/flows failed (is the proxy running with --api-port?)")?
        .error_for_status()
        .context("flows search request rejected")?;

    let body: FlowSearchResponse = resp.json().await.context("decode flows response")?;
    let mut items = body.items;
    if !text_tokens.is_empty() {
        items.retain(|s| summary_matches_text_tokens(s, &text_tokens));
    }

    match opts.output.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&items)?),
        "jsonl" => {
            for item in &items {
                println!("{}", serde_json::to_string(item)?);
            }
        }
        _ => print_flow_table(&items),
    }

    Ok(())
}

fn summary_matches_text_tokens(summary: &FlowSummary, tokens: &[String]) -> bool {
    let url_lc = summary.url.to_ascii_lowercase();
    let method_lc = summary.method.to_ascii_lowercase();
    tokens.iter().all(|t| {
        let needle = t.to_ascii_lowercase();
        url_lc.contains(&needle) || method_lc.contains(&needle)
    })
}

fn print_flow_table(items: &[FlowSummary]) {
    if items.is_empty() {
        println!("No flows matched.");
        return;
    }
    println!(
        "{:<38} {:<8} {:<6} {:<48} {:>8}",
        "ID", "METHOD", "STATUS", "URL", "MS"
    );
    for s in items {
        let status = s
            .status
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());
        let ms = s
            .duration_ms
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".to_string());
        let url = if s.url.len() > 48 {
            format!("{}…", &s.url[..47])
        } else {
            s.url.clone()
        };
        println!(
            "{:<38} {:<8} {:<6} {:<48} {:>8}",
            &s.id[..38.min(s.id.len())],
            s.method,
            status,
            url,
            ms
        );
    }
}

async fn execute_stream(control_url: String, output: String) -> Result<()> {
    let ws_url = if control_url.starts_with("https") {
        control_url.replace("https", "wss") + "/api/flows/ws"
    } else if control_url.starts_with("http") {
        control_url.replace("http", "ws") + "/api/flows/ws"
    } else {
        control_url + "/api/flows/ws"
    };

    info!("Connecting to {}", ws_url);
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to control server: {}", e))?;
    let (_, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to read message from control server: {}",
                    e
                ));
            }
        };

        if !msg.is_text() {
            continue;
        }

        let text = match msg.to_text() {
            Ok(text) => text,
            Err(_) => continue,
        };

        if output == "jsonl" {
            println!("{}", text);
            continue;
        }

        let update = match serde_json::from_str::<relay_core_api::flow::FlowUpdate>(text) {
            Ok(update) => update,
            Err(_) => continue,
        };

        match update {
            relay_core_api::flow::FlowUpdate::Full(flow) => {
                if output == "json" {
                    if let Ok(json) = serde_json::to_string_pretty(&flow) {
                        println!("{}", json);
                    }
                } else {
                    use relay_core_api::flow::Layer;
                    let url = match &flow.layer {
                        Layer::Http(h) => h.request.url.to_string(),
                        Layer::WebSocket(w) => w.handshake_request.url.to_string(),
                        _ => "unknown".to_string(),
                    };
                    let method = match &flow.layer {
                        Layer::Http(h) => h.request.method.clone(),
                        Layer::WebSocket(w) => w.handshake_request.method.clone(),
                        _ => "".to_string(),
                    };
                    info!("[Flow] {} {} {}", flow.id, method, url);
                }
            }
            relay_core_api::flow::FlowUpdate::WebSocketMessage { flow_id, message } => {
                if output == "table" {
                    info!("[WS] [{}] {} bytes", flow_id, message.content.size);
                }
            }
            relay_core_api::flow::FlowUpdate::HttpBody {
                flow_id,
                direction,
                body,
            } => {
                if output == "table" {
                    info!("[Body] [{}] {:?} {} bytes", flow_id, direction, body.size);
                }
            }
        }
    }

    Ok(())
}

pub async fn execute_intercept(action: InterceptAction, control_url: String) -> Result<()> {
    let url = match action {
        InterceptAction::Pause => format!("{}/api/intercept/pause", control_url),
        InterceptAction::Resume => format!("{}/api/intercept/resume", control_url),
    };
    let client = reqwest::Client::new();
    client
        .post(&url)
        .send()
        .await?
        .error_for_status()
        .context("intercept control request failed")?;
    Ok(())
}
