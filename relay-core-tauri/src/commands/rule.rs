use tauri::State;
use crate::RelayCoreState;
use crate::interceptor::InterceptRule;
use relay_core_runtime::audit::AuditActor;
use serde_json::json;

#[tauri::command]
pub async fn set_intercept_rule(
    state: State<'_, RelayCoreState>,
    rule: InterceptRule
) -> Result<String, String> {
    set_intercept_rule_impl(&state, rule).await
}

pub async fn set_intercept_rule_impl(
    state: &RelayCoreState,
    rule: InterceptRule
) -> Result<String, String> {
    let rule_id = rule.id.clone();
    state
        .core
        .upsert_legacy_intercept_rule_from(
            AuditActor::Tauri,
            rule_id,
            json!({ "command": "set_intercept_rule" }),
            rule,
        )
        .await?;
    
    Ok("Rule updated".to_string())
}

#[cfg(test)]
mod tests {
    use super::set_intercept_rule_impl;
    use crate::interceptor::InterceptRule;
    use crate::RelayCoreState;
    use relay_core_lib::rule::{Action, Filter, Rule, RuleStage, RuleTermination, StringMatcher};

    #[tokio::test]
    async fn test_set_intercept_rule_impl_replaces_same_legacy_rule_family() {
        let state = RelayCoreState::new_async().await;

        let initial = InterceptRule {
            id: "legacy-1".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "both".to_string(),
        };
        let res = set_intercept_rule_impl(&state, initial).await;
        assert!(res.is_ok());

        let mut rules = state.ctx.rules.get_rules().await;
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, "legacy-1-0");
        assert_eq!(rules[1].id, "legacy-1-1");

        let updated = InterceptRule {
            id: "legacy-1".to_string(),
            active: true,
            url_pattern: "example.org".to_string(),
            method: Some("POST".to_string()),
            phase: "request".to_string(),
        };
        let res = set_intercept_rule_impl(&state, updated).await;
        assert!(res.is_ok());

        let rules = state.ctx.rules.get_rules().await;
        assert_eq!(rules.len(), 1, "old derived rules should be replaced");
        assert_eq!(rules[0].id, "legacy-1");
        assert_eq!(rules[0].stage, RuleStage::RequestHeaders);
        match &rules[0].filter {
            Filter::And(filters) => {
                assert_eq!(filters.len(), 2);
            }
            other => panic!("expected And filter, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_intercept_rule_impl_keeps_unrelated_rules() {
        let state = RelayCoreState::new_async().await;
        let unrelated = Rule {
            id: "manual-rule".to_string(),
            name: "manual".to_string(),
            active: true,
            stage: RuleStage::RequestHeaders,
            priority: 10,
            termination: RuleTermination::Continue,
            filter: Filter::Url(StringMatcher::Contains("manual".to_string())),
            actions: vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
            constraints: None,
        };
        state.core.set_rules(vec![unrelated]).await;

        let legacy = InterceptRule {
            id: "legacy-keep".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "request".to_string(),
        };
        let res = set_intercept_rule_impl(&state, legacy).await;
        assert!(res.is_ok());

        let mut ids: Vec<String> = state.ctx.rules.get_rules().await.into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["legacy-keep".to_string(), "manual-rule".to_string()]);
    }

    #[tokio::test]
    async fn test_set_intercept_rule_impl_inactive_clears_existing_family() {
        let state = RelayCoreState::new_async().await;

        let active = InterceptRule {
            id: "legacy-clear".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "both".to_string(),
        };
        set_intercept_rule_impl(&state, active)
            .await
            .expect("seed active rule");
        assert_eq!(state.ctx.rules.get_rules().await.len(), 2);

        let inactive = InterceptRule {
            id: "legacy-clear".to_string(),
            active: false,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "both".to_string(),
        };
        let res = set_intercept_rule_impl(&state, inactive).await;
        assert!(res.is_ok());
        assert!(
            state.ctx.rules.get_rules().await.is_empty(),
            "inactive update should remove existing family and add nothing"
        );
    }

    #[tokio::test]
    async fn test_set_intercept_rule_impl_invalid_phase_removes_family_without_readding() {
        let state = RelayCoreState::new_async().await;

        let active = InterceptRule {
            id: "legacy-invalid-phase".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "request".to_string(),
        };
        set_intercept_rule_impl(&state, active)
            .await
            .expect("seed active rule");
        assert_eq!(state.ctx.rules.get_rules().await.len(), 1);

        let invalid_phase = InterceptRule {
            id: "legacy-invalid-phase".to_string(),
            active: true,
            url_pattern: "example.com".to_string(),
            method: None,
            phase: "invalid".to_string(),
        };
        let res = set_intercept_rule_impl(&state, invalid_phase).await;
        assert!(res.is_ok());
        assert!(
            state.ctx.rules.get_rules().await.is_empty(),
            "invalid phase yields empty to_rules and should leave no family rules"
        );
    }
}
