use anyhow::Result;
use relay_core_api::flow::{Flow, Layer};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct AnalyzeOptions {
    pub file: PathBuf,
    pub format: String,
    pub output: String,
    pub top_n: usize,
}

pub fn execute(opts: AnalyzeOptions) -> Result<()> {
    let flows = match opts.format.as_str() {
        "har" => crate::utils::load_flows_har(&opts.file)?,
        _ => crate::utils::load_flows_jsonl(&opts.file)?,
    };

    if flows.is_empty() {
        eprintln!("No flows found in file: {}", opts.file.display());
        return Ok(());
    }

    let http_flows: Vec<&Flow> = flows
        .iter()
        .filter(|f| matches!(f.layer, Layer::Http(_)))
        .collect();

    if http_flows.is_empty() {
        eprintln!("No HTTP flows found in file.");
        return Ok(());
    }

    let total = http_flows.len();
    let error_count = http_flows.iter().filter(|f| has_error(f)).count();
    let avg_duration = average_duration(&http_flows);
    let total_body_bytes: u64 = http_flows.iter().map(|f| response_body_size(f)).sum();

    match opts.output.as_str() {
        "json" => print_json(
            &http_flows,
            total,
            error_count,
            avg_duration,
            total_body_bytes,
            opts.top_n,
        )?,
        _ => print_table(
            &http_flows,
            total,
            error_count,
            avg_duration,
            total_body_bytes,
            opts.top_n,
        ),
    }

    Ok(())
}

fn has_error(flow: &Flow) -> bool {
    if let Layer::Http(h) = &flow.layer
        && let Some(resp) = &h.response
    {
        return resp.status >= 400 || resp.status == 0;
    }
    flow.tags.iter().any(|t| t == "error")
}

fn response_body_size(flow: &Flow) -> u64 {
    if let Layer::Http(h) = &flow.layer
        && let Some(resp) = &h.response
    {
        return resp.body.as_ref().map(|b| b.size).unwrap_or(0);
    }
    0
}

fn average_duration(flows: &[&Flow]) -> Option<u64> {
    let mut total_ms = 0u64;
    let mut count = 0u64;
    for f in flows {
        if let Some(end) = f.end_time {
            let dur = (end - f.start_time).num_milliseconds();
            if dur > 0 {
                total_ms += dur as u64;
                count += 1;
            }
        }
    }
    total_ms.checked_div(count)
}

fn duration_ms(flow: &Flow) -> Option<u64> {
    flow.end_time
        .map(|end| (end - flow.start_time).num_milliseconds().max(0) as u64)
}

fn host_from_flow(flow: &Flow) -> Option<String> {
    if let Layer::Http(h) = &flow.layer {
        h.request.url.host_str().map(|s| s.to_string())
    } else {
        None
    }
}

fn method_from_flow(flow: &Flow) -> &str {
    if let Layer::Http(h) = &flow.layer {
        &h.request.method
    } else {
        "UNKNOWN"
    }
}

fn status_from_flow(flow: &Flow) -> u16 {
    if let Layer::Http(h) = &flow.layer {
        h.response.as_ref().map(|r| r.status).unwrap_or(0)
    } else {
        0
    }
}

fn status_category(status: u16) -> &'static str {
    match status {
        0 => "No Response",
        100..=199 => "1xx Informational",
        200..=299 => "2xx Success",
        300..=399 => "3xx Redirect",
        400..=499 => "4xx Client Error",
        500..=599 => "5xx Server Error",
        _ => "Unknown",
    }
}

fn error_message(flow: &Flow) -> Option<String> {
    if let Layer::Http(h) = &flow.layer {
        if let Some(err) = &h.error {
            return Some(err.clone());
        }
        if let Some(resp) = &h.response {
            if resp.status >= 400 {
                return Some(format!("HTTP {} {}", resp.status, resp.status_text));
            }
            if resp.status == 0 {
                return Some("No response received".to_string());
            }
        } else {
            return Some("No response".to_string());
        }
    }
    None
}

// ── Table output ──────────────────────────────────────────────

