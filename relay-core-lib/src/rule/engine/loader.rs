//! Load-time rule validation (roadmap §24.6).
//!
//! Compilation used to be infallible: an invalid regex, glob or CIDR was turned into an `Invalid`
//! sentinel that **never matches**, so a rule with a typo was accepted, stored, reported as enabled,
//! and silently did nothing — the single hardest class of bug to diagnose from the outside.
//! Stage mismatches had the same shape: only caught at execution time, reported into a trace nobody
//! reads.
//!
//! This module answers "is this rule usable?" *before* it is accepted, so an adapter can reject it
//! with a specific reason instead of storing a rule that cannot work.

use crate::rule::model::{Action, Filter, Rule, RuleStage, StringMatcher};
use std::fmt;

/// Why a rule cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleValidationError {
    /// A pattern in the filter failed to compile.
    InvalidPattern {
        /// Where in the filter the pattern lives, for a precise message.
        field: String,
        /// The offending pattern.
        pattern: String,
        /// The underlying parser error.
        detail: String,
    },
    /// A filter is not available at the rule's stage.
    FilterNotAvailableAtStage {
        /// The stage the rule targets.
        stage: RuleStage,
        /// The filter that cannot run there.
        filter: String,
    },
    /// An action is not allowed at the rule's stage.
    ActionNotAvailableAtStage {
        /// The stage the rule targets.
        stage: RuleStage,
        /// The action that cannot run there.
        action: String,
    },
    /// A rule has no actions, so it can never have an effect.
    ///
    /// This is a **warning**, not a rejection: a rule can legitimately be staged before its actions
    /// are configured, and blocking it would break that workflow. It is still worth surfacing, since
    /// an actionless rule silently doing nothing is otherwise indistinguishable from a working one.
    NoActions,
}

impl RuleValidationError {
    /// Does this problem make the rule unusable, rather than merely pointless?
    ///
    /// Pattern and stage problems block acceptance because the rule cannot be evaluated as written.
    /// A missing action does not: the rule is well-formed, it simply does nothing yet.
    pub const fn is_blocking(&self) -> bool {
        !matches!(self, RuleValidationError::NoActions)
    }
}

/// The outcome of validating one rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleValidationReport {
    /// Problems that make the rule unusable.
    pub errors: Vec<RuleValidationError>,
    /// Problems that do not block acceptance but should be surfaced to the author.
    pub warnings: Vec<RuleValidationError>,
}

impl RuleValidationReport {
    /// Is the rule usable?
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Every problem, blocking or not.
    pub fn all(&self) -> impl Iterator<Item = &RuleValidationError> {
        self.errors.iter().chain(self.warnings.iter())
    }
}

impl fmt::Display for RuleValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleValidationError::InvalidPattern {
                field,
                pattern,
                detail,
            } => write!(f, "invalid pattern in {field}: '{pattern}' ({detail})"),
            RuleValidationError::FilterNotAvailableAtStage { stage, filter } => {
                write!(f, "filter {filter} is not available at stage {stage:?}")
            }
            RuleValidationError::ActionNotAvailableAtStage { stage, action } => {
                write!(f, "action {action} is not allowed at stage {stage:?}")
            }
            RuleValidationError::NoActions => {
                write!(f, "rule has no actions, so it can never have an effect")
            }
        }
    }
}

impl std::error::Error for RuleValidationError {}

/// Validate a rule as loaded, before it is accepted.
///
/// Returns every problem rather than only the first, so an adapter can report a complete list in one
/// round trip instead of making the user fix errors one at a time.
pub fn validate_rule(rule: &Rule) -> RuleValidationReport {
    let mut report = RuleValidationReport::default();
    let mut problems = Vec::new();

    validate_filter_patterns(&rule.filter, "filter", &mut problems);
    validate_filter_stage(&rule.filter, &rule.stage, &mut problems);

    if rule.actions.is_empty() {
        problems.push(RuleValidationError::NoActions);
    }
    for action in &rule.actions {
        if !super::validator::validate_action_stage(action, &rule.stage) {
            problems.push(RuleValidationError::ActionNotAvailableAtStage {
                stage: rule.stage.clone(),
                action: action_name(action).to_string(),
            });
        }
    }

    for problem in problems {
        if problem.is_blocking() {
            report.errors.push(problem);
        } else {
            report.warnings.push(problem);
        }
    }
    report
}

