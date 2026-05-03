use tokio::sync::{mpsc, oneshot};
use relay_core_lib::rule::{Rule, RuleGroup};
use relay_core_lib::rule::engine::RuleEngine;
use relay_core_lib::rule::engine::state::{RuleStateStore, InMemoryRuleStateStore};
use relay_core_storage::store::Store;
use std::sync::Arc;

#[derive(Debug)]
pub enum RuleStoreMessage {
    SetRules(Vec<Rule>),
    SetRuleGroups(Vec<RuleGroup>),
    GetRules(oneshot::Sender<Vec<Rule>>),
    GetRuleGroups(oneshot::Sender<Vec<RuleGroup>>),
    GetRuleEngine(oneshot::Sender<Arc<RuleEngine>>),
    ReportExecError,
    GetMetrics(oneshot::Sender<usize>), // exec_errors
}

pub struct RuleStoreActor {
    rules: Vec<Rule>,
    rule_groups: Vec<RuleGroup>,
    rule_engine: Arc<RuleEngine>,
    state_store: Arc<dyn RuleStateStore>,
    receiver: mpsc::Receiver<RuleStoreMessage>,
    store: Option<Store>,
    exec_errors: usize,
}

impl RuleStoreActor {
    pub fn new(receiver: mpsc::Receiver<RuleStoreMessage>, store: Option<Store>) -> Self {
        let state_store = Arc::new(InMemoryRuleStateStore::new());
        Self {
            rules: Vec::new(),
            rule_groups: Vec::new(),
            rule_engine: Arc::new(RuleEngine::new(Vec::new(), Vec::new(), None, Some(state_store.clone()))),
            state_store,
            receiver,
            store,
            exec_errors: 0,
        }
    }

    pub async fn run(mut self) {
        // Load rules from store if available
        if let Some(store) = &self.store {
            if let Ok(loaded_rules) = store.load_rules().await {
                let mut rules = Vec::new();
                for (_, content) in loaded_rules {
                    if let Ok(rule) = serde_json::from_value::<Rule>(content) {
                        rules.push(rule);
                    }
                }
                if !rules.is_empty() {
                    self.rules = rules;
                    self.rule_engine = Arc::new(RuleEngine::new(self.rules.clone(), self.rule_groups.clone(), None, Some(self.state_store.clone())));
                    tracing::info!("Loaded {} rules from storage", self.rules.len());
                }
            }
        }

        while let Some(msg) = self.receiver.recv().await {
            match msg {
                RuleStoreMessage::SetRules(rules) => {
                    self.rules = rules;
                    self.rule_engine = Arc::new(RuleEngine::new(self.rules.clone(), self.rule_groups.clone(), None, Some(self.state_store.clone())));
                    
                    if let Some(store) = &self.store {
                        let rules_to_save: Vec<(String, serde_json::Value)> = self.rules.iter()
                            .filter_map(|r| serde_json::to_value(r).ok().map(|v| (r.id.clone(), v)))
                            .collect();
                        if let Err(e) = store.replace_rules(&rules_to_save).await {
                            tracing::error!("Failed to save rules: {}", e);
                        }
                    }
                },
                RuleStoreMessage::SetRuleGroups(groups) => {
                    self.rule_groups = groups;
                    self.rule_engine = Arc::new(RuleEngine::new(self.rules.clone(), self.rule_groups.clone(), None, Some(self.state_store.clone())));
                },
                RuleStoreMessage::GetRules(respond_to) => {
                    let _ = respond_to.send(self.rules.clone());
                },
                RuleStoreMessage::GetRuleGroups(respond_to) => {
                    let _ = respond_to.send(self.rule_groups.clone());
                },
                RuleStoreMessage::GetRuleEngine(respond_to) => {
                    let _ = respond_to.send(self.rule_engine.clone());
                },
                RuleStoreMessage::ReportExecError => {
                    self.exec_errors += 1;
                },
                RuleStoreMessage::GetMetrics(respond_to) => {
                    let _ = respond_to.send(self.exec_errors);
                },
            }
        }
    }
}