fn print_table(
    flows: &[&Flow],
    total: usize,
    error_count: usize,
    avg_duration: Option<u64>,
    total_body: u64,
    top_n: usize,
) {
    println!("╔══════════════════════════════════════════╗");
    println!("║         RelayCore Flow Analysis         ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║ Total flows:    {:<6}                  ║", total);
    println!(
        "║ Error flows:    {:<6} ({:.1}%)          ║",
        error_count,
        if total > 0 {
            error_count as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    if let Some(avg) = avg_duration {
        println!("║ Avg duration:   {:<6} ms                ║", avg);
    }
    println!(
        "║ Total resp. body: {:>10}                ║",
        format_bytes(total_body)
    );
    println!("╚══════════════════════════════════════════╝");
    println!();

    print_host_histogram(flows);
    println!();
    print_method_histogram(flows);
    println!();
    print_status_histogram(flows);
    println!();
    print_slow_requests(flows, top_n);
    println!();
    print_error_clustering(flows);
}

fn print_host_histogram(flows: &[&Flow]) {
    let mut hosts: HashMap<String, usize> = HashMap::new();
    for f in flows {
        if let Some(host) = host_from_flow(f) {
            *hosts.entry(host).or_default() += 1;
        }
    }
    let mut sorted: Vec<_> = hosts.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    println!("── Host Histogram ──");
    let max_count = sorted.first().map(|(_, c)| *c).unwrap_or(1);
    for (host, count) in sorted.iter().take(20) {
        let bar = histogram_bar(*count, max_count, 30);
        println!("  {:>5}  {:<40} {}", count, truncate_str(host, 40), bar);
    }
}

fn print_method_histogram(flows: &[&Flow]) {
    let mut methods: HashMap<&str, usize> = HashMap::new();
    for f in flows {
        *methods.entry(method_from_flow(f)).or_default() += 1;
    }
    let mut sorted: Vec<_> = methods.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    println!("── Method Histogram ──");
    let max_count = sorted.first().map(|(_, c)| *c).unwrap_or(1);
    for (method, count) in sorted {
        let bar = histogram_bar(count, max_count, 30);
        println!("  {:>5}  {:<8} {}", count, method, bar);
    }
}

fn print_status_histogram(flows: &[&Flow]) {
    let mut categories: HashMap<&str, usize> = HashMap::new();
    for f in flows {
        let cat = status_category(status_from_flow(f));
        *categories.entry(cat).or_default() += 1;
    }

    let order = [
        "2xx Success",
        "3xx Redirect",
        "4xx Client Error",
        "5xx Server Error",
        "1xx Informational",
        "No Response",
        "Unknown",
    ];

    println!("── Status Code Distribution ──");
    let max_count = categories.values().max().copied().unwrap_or(1);
    for cat in &order {
        if let Some(&count) = categories.get(cat) {
            let bar = histogram_bar(count, max_count, 30);
            println!("  {:>5}  {:<22} {}", count, cat, bar);
        }
    }
}

fn print_slow_requests(flows: &[&Flow], top_n: usize) {
    let mut with_dur: Vec<(&&Flow, u64)> = flows
        .iter()
        .filter_map(|f| duration_ms(f).map(|d| (f, d)))
        .collect();
    with_dur.sort_by_key(|b| std::cmp::Reverse(b.1));

    println!("── Top {} Slow Requests ──", top_n.min(with_dur.len()));
    println!(
        "  {:<7} {:<6} {:<6} {:<50}",
        "Duration", "Method", "Status", "URL"
    );
    println!(
        "  {:<7} {:<6} {:<6} {:<50}",
        "───────", "──────", "──────", "──────────────────────────────────────────────────"
    );
    for (flow, dur) in with_dur.iter().take(top_n) {
        let method = method_from_flow(flow);
        let status = status_from_flow(flow);
        let url = if let Layer::Http(h) = &flow.layer {
            truncate_str(h.request.url.as_str(), 50)
        } else {
            String::new()
        };
        println!(
            "  {:<7} {:<6} {:<6} {}",
            format_duration(*dur),
            method,
            status,
            url
        );
    }
}

fn print_error_clustering(flows: &[&Flow]) {
    let errors: Vec<(&&Flow, String)> = flows
        .iter()
        .filter_map(|f| error_message(f).map(|e| (f, e)))
        .collect();

    let mut clusters: HashMap<String, (usize, String)> = HashMap::new();
    for (flow, err) in &errors {
        let key = cluster_key(flow, err);
        let entry = clusters
            .entry(key.clone())
            .or_insert_with(|| (0, err.clone()));
        entry.0 += 1;
    }

    let mut sorted: Vec<_> = clusters.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1.0));

    println!("── Error Clustering ──");
    if sorted.is_empty() {
        println!("  No errors found.");
    } else {
        for (key, (count, _example)) in sorted.iter().take(20) {
            println!("  {:>5}x  {}", count, key);
        }
    }
}