/// Validate every rule in a list.
///
/// An inactive rule is still validated: it becomes active later, and a broken rule that "works" until
/// someone enables it is worse than one that never loads.
pub fn validate_rules<'a, I>(rules: I) -> RuleValidationReport
where
    I: IntoIterator<Item = &'a Rule>,
{
    let mut report = RuleValidationReport::default();
    for rule in rules {
        let one = validate_rule(rule);
        report.errors.extend(one.errors);
        report.warnings.extend(one.warnings);
    }
    report
}

/// Validate every rule, pairing each problem with the id of the rule it came from.
pub fn validate_rules_with_ids<'a, I>(rules: I) -> Vec<(String, RuleValidationError)>
where
    I: IntoIterator<Item = &'a Rule>,
{
    let mut out = Vec::new();
    for rule in rules {
        for problem in validate_rule(rule).all() {
            out.push((rule.id.clone(), problem.clone()));
        }
    }
    out
}

fn validate_filter_patterns(filter: &Filter, path: &str, errors: &mut Vec<RuleValidationError>) {
    match filter {
        Filter::All
        | Filter::DstPort(_)
        | Filter::Protocol(_)
        | Filter::TransparentMode(_)
        | Filter::StatusCode(_) => {}

        Filter::SrcIp(cidr) => {
            if let Err(e) = cidr.parse::<ipnetwork::IpNetwork>() {
                errors.push(RuleValidationError::InvalidPattern {
                    field: format!("{path}.src_ip"),
                    pattern: cidr.clone(),
                    detail: e.to_string(),
                });
            }
        }

        Filter::Url(m) => validate_matcher(m, &format!("{path}.url"), errors),
        Filter::Host(m) => validate_matcher(m, &format!("{path}.host"), errors),
        Filter::Path(m) => validate_matcher(m, &format!("{path}.path"), errors),
        Filter::Method(m) => validate_matcher(m, &format!("{path}.method"), errors),
        Filter::ResponseBody(m) => validate_matcher(m, &format!("{path}.response_body"), errors),
        Filter::WebSocketMessage(m) => {
            validate_matcher(m, &format!("{path}.websocket_message"), errors)
        }

        Filter::RequestHeader { value, .. } => {
            if let Some(m) = value {
                validate_matcher(m, &format!("{path}.request_header"), errors);
            }
        }
        Filter::ResponseHeader { value, .. } => {
            if let Some(m) = value {
                validate_matcher(m, &format!("{path}.response_header"), errors);
            }
        }

        Filter::And(inner) | Filter::Or(inner) => {
            for (i, f) in inner.iter().enumerate() {
                validate_filter_patterns(f, &format!("{path}[{i}]"), errors);
            }
        }
        Filter::Not(inner) => validate_filter_patterns(inner, &format!("{path}.not"), errors),
    }
}

fn validate_matcher(m: &StringMatcher, field: &str, errors: &mut Vec<RuleValidationError>) {
    match m {
        StringMatcher::Regex(pattern) => {
            if let Err(e) = regex::Regex::new(pattern) {
                errors.push(RuleValidationError::InvalidPattern {
                    field: field.to_string(),
                    pattern: pattern.clone(),
                    detail: e.to_string(),
                });
            }
        }
        StringMatcher::Glob(pattern) => {
            if let Err(e) = glob::Pattern::new(pattern) {
                errors.push(RuleValidationError::InvalidPattern {
                    field: field.to_string(),
                    pattern: pattern.clone(),
                    detail: e.to_string(),
                });
            }
        }
        StringMatcher::Exact(_)
        | StringMatcher::Contains(_)
        | StringMatcher::Prefix(_)
        | StringMatcher::Suffix(_) => {}
    }
}

fn validate_filter_stage(
    filter: &Filter,
    stage: &RuleStage,
    errors: &mut Vec<RuleValidationError>,
) {
    // Compile just enough to reuse the compiled-stage matrix without duplicating it.
    let compiled = super::compiler::compile_filter(filter);
    if !super::validator::validate_filter_stage(&compiled, stage) {
        errors.push(RuleValidationError::FilterNotAvailableAtStage {
            stage: stage.clone(),
            filter: filter_name(filter).to_string(),
        });
    }
}

