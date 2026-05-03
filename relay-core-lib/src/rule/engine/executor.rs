use crate::rule::model::{Rule, RuleGroup, RuleStage, RuleOutcome, RuleExecutionEvent, RuleTermination, RuleTraceSummary};
use crate::rule::engine::compiler;
use crate::rule::engine::compiled::CompiledRule;
use crate::rule::engine::matcher;
use crate::rule::engine::validator;
use crate::rule::engine::actions;
use crate::rule::engine::state::{RuleStateStore, InMemoryRuleStateStore};
use relay_core_api::flow::Flow;
use std::collections::HashMap;
use std::sync::Arc;
use relay_core_api::policy::ProxyPolicy;

pub struct ExecutionContext {
    pub trace: Vec<RuleExecutionEvent>,
    pub variables: HashMap<String, String>,
    pub policy: Option<Arc<ProxyPolicy>>,
    pub summary: RuleTraceSummary,
    pub state_store: Arc<dyn RuleStateStore>,
}

#[derive(Debug)]
pub struct RuleEngine {
    compiled_rules: Vec<CompiledRule>,
    policy: Option<Arc<ProxyPolicy>>,
    state_store: Arc<dyn RuleStateStore>,
}

impl RuleEngine {
    pub fn new(rules: Vec<Rule>, rule_groups: Vec<RuleGroup>, policy: Option<Arc<ProxyPolicy>>, state_store: Option<Arc<dyn RuleStateStore>>) -> Self {
        let mut all_rules = Vec::new();
        
        // Flatten rules
        for rule in rules {
            all_rules.push(rule);
        }
        
        // Flatten rule groups
        for group in rule_groups {
            if group.active {
                for rule in group.rules {
                    all_rules.push(rule);
                }
            }
        }
        
        // Sort by priority (descending)
        all_rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        
        // Compile all rules
        let compiled_rules = all_rules.into_iter()
            .map(compiler::compile_rule)
            .collect();
            
        Self { 
            compiled_rules, 
            policy,
            state_store: state_store.unwrap_or_else(|| Arc::new(InMemoryRuleStateStore::new()))
        }
    }

    pub fn has_rules_for_stage(&self, stage: RuleStage) -> bool {
        self.compiled_rules.iter().any(|r| r.original.active && r.original.stage == stage)
    }

