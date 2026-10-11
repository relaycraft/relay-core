use crate::args::RulesAction;
use crate::utils::{load_flow, load_rules};
use anyhow::{Context, Result};

use super::daemon_client::resolve_daemon;

pub async fn execute(action: RulesAction) -> Result<()> {
    match action {
        RulesAction::Validate { file } => match load_rules(&file) {
            Ok(rules) => println!("Valid rule set: {} rules found.", rules.len()),
            Err(e) => {
                eprintln!("Invalid rules file: {}", e);
                std::process::exit(1);
            }
        },
        RulesAction::Print { file, format } => match load_rules(&file) {
            Ok(rules) => {
                let output = if format == "json" {
                    serde_json::to_string_pretty(&rules)
                        .context("Failed to serialize rules as JSON")?
                } else {
                    serde_yaml::to_string(&rules).context("Failed to serialize rules as YAML")?
                };
                println!("{}", output);
            }
            Err(e) => {
                eprintln!("Failed to load rules: {}", e);
                std::process::exit(1);
            }
        },
        RulesAction::Test { file, flow } => {
            let rules = load_rules(&file)?;
            let flow_data = load_flow(&flow)?;

            println!(
                "Testing {} rules against flow {}...",
                rules.len(),
                flow_data.id
            );
            let mut matched = 0;
            for rule in rules {
                if rule.matches(&flow_data, "request") {
                    println!("✓ Rule '{}' matches request", rule.id);
                    matched += 1;
                } else if rule.matches(&flow_data, "response") {
                    println!("✓ Rule '{}' matches response", rule.id);
                    matched += 1;
                }
            }

            if matched == 0 {
                println!("No rules matched.");
                std::process::exit(1);
            }
        }
        RulesAction::List { api_url } => {
            // Two defects hid behind one default. The URL defaulted to `127.0.0.1:18082`, a port no
            // daemon binds (the control API defaults to 8082), and the request carried no
            // credential, which a token-protected daemon rejects. Either one alone made this
            // command fail against every live engine. Resolve the daemon and go through the shared
            // control client instead, as `flows` already did.
            let daemon = resolve_daemon(api_url.as_deref()).await?;
            let items = daemon.client("cli:relay rules list").rules().await?;
            if items.is_empty() {
                println!("No rules loaded.");
            } else {
                println!("{} active rules:", items.len());
                for (i, rule) in items.iter().enumerate() {
                    let id = rule.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = rule.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let active = rule
                        .get("active")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let stage = rule.get("stage").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = if active { "✓" } else { "✗" };
                    println!("  {}. [{}] {} ({} {})", i + 1, status, id, stage, name);
                }
            }
        }
    }
    Ok(())
}