fn cluster_key(flow: &Flow, err: &str) -> String {
    let status = status_from_flow(flow);
    let host = host_from_flow(flow).unwrap_or_default();
    if status >= 400 {
        format!("{} on {}", err, host)
    } else if status == 0 {
        format!("No response from {}", host)
    } else if !host.is_empty() {
        format!("{} from {}", err, host)
    } else {
        err.to_string()
    }
}

// ── JSON output ────────────────────────────────────────────────

fn print_json(
    flows: &[&Flow],
    total: usize,
    error_count: usize,
    avg_duration: Option<u64>,
    total_body: u64,
    top_n: usize,
) -> Result<()> {
    let mut hosts: HashMap<String, usize> = HashMap::new();
    let mut methods: HashMap<String, usize> = HashMap::new();
    let mut statuses: HashMap<String, usize> = HashMap::new();

    for f in flows {
        if let Some(host) = host_from_flow(f) {
            *hosts.entry(host).or_default() += 1;
        }
        *methods.entry(method_from_flow(f).to_string()).or_default() += 1;
        *statuses
            .entry(status_category(status_from_flow(f)).to_string())
            .or_default() += 1;
    }

    let mut host_sorted: Vec<_> = hosts.into_iter().collect();
    host_sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut method_sorted: Vec<_> = methods.into_iter().collect();
    method_sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut status_sorted: Vec<_> = statuses.into_iter().collect();
    status_sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut with_dur: Vec<(&&Flow, u64)> = flows
        .iter()
        .filter_map(|f| duration_ms(f).map(|d| (f, d)))
        .collect();
    with_dur.sort_by_key(|b| std::cmp::Reverse(b.1));
    let slow: Vec<serde_json::Value> = with_dur
        .iter()
        .take(top_n)
        .map(|(f, dur)| {
            serde_json::json!({
                "method": method_from_flow(f),
                "url": f.layer_json_url(),
                "status": status_from_flow(f),
                "duration_ms": dur,
            })
        })
        .collect();

    let mut error_clusters: HashMap<String, usize> = HashMap::new();
    for f in flows {
        if let Some(err) = error_message(f) {
            let key = cluster_key(f, &err);
            *error_clusters.entry(key).or_default() += 1;
        }
    }
    let mut errors_sorted: Vec<_> = error_clusters.into_iter().collect();
    errors_sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

    let output = serde_json::json!({
        "summary": {
            "total_flows": total,
            "error_flows": error_count,
            "error_rate_pct": if total > 0 { error_count as f64 / total as f64 * 100.0 } else { 0.0 },
            "avg_duration_ms": avg_duration,
            "total_response_body_bytes": total_body,
        },
        "host_histogram": host_sorted.into_iter().map(|(h, c)| serde_json::json!({"host": h, "count": c})).collect::<Vec<_>>(),
        "method_histogram": method_sorted.into_iter().map(|(m, c)| serde_json::json!({"method": m, "count": c})).collect::<Vec<_>>(),
        "status_histogram": status_sorted.into_iter().map(|(s, c)| serde_json::json!({"category": s, "count": c})).collect::<Vec<_>>(),
        "slow_requests": slow,
        "error_clusters": errors_sorted.into_iter().map(|(k, c)| serde_json::json!({"cluster": k, "count": c})).collect::<Vec<_>>(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────

fn histogram_bar(count: usize, max: usize, width: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let filled = (count * width / max).min(width);
    "█".repeat(filled)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}", size, UNITS[unit_idx])
}

fn format_duration(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}ms", ms)
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

trait FlowUrlExt {
    fn layer_json_url(&self) -> String;
}

impl FlowUrlExt for Flow {
    fn layer_json_url(&self) -> String {
        if let Layer::Http(h) = &self.layer {
            h.request.url.as_str().to_string()
        } else {
            String::new()
        }
    }
}