    pub async fn execute(&self, stage: RuleStage, flow: &mut Flow) -> ExecutionContext {
        let mut ctx = ExecutionContext { 
            trace: vec![],
            variables: HashMap::new(),
            policy: self.policy.clone(),
            summary: RuleTraceSummary::NoMatch,
            state_store: self.state_store.clone(),
        };
        
        let mut terminated = false;
        let mut modified_rules = Vec::new();

        for compiled_rule in &self.compiled_rules {
            let rule = &compiled_rule.original;

            if !rule.active {
                continue;
            }

            if rule.stage != stage {
                continue;
            }
            
            // Validate filter stage (using pre-compiled filter)
            if !validator::validate_filter_stage(&compiled_rule.filter, &stage) {
                 ctx.trace.push(RuleExecutionEvent {
                    rule_id: rule.id.clone(),
                    stage: stage.clone(),
                    matched: false,
                    duration_us: 0,
                    outcome: RuleOutcome::Failed(format!("Filter invalid for stage {:?}", stage)),
                });
                continue;
            }
            
            // Validate action stage
            let mut actions_valid = true;
            for action in &rule.actions {
                if !validator::validate_action_stage(action, &stage) {
                     ctx.trace.push(RuleExecutionEvent {
                        rule_id: rule.id.clone(),
                        stage: stage.clone(),
                        matched: false,
                        duration_us: 0,
                        outcome: RuleOutcome::Failed(format!("Action {:?} not allowed in stage {:?}", action, stage)),
                    });
                    actions_valid = false;
                    break;
                }
            }
            if !actions_valid {
                continue;
            }

            // Match (using pre-compiled filter)
            let start = std::time::Instant::now();
            let matched = matcher::matches(&compiled_rule.filter, flow);
            
            if matched {
                 let timeout_ms = rule.constraints.as_ref().and_then(|c| c.timeout_ms);
                 
                 let action_execution = async {
                     let mut rule_outcome = RuleOutcome::MatchedAndExecuted;
                     let mut rule_terminated = false;
                     
                     for action in &rule.actions {
                        match actions::execute_action(action, flow, &mut ctx).await {
                            actions::ActionOutcome::Continue => {},
                            actions::ActionOutcome::Terminated(reason) => {
                                 rule_outcome = RuleOutcome::MatchedAndTerminated;
                                 ctx.summary = RuleTraceSummary::Terminated { 
                                     rule_id: rule.id.clone(), 
                                     reason 
                                 };
                                 rule_terminated = true;
                                 break; 
                            },
                            actions::ActionOutcome::Failed(err) => {
                                 rule_outcome = RuleOutcome::Failed(err);
                                 break;
                            }
                        }
                     }
                     (rule_outcome, rule_terminated)
                 };

                 let (rule_outcome, rule_terminated) = if let Some(ms) = timeout_ms {
                     match tokio::time::timeout(std::time::Duration::from_millis(ms), action_execution).await {
                         Ok(res) => res,
                         Err(_) => (RuleOutcome::Failed(format!("Rule execution timed out after {}ms", ms)), false)
                     }
                 } else {
                     action_execution.await
                 };
                 
                 if rule_terminated {
                     terminated = true;
                 }
                 
                 ctx.trace.push(RuleExecutionEvent {
                    rule_id: rule.id.clone(),
                    stage: stage.clone(),
                    matched: true,
                    duration_us: start.elapsed().as_micros() as u64,
                    outcome: rule_outcome.clone(),
                });
                
                if matches!(rule_outcome, RuleOutcome::MatchedAndExecuted) {
                     modified_rules.push(rule.id.clone());
                }

                if terminated {
                    break;
                }
                
                if let RuleTermination::Stop = rule.termination {
                    break;
                }
            }
        }
        
        if !terminated && !modified_rules.is_empty() {
            ctx.summary = RuleTraceSummary::Modified { rule_ids: modified_rules };
        }
        
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::model::{Action, Filter, Rule, RuleStage, RuleTermination, RuleOutcome, StringMatcher, BodySource, RuleConstraints};
    use relay_core_api::flow::{Flow, Layer, NetworkInfo, TransportProtocol, HttpRequest};
    use uuid::Uuid;
    use chrono::Utc;
    use url::Url;

    fn create_test_flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12345,
                server_ip: "1.1.1.1".to_string(),
                server_port: 80,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(relay_core_api::flow::HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("http://example.com").unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_rule_timeout() {
        let rule = Rule {
            id: "test-rule-timeout".to_string(),
            name: "Test Rule Timeout".to_string(),
            active: true,
            stage: RuleStage::RequestHeaders,
            priority: 0,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![
                Action::Delay { ms: 200 } // Delay 200ms
            ],
            constraints: Some(RuleConstraints {
                timeout_ms: Some(50), // Timeout 50ms
            }),
        };

        let engine = RuleEngine::new(vec![rule], vec![], None, None);
        let mut flow = create_test_flow();
        
        // Execute in RequestHeaders stage
        let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
        
        // Expect 1 trace entry with Failed outcome due to timeout
        assert_eq!(ctx.trace.len(), 1);
        if let RuleOutcome::Failed(msg) = &ctx.trace[0].outcome {
            assert!(msg.contains("timed out"));
        } else {
            panic!("Expected Failed outcome, got {:?}", ctx.trace[0].outcome);
        }
    }

    #[tokio::test]
    async fn test_filter_stage_validation_failure() {
        let rule = Rule {
            id: "test-rule-1".to_string(),
            name: "Test Rule 1".to_string(),
            active: true,
            stage: RuleStage::Connect, // Stage is Connect
            priority: 0,
            termination: RuleTermination::Continue,
            filter: Filter::Path(StringMatcher::Exact("/foo".to_string())), // Path filter invalid in Connect
            actions: vec![],
            constraints: None,
        };

        let engine = RuleEngine::new(vec![rule], vec![], None, None);
        let mut flow = create_test_flow();
        
        // Execute in Connect stage
        let ctx = engine.execute(RuleStage::Connect, &mut flow).await;
        
        // Expect 1 trace entry with Failed outcome
        assert_eq!(ctx.trace.len(), 1);
        if let RuleOutcome::Failed(msg) = &ctx.trace[0].outcome {
            assert!(msg.contains("Filter invalid"));
        } else {
            assert!(false, "Expected Failed outcome, got {:?}", ctx.trace[0].outcome);
        }
    }

    #[tokio::test]
    async fn test_action_stage_validation_failure() {
        let rule = Rule {
            id: "test-rule-2".to_string(),
            name: "Test Rule 2".to_string(),
            active: true,
            stage: RuleStage::Connect, // Stage is Connect
            priority: 0,
            termination: RuleTermination::Continue,
            filter: Filter::All, // Valid filter
            actions: vec![
                Action::SetResponseBody { body: BodySource::Text("foo".to_string()) } // Invalid action in Connect
            ],
            constraints: None,
        };

        let engine = RuleEngine::new(vec![rule], vec![], None, None);
        let mut flow = create_test_flow();
        
        // Execute in Connect stage
        let ctx = engine.execute(RuleStage::Connect, &mut flow).await;
        
        // Expect 1 trace entry with Failed outcome
        assert_eq!(ctx.trace.len(), 1);
        if let RuleOutcome::Failed(msg) = &ctx.trace[0].outcome {
            assert!(msg.contains("Action"));
            assert!(msg.contains("not allowed"));
        } else {
            assert!(false, "Expected Failed outcome, got {:?}", ctx.trace[0].outcome);
        }
    }

    #[test]
    fn test_has_rules_for_stage_respects_active_flag() {
        let inactive_rule = Rule {
            id: "inactive-rh".to_string(),
            name: "Inactive".to_string(),
            active: false,
            stage: RuleStage::RequestHeaders,
            priority: 10,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
            constraints: None,
        };
        let active_connect_rule = Rule {
            id: "active-connect".to_string(),
            name: "Active Connect".to_string(),
            active: true,
            stage: RuleStage::Connect,
            priority: 10,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![Action::Drop],
            constraints: None,
        };

        let engine = RuleEngine::new(vec![inactive_rule, active_connect_rule], vec![], None, None);
        assert!(
            !engine.has_rules_for_stage(RuleStage::RequestHeaders),
            "inactive rules should not count for stage presence"
        );
        assert!(engine.has_rules_for_stage(RuleStage::Connect));
    }

    #[tokio::test]
    async fn test_execute_orders_rules_by_priority_descending() {
        let low = Rule {
            id: "low-pri".to_string(),
            name: "low".to_string(),
            active: true,
            stage: RuleStage::RequestHeaders,
            priority: 1,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![Action::Tag {
                key: "order".to_string(),
                value: "low".to_string(),
            }],
            constraints: None,
        };
        let high = Rule {
            id: "high-pri".to_string(),
            name: "high".to_string(),
            active: true,
            stage: RuleStage::RequestHeaders,
            priority: 100,
            termination: RuleTermination::Continue,
            filter: Filter::All,
            actions: vec![Action::Tag {
                key: "order".to_string(),
                value: "high".to_string(),
            }],
            constraints: None,
        };
        let engine = RuleEngine::new(vec![low, high], vec![], None, None);
        let mut flow = create_test_flow();

        let ctx = engine.execute(RuleStage::RequestHeaders, &mut flow).await;
        assert_eq!(ctx.trace.len(), 2);
        assert_eq!(ctx.trace[0].rule_id, "high-pri");
        assert_eq!(ctx.trace[1].rule_id, "low-pri");
        assert_eq!(flow.tags, vec!["order:high".to_string(), "order:low".to_string()]);
    }
}
