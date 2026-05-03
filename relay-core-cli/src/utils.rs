use std::path::PathBuf;
use anyhow::Result;
use relay_core_runtime::rule::InterceptRule;
use relay_core_api::flow::Flow;

pub fn load_rules(path: &PathBuf) -> Result<Vec<InterceptRule>> {
    let content = std::fs::read_to_string(path)?;
    let rules: Vec<InterceptRule> = if path.extension().is_some_and(|ext| ext == "yaml" || ext == "yml") {
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
