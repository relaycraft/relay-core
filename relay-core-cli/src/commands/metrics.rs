use anyhow::Result;
use relay_core_runtime::CoreMetrics;
use reqwest::Client;

pub async fn execute(proxy_url: String, output: String) -> Result<()> {
    let url = format!("{}/_relay/metrics", proxy_url.trim_end_matches('/'));
    let client = Client::new();
    let resp = client.get(&url).send().await?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("Failed to get metrics: {}", resp.status()));
    }

    let metrics: CoreMetrics = resp.json().await?;

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
