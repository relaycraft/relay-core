//! Explicit body-handling plan.
//!
//! Roadmap §3-3 ("引入 BodyPlan"): deciding how a body stream is handled must be a *decision*, not
//! an accident of which host happens to buffer. The rule engine's body stages can only match on a
//! body it can see, yet buffering every body destroys streaming for the common case where nothing
//! inspects the body at all (roadmap §22: "无修改场景不无谓解压/缓冲").
//!
//! This module owns the decision; the engine crate owns the mechanics of carrying it out, so every
//! host reaches the same conclusion from the same inputs.

use serde::{Deserialize, Serialize};

/// How a body stream should be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyPlan {
    /// Forward frames as they arrive; no copy is retained.
    PassThrough,
    /// Forward frames as they arrive while retaining a bounded copy for observation.
    ///
    /// This is what `TapBody` implements: streaming is preserved, so it is safe as a default.
    Capture { limit: usize },
    /// Materialize the whole body (bounded) before it is forwarded, because something needs to
    /// inspect or rewrite it.
    Buffer { limit: usize },
}

impl BodyPlan {
    /// Does this plan require the caller to hold the body before it can proceed?
    pub const fn needs_full_body(&self) -> bool {
        matches!(self, BodyPlan::Buffer { .. })
    }

    /// Retained-byte budget, if the plan retains anything.
    pub const fn limit(&self) -> Option<usize> {
        match self {
            BodyPlan::PassThrough => None,
            BodyPlan::Capture { limit } | BodyPlan::Buffer { limit } => Some(*limit),
        }
    }
}

/// Inputs that determine the plan for one direction of one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyPlanInputs {
    /// Whether any enabled rule targets this direction's body stage.
    pub has_body_stage_rules: bool,
    /// Whether a script hook is registered for this direction's body stage.
    pub has_body_hook_script: bool,
    /// Whether a manual breakpoint is configured for this direction's body stage.
    pub has_body_intercept: bool,
    /// Whether the adapter wants a copy for display/storage even when nothing inspects it.
    pub wants_observation: bool,
    /// Maximum bytes that may be retained or buffered.
    pub budget: usize,
}

/// Choose the cheapest plan that still satisfies every consumer of this body.
///
/// A zero budget disables buffering entirely: rules that need the full body cannot run, and the
/// caller must record that degradation rather than silently matching against an empty body.
pub const fn decide(inputs: BodyPlanInputs) -> BodyPlan {
    if inputs.budget == 0 {
        return BodyPlan::PassThrough;
    }
    if inputs.has_body_stage_rules || inputs.has_body_hook_script || inputs.has_body_intercept {
        BodyPlan::Buffer {
            limit: inputs.budget,
        }
    } else if inputs.wants_observation {
        BodyPlan::Capture {
            limit: inputs.budget,
        }
    } else {
        BodyPlan::PassThrough
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyPlan, BodyPlanInputs, decide};

    fn inputs() -> BodyPlanInputs {
        BodyPlanInputs {
            has_body_stage_rules: false,
            has_body_hook_script: false,
            has_body_intercept: false,
            wants_observation: false,
            budget: 1024,
        }
    }

    #[test]
    fn nothing_interested_means_no_copy_at_all() {
        assert_eq!(decide(inputs()), BodyPlan::PassThrough);
    }

    #[test]
    fn observation_alone_uses_bounded_capture_and_keeps_streaming() {
        let plan = decide(BodyPlanInputs {
            wants_observation: true,
            ..inputs()
        });
        assert_eq!(plan, BodyPlan::Capture { limit: 1024 });
        assert!(
            !plan.needs_full_body(),
            "capture must not force the body to be materialized"
        );
    }

    #[test]
    fn any_rewriting_consumer_forces_a_buffered_body() {
        for mutate in [
            |i: &mut BodyPlanInputs| i.has_body_stage_rules = true,
            |i: &mut BodyPlanInputs| i.has_body_hook_script = true,
            |i: &mut BodyPlanInputs| i.has_body_intercept = true,
        ] {
            let mut i = inputs();
            mutate(&mut i);
            let plan = decide(i);
            assert_eq!(plan, BodyPlan::Buffer { limit: 1024 });
            assert!(plan.needs_full_body());
        }
    }

    #[test]
    fn zero_budget_disables_buffering_rather_than_buffering_everything() {
        let plan = decide(BodyPlanInputs {
            has_body_stage_rules: true,
            budget: 0,
            ..inputs()
        });
        assert_eq!(plan, BodyPlan::PassThrough);
        assert_eq!(plan.limit(), None);
    }
}