fn filter_name(filter: &Filter) -> &'static str {
    match filter {
        Filter::All => "All",
        Filter::SrcIp(_) => "SrcIp",
        Filter::DstPort(_) => "DstPort",
        Filter::Protocol(_) => "Protocol",
        Filter::TransparentMode(_) => "TransparentMode",
        Filter::Url(_) => "Url",
        Filter::Host(_) => "Host",
        Filter::Path(_) => "Path",
        Filter::Method(_) => "Method",
        Filter::RequestHeader { .. } => "RequestHeader",
        Filter::ResponseHeader { .. } => "ResponseHeader",
        Filter::StatusCode(_) => "StatusCode",
        Filter::ResponseBody(_) => "ResponseBody",
        Filter::WebSocketMessage(_) => "WebSocketMessage",
        Filter::And(_) => "And",
        Filter::Or(_) => "Or",
        Filter::Not(_) => "Not",
    }
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Drop => "Drop",
        Action::Abort => "Abort",
        Action::Delay { .. } => "Delay",
        Action::Throttle { .. } => "Throttle",
        Action::Tag { .. } => "Tag",
        Action::Inspect => "Inspect",
        Action::SetVariable { .. } => "SetVariable",
        Action::RateLimit { .. } => "RateLimit",
        Action::RedirectIp { .. } => "RedirectIp",
        Action::SetTtl { .. } => "SetTtl",
        Action::ForwardPort { .. } => "ForwardPort",
        Action::MockResponse { .. } => "MockResponse",
        Action::MapLocal { .. } => "MapLocal",
        Action::MapRemote { .. } => "MapRemote",
        Action::Redirect { .. } => "Redirect",
        Action::AddRequestHeader { .. } => "AddRequestHeader",
        Action::UpdateRequestHeader { .. } => "UpdateRequestHeader",
        Action::DeleteRequestHeader { .. } => "DeleteRequestHeader",
        Action::AddResponseHeader { .. } => "AddResponseHeader",
        Action::UpdateResponseHeader { .. } => "UpdateResponseHeader",
        Action::DeleteResponseHeader { .. } => "DeleteResponseHeader",
        Action::SetRequestMethod { .. } => "SetRequestMethod",
        Action::SetRequestUrl { .. } => "SetRequestUrl",
        Action::SetRequestBody { .. } => "SetRequestBody",
        Action::SetResponseStatus { .. } => "SetResponseStatus",
        Action::SetResponseBody { .. } => "SetResponseBody",
        Action::TransformRequestBody { .. } => "TransformRequestBody",
        Action::TransformResponseBody { .. } => "TransformResponseBody",
        Action::MockWebSocketMessage { .. } => "MockWebSocketMessage",
        Action::DropWebSocketMessage => "DropWebSocketMessage",
    }
}

#[cfg(test)]
mod tests {
    use super::{RuleValidationError, validate_rule, validate_rules_with_ids};
    use crate::rule::model::{
        Action, BodySource, Filter, Rule, RuleStage, RuleTermination, StringMatcher,
    };

    fn rule(stage: RuleStage, filter: Filter, actions: Vec<Action>) -> Rule {
        Rule {
            id: "r".to_string(),
            name: "r".to_string(),
            active: true,
            stage,
            priority: 0,
            termination: RuleTermination::Continue,
            filter,
            actions,
            constraints: None,
        }
    }

