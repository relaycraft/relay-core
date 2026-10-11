use super::daemon_client::resolve_daemon;
use anyhow::{Context, Result};

/// Print the engine's runtime metrics.
///
/// The engine exposes the same `CoreMetrics` on **two** surfaces, and both are deliberate:
///
/// * the proxy listener answers `GET /_relay/metrics` (and `/_relay/metrics/prometheus`) from
///   `MetricsInterceptor`, so a load balancer, a Prometheus scrape or a scheduler can read metrics
///   without a credential;
/// * the control API answers `GET /api/v1/metrics`, which additionally works while the proxy is
///   stopped and is covered by the daemon's bearer token.
///
/// `relay-core metrics` reached only the first, through a `--proxy-url` whose default was the proxy's
/// default port — so it worked against a proxy on 8080 and against nothing else. It now prefers the
/// daemon (discovered, like every other command) and keeps `--proxy-url` for the case the daemon
/// cannot answer: reading a proxy that this CLI has no manifest for.
pub async fn execute(
    api_url: Option<String>,
    proxy_url: Option<String>,
    output: String,
) -> Result<()> {
    let metrics = match proxy_url {
        Some(proxy_url) => from_proxy(&proxy_url).await?,
        None => {
            resolve_daemon(api_url.as_deref())
                .await?
                .client("cli:relay metrics")
                .metrics()
                .await?
        }
    };

    if output == "json" {
        println!("{}", serde_json::to_string_pretty(&metrics)?);
    } else {
        println!("{:<25} | {:<10}", "Metric", "Value");
        println!("{:-<25}-|-{:-<10}", "", "");
        println!("{:<25} | {:<10}", "Flows Total", metrics.flows_total);
        println!(
            "{:<25} | {:<10}",
            "Flows In Memory", metrics.flows_in_memory
        );
        println!("{:<25} | {:<10}", "Flows Dropped", metrics.flows_dropped);
        println!(
            "{:<25} | {:<10}",
            "Intercepts Pending", metrics.intercepts_pending
        );
        println!(
            "{:<25} | {:<10}",
            "WS Pending Messages", metrics.ws_pending_messages
        );
        println!(
            "{:<25} | {:<10}",
            "Oldest Intercept Age",
            metrics
                .oldest_intercept_age_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string())
        );
        println!(
            "{:<25} | {:<10}",
            "Oldest WS Msg Age",
            metrics
                .oldest_ws_message_age_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string())
        );
        println!(
            "{:<25} | {:<10}",
            "Rule Exec Errors", metrics.rule_exec_errors
        );
        println!(
            "{:<25} | {:<10}",
            "Audit Events Total", metrics.audit_events_total
        );
        println!(
            "{:<25} | {:<10}",
            "Audit Events Failed", metrics.audit_events_failed
        );
        println!(
            "{:<25} | {:<10}",
            "Flow Events Lagged", metrics.flow_events_lagged_total
        );
        println!(
            "{:<25} | {:<10}",
            "Audit Events Lagged", metrics.audit_events_lagged_total
        );
    }

    Ok(())
}

/// Read the proxy's own metrics endpoint.
///
/// Never use the system proxy settings: this call is aimed *at* the proxy's loopback listener, and
/// on Windows a configured system proxy would send it back through the proxy itself (os error 10013).
async fn from_proxy(proxy_url: &str) -> Result<relay_core_runtime::CoreMetrics> {
    let url = format!("{}/_relay/metrics", proxy_url.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("no proxy answered at {url}"))?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "{url} answered {} instead of metrics",
            response.status()
        ));
    }
    response
        .json()
        .await
        .with_context(|| format!("{url} did not answer with metrics JSON"))
}
