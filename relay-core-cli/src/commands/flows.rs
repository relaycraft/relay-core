use crate::sse_client;
use anyhow::{Context, Result, bail};
use relay_core_api::CLI_COMMAND;
use relay_core_api::modification::{FlowQuery, FlowSummary, parse_flow_filter};
use relay_core_http::control::{DaemonStatus, connect};
use relay_core_runtime::paths;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Deserialize)]
struct FlowSearchResponse {
    items: Vec<FlowSummary>,
}

/// CLI flags for `relay flows`.
pub struct FlowsOptions {
    /// Control API base URL. Discovered from the daemon manifest when omitted.
    pub api_url: Option<String>,
    /// Follow live traffic instead of listing what has been captured.
    pub follow: bool,
    pub output: String,
    pub filter: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub method: Option<String>,
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
    pub has_error: bool,
    /// Only WebSocket flows, and search rather than stream.
    pub websocket: bool,
    pub limit: usize,
}

pub async fn execute(opts: FlowsOptions) -> Result<()> {
    let daemon = resolve_daemon(opts.api_url.as_deref()).await?;

    if opts.follow {
        // A filter that silently does nothing is worse than a refusal: `relay flows --follow
        // --host api.example.com` would look like it was filtering and would not be.
        if opts.has_filters() {
            bail!(
                "filters apply to a listing, not to a live stream; drop --follow to search, or \
                 drop the filters to follow everything"
            );
        }
        return execute_stream(daemon).await;
    }

    execute_search(opts, &daemon).await
}

/// Where the daemon is, and how to authenticate to it.
///
/// Discovered rather than defaulted: a daemon that fell back to an ephemeral port would make a
/// hard-coded `127.0.0.1:8082` connect to something else, or to nothing.
struct ResolvedDaemon {
    base_url: String,
    token: Option<String>,
}

async fn resolve_daemon(explicit: Option<&str>) -> Result<ResolvedDaemon> {
    if let Some(url) = explicit {
        return Ok(ResolvedDaemon {
            base_url: url.trim_end_matches('/').to_string(),
            token: std::env::var("RELAY_API_TOKEN").ok(),
        });
    }

    let data_dir = paths::resolve_data_dir();
    match connect(&data_dir).await {
        DaemonStatus::Running { manifest, .. } => Ok(ResolvedDaemon {
            base_url: manifest.control_base_url(),
            token: manifest.token.clone(),
        }),
        DaemonStatus::NotRunning => bail!(
            "no RelayCore daemon is running in {}. Start one with `{CLI_COMMAND} start`, or pass --api-url.",
            data_dir.display()
        ),
        DaemonStatus::Unresponsive { manifest, reason } => bail!(
            "the RelayCore daemon (pid {}) failed the control handshake at {} ({reason})",
            manifest.pid,
            manifest.control_base_url()
        ),
        DaemonStatus::Incompatible { found, expected } => bail!(
            "the running RelayCore daemon speaks control protocol {found}, this build speaks {expected}"
        ),
    }
}

impl FlowsOptions {
    fn has_filters(&self) -> bool {
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

async fn execute_search(opts: FlowsOptions, daemon: &ResolvedDaemon) -> Result<()> {
    let (query, text_tokens) = opts.to_flow_query();

    let base = daemon.base_url.as_str();
    let mut url =
        reqwest::Url::parse(&format!("{base}/api/v1/flows")).context("invalid --api-url")?;
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
    let mut request = client.get(url);
    if let Some(token) = daemon.token.as_deref() {
        request = request.bearer_auth(token);
    }
    let resp = request
        .send()
        .await
        .context("GET /api/v1/flows failed (is the daemon running?)")?
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

/// Stream live traffic from the daemon.
///
/// The transport is the control API's own event stream (`/api/v1/events`), the same one the Web UI
/// and the TUI read, so `relay flows` cannot drift from what other clients see.
async fn execute_stream(daemon: ResolvedDaemon) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<relay_core_api::flow::FlowUpdate>(256);
    let client = sse_client::ApiClient::new(daemon.base_url.clone(), daemon.token.clone());

    info!("Streaming from {}/api/v1/events", daemon.base_url);

    let reader = tokio::spawn(async move { client.stream_events(tx).await });

    while let Some(update) = rx.recv().await {
        print_update(&update);
    }

    match reader.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!("flow stream task failed: {error}")),
    }
}

/// One line per update, in the requested format.
fn print_update(update: &relay_core_api::flow::FlowUpdate) {
    use relay_core_api::flow::{FlowUpdate, Layer};

    match update {
        FlowUpdate::Full(flow) => {
            let url = match &flow.layer {
                Layer::Http(http) => http.request.url.to_string(),
                Layer::WebSocket(ws) => ws.handshake_request.url.to_string(),
                _ => "unknown".to_string(),
            };
            let method = match &flow.layer {
                Layer::Http(http) => http.request.method.clone(),
                Layer::WebSocket(ws) => ws.handshake_request.method.clone(),
                _ => String::new(),
            };
            info!("[Flow] {} {} {}", flow.id, method, url);
        }
        FlowUpdate::WebSocketMessage { flow_id, message } => {
            info!("[WS] [{}] {} bytes", flow_id, message.content.size);
        }
        FlowUpdate::HttpBody {
            flow_id,
            direction,
            body,
        } => {
            info!("[Body] [{}] {:?} {} bytes", flow_id, direction, body.size);
        }
        FlowUpdate::BodyBudgetExceeded { flow_id, direction } => {
            info!("[BudgetExceeded] [{}] {:?}", flow_id, direction);
        }
        FlowUpdate::ResponseTrailers { flow_id, trailers } => {
            // gRPC reports the outcome of a call here, so it is worth showing.
            info!("[Trailers] [{}] {} trailer(s)", flow_id, trailers.len());
        }
    }
}