    #[test]
    fn a_valid_rule_reports_no_problems() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::Host(StringMatcher::Regex("^api\\.example\\.com$".to_string())),
            vec![Action::AddRequestHeader {
                name: "x".to_string(),
                value: "1".to_string(),
            }],
        );
        let report = validate_rule(&r);
        assert!(report.is_ok(), "unexpected problems: {report:?}");
        assert!(report.warnings.is_empty());
    }

    /// The headline case: a typo in a regex used to compile to a never-matching sentinel, so the rule
    /// was accepted and silently did nothing.
    #[test]
    fn an_invalid_regex_is_rejected_with_its_location() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::Url(StringMatcher::Regex("([unclosed".to_string())),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );

        let report = validate_rule(&r);
        assert_eq!(report.errors.len(), 1, "expected one error, got {report:?}");
        match &report.errors[0] {
            RuleValidationError::InvalidPattern { field, pattern, .. } => {
                assert!(
                    field.contains("url"),
                    "field should locate the pattern: {field}"
                );
                assert_eq!(pattern, "([unclosed");
            }
            other => panic!("expected InvalidPattern, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_glob_is_rejected() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::Path(StringMatcher::Glob("[".to_string())),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        assert!(
            validate_rule(&r)
                .errors
                .iter()
                .any(|e| matches!(e, RuleValidationError::InvalidPattern { .. })),
            "an unparseable glob must be rejected"
        );
    }

    #[test]
    fn an_invalid_cidr_is_rejected() {
        let r = rule(
            RuleStage::Connect,
            Filter::SrcIp("10.0.0.0/33".to_string()),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        assert!(
            validate_rule(&r)
                .errors
                .iter()
                .any(|e| matches!(e, RuleValidationError::InvalidPattern { field, .. } if field.contains("src_ip"))),
            "an invalid CIDR must be rejected"
        );
    }

    #[test]
    fn nested_patterns_are_validated_too() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::And(vec![
                Filter::All,
                Filter::Or(vec![Filter::Host(StringMatcher::Regex("(bad".to_string()))]),
            ]),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        assert_eq!(
            validate_rule(&r).errors.len(),
            1,
            "a pattern nested in And/Or must still be checked"
        );
    }

    #[test]
    fn a_body_filter_at_the_wrong_stage_is_rejected() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::ResponseBody(StringMatcher::Contains("x".to_string())),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        assert!(
            validate_rule(&r)
                .errors
                .iter()
                .any(|e| matches!(e, RuleValidationError::FilterNotAvailableAtStage { .. })),
            "a response-body filter cannot run at the request-headers stage"
        );
    }

    #[test]
    fn an_action_at_the_wrong_stage_is_rejected() {
        let r = rule(
            RuleStage::Connect,
            Filter::All,
            vec![Action::SetResponseBody {
                body: BodySource::Text("x".to_string()),
            }],
        );
        assert!(
            validate_rule(&r)
                .errors
                .iter()
                .any(|e| matches!(e, RuleValidationError::ActionNotAvailableAtStage { .. })),
            "a response-body action cannot run at the connect stage"
        );
    }

    /// A rule with no actions is pointless but well-formed, so it must warn rather than be refused:
    /// staging a rule before configuring its actions is a normal workflow.
    #[test]
    fn a_rule_with_no_actions_warns_but_is_not_refused() {
        let r = rule(RuleStage::RequestHeaders, Filter::All, vec![]);
        let report = validate_rule(&r);

        assert!(
            report.is_ok(),
            "an actionless rule must still be acceptable"
        );
        assert_eq!(report.warnings, vec![RuleValidationError::NoActions]);
        assert!(report.errors.is_empty());
    }

    /// Blocking problems are exactly the ones that make a rule unevaluable.
    #[test]
    fn only_unusable_problems_block_acceptance() {
        assert!(!RuleValidationError::NoActions.is_blocking());
        assert!(
            RuleValidationError::InvalidPattern {
                field: "f".to_string(),
                pattern: "p".to_string(),
                detail: "d".to_string(),
            }
            .is_blocking()
        );
        assert!(
            RuleValidationError::FilterNotAvailableAtStage {
                stage: RuleStage::Connect,
                filter: "Url".to_string(),
            }
            .is_blocking()
        );
    }

    /// Every problem is returned at once, so a caller can report a full list rather than one per fix.
    #[test]
    fn all_problems_are_reported_together() {
        let r = rule(
            RuleStage::Connect,
            Filter::Url(StringMatcher::Regex("(bad".to_string())),
            vec![Action::SetResponseBody {
                body: BodySource::Text("x".to_string()),
            }],
        );

        let report = validate_rule(&r);
        assert!(
            report.errors.len() >= 2,
            "expected pattern and stage problems together, got {report:?}"
        );
    }

    /// Inactive rules are validated: they become active later, and a broken rule that only fails once
    /// enabled is worse than one that never loads.
    #[test]
    fn inactive_rules_are_still_validated() {
        let mut r = rule(
            RuleStage::RequestHeaders,
            Filter::Url(StringMatcher::Regex("(bad".to_string())),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        r.active = false;

        assert_eq!(validate_rules_with_ids([&r]).len(), 1);
    }

    #[test]
    fn validate_rules_reports_the_owning_rule_id() {
        let r = rule(
            RuleStage::RequestHeaders,
            Filter::Url(StringMatcher::Regex("(bad".to_string())),
            vec![Action::Tag {
                key: "k".to_string(),
                value: "v".to_string(),
            }],
        );
        let errors = validate_rules_with_ids([&r]);
        assert_eq!(errors[0].0, "r");
    }
}
