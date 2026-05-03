use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value;
use relay_core_lib::rule::Rule;
use relay_core_lib::rule::engine::RuleEngine;
use crate::audit::AuditActor;
use crate::rule::{InterceptRuleConfig, MockResponseRuleConfig};
use crate::CoreState;

#[async_trait]
pub trait RuleService: Send + Sync {
    async fn get_rules(&self) -> Vec<Rule>;
    async fn get_rule_engine(&self) -> Arc<RuleEngine>;
    async fn upsert_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: Value,
        rule: Rule,
    ) -> Result<(), String>;
    async fn delete_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: Value,
        rule_id: &str,
    ) -> Result<bool, String>;
    async fn create_mock_response_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: Value,
        config: MockResponseRuleConfig,
    ) -> Result<String, String>;
    async fn create_intercept_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: Value,
        config: InterceptRuleConfig,
    ) -> Result<String, String>;
}

#[async_trait]
impl RuleService for CoreState {
    async fn get_rules(&self) -> Vec<Rule> {
        CoreState::get_rules(self).await
    }

    async fn get_rule_engine(&self) -> Arc<RuleEngine> {
        CoreState::get_rule_engine(self).await
    }

    async fn upsert_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: Value,
        rule: Rule,
    ) -> Result<(), String> {
        CoreState::upsert_rule_from(self, actor, operation, target, details, rule).await
    }

    async fn delete_rule_from(
        &self,
        actor: AuditActor,
        operation: &str,
        target: String,
        details: Value,
        rule_id: &str,
    ) -> Result<bool, String> {
        CoreState::delete_rule_from(self, actor, operation, target, details, rule_id).await
    }

    async fn create_mock_response_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: Value,
        config: MockResponseRuleConfig,
    ) -> Result<String, String> {
        CoreState::create_mock_response_rule_from(self, actor, target, details, config).await
    }

    async fn create_intercept_rule_from(
        &self,
        actor: AuditActor,
        target: String,
        details: Value,
        config: InterceptRuleConfig,
    ) -> Result<String, String> {
        CoreState::create_intercept_rule_from(self, actor, target, details, config).await
    }
}
