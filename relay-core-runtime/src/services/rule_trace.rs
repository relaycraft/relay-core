//! Bounded, in-memory per-flow rule-evaluation traces, for readers that ask after the fact.
//!
//! `FlowEvent::RuleEvaluated` already streams each evaluation, but a stream is only useful to a
//! consumer that was listening. An agent asking "why did this rule not fire on flow X" arrives
//! later, so the runtime keeps the most recent evaluations per flow for that question.
//!
//! This is deliberately **not** persisted: a trace is diagnostic data about a live process, and
//! writing every evaluated rule to SQLite would grow the database for a view nobody replays.

use relay_core_api::rule::RuleExecutionEvent;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

/// How many flows keep a trace. Bounded separately from the flow history so a trace request cannot
/// keep an otherwise-evicted flow's rules alive in memory.
pub const MAX_TRACED_FLOWS: usize = 256;

/// How many evaluations are kept per flow. A rule set large enough to exceed this has its oldest
/// evaluations dropped first, and the API reports the truncation rather than pretending otherwise.
pub const MAX_EVENTS_PER_FLOW: usize = 100;

/// One flow's trace and whether the event budget cut it short.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RuleTraceSnapshot {
    pub events: Vec<RuleExecutionEvent>,
    pub truncated: bool,
}

#[derive(Default)]
pub struct RuleTraceStore {
    /// Insertion order, so the oldest flow is the one evicted.
    order: VecDeque<Uuid>,
    entries: HashMap<Uuid, VecDeque<RuleExecutionEvent>>,
}

impl RuleTraceStore {
    /// Record one evaluation, evicting the oldest flow when the budget is full.
    pub fn record(&mut self, flow_id: Uuid, event: RuleExecutionEvent) {
        if !self.entries.contains_key(&flow_id) {
            while self.order.len() >= MAX_TRACED_FLOWS {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                self.entries.remove(&oldest);
            }
            self.order.push_back(flow_id);
        }

        let events = self.entries.entry(flow_id).or_default();
        if events.len() >= MAX_EVENTS_PER_FLOW {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// The recorded evaluations for one flow, oldest first.
    pub fn get(&self, flow_id: Uuid) -> Vec<RuleExecutionEvent> {
        self.entries
            .get(&flow_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the flow's trace was truncated by [`MAX_EVENTS_PER_FLOW`].
    pub fn is_truncated(&self, flow_id: Uuid) -> bool {
        self.entries
            .get(&flow_id)
            .is_some_and(|events| events.len() >= MAX_EVENTS_PER_FLOW)
    }

    pub fn snapshot(&self, flow_id: Uuid) -> RuleTraceSnapshot {
        RuleTraceSnapshot {
            events: self.get(flow_id),
            truncated: self.is_truncated(flow_id),
        }
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core_api::rule::{RuleOutcome, RuleStage};

    fn event(rule_id: &str) -> RuleExecutionEvent {
        RuleExecutionEvent {
            rule_id: rule_id.to_string(),
            stage: RuleStage::RequestHeaders,
            matched: false,
            duration_us: 1,
            outcome: RuleOutcome::NotMatched,
        }
    }

    #[test]
    fn events_are_kept_per_flow_and_in_order() {
        let mut store = RuleTraceStore::default();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        store.record(a, event("one"));
        store.record(b, event("two"));
        store.record(a, event("three"));

        let recorded = store.get(a);
        let ids: Vec<&str> = recorded
            .iter()
            .map(|event| event.rule_id.as_str())
            .collect();
        assert_eq!(ids, vec!["one", "three"], "order and ownership must hold");
        assert_eq!(store.get(b).len(), 1);
    }

    #[test]
    fn a_flow_evicted_from_the_budget_loses_its_trace() {
        let mut store = RuleTraceStore::default();
        let first = Uuid::new_v4();
        store.record(first, event("first"));
        for _ in 0..MAX_TRACED_FLOWS {
            store.record(Uuid::new_v4(), event("later"));
        }
        assert!(
            store.get(first).is_empty(),
            "the oldest flow must not keep a trace after eviction"
        );
    }

    #[test]
    fn a_long_trace_is_bounded_and_reports_truncation() {
        let mut store = RuleTraceStore::default();
        let flow = Uuid::new_v4();
        for index in 0..(MAX_EVENTS_PER_FLOW + 5) {
            store.record(flow, event(&format!("rule-{index}")));
        }
        let events = store.get(flow);
        assert_eq!(events.len(), MAX_EVENTS_PER_FLOW);
        assert_eq!(
            events.last().map(|event| event.rule_id.as_str()),
            Some(format!("rule-{}", MAX_EVENTS_PER_FLOW + 4).as_str()),
            "the newest evaluations survive"
        );
        assert!(store.is_truncated(flow));
    }
}
