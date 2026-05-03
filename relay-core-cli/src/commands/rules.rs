use crate::args::RulesAction;
use crate::utils::{load_rules, load_flow};
use anyhow::Result;

pub fn execute(action: RulesAction) -> Result<()> {
    match action {
        RulesAction::Validate { file } => {
            match load_rules(&file) {
                Ok(rules) => println!("Valid rule set: {} rules found.", rules.len()),
                Err(e) => {
                    eprintln!("Invalid rules file: {}", e);
                    std::process::exit(1);
                }
            }
        },
        RulesAction::Print { file, format } => {
            match load_rules(&file) {
                Ok(rules) => {
                    let output = if format == "json" {
                        serde_json::to_string_pretty(&rules).unwrap()
                    } else {
                        serde_yaml::to_string(&rules).unwrap()
                    };
                    println!("{}", output);
                },
                Err(e) => {
                    eprintln!("Failed to load rules: {}", e);
                    std::process::exit(1);
                }
            }
        },
        RulesAction::Test { file, flow } => {
            let rules = load_rules(&file)?;
            let flow_data = load_flow(&flow)?;
            
            println!("Testing {} rules against flow {}...", rules.len(), flow_data.id);
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
    }
    Ok(())
}
