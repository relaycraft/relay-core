import type { Flow } from '@/types/api';

/** What the Trace tab shows: the rules that ran, and the variables they left behind. */
export interface FlowTrace {
  matchedRules: string[];
  ruleVariables: [string, string][];
}

/**
 * Read a flow's rule activity, tolerating the shape the API actually sends.
 *
 * `matched_rules` and `rule_variables` are marked `skip_serializing_if` on the Rust side, so an
 * ordinary capture carries **neither key** — they are absent, not empty. The Trace tab read
 * `flow.matched_rules.length` directly, which threw `TypeError: Cannot read properties of undefined`
 * on every flow that no rule had touched. Solid was updating the view when it threw, so the failure
 * did not stay in the Trace tab: it aborted the update queue and left the whole detail pane unable to
 * re-render **any** tab, which is what a reader experiences as the UI freezing.
 *
 * Both fields stay optional in the types for the same reason: a field the wire omits must not be
 * typed as always present.
 */
export function flowTrace(flow: Flow): FlowTrace {
  return {
    matchedRules: [...(flow.matched_rules ?? [])],
    ruleVariables: Object.entries(flow.rule_variables ?? {}),
  };
}
