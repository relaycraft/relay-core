/** One rule evaluation as `GET /flows/{id}/trace` returns it. */
export interface RuleTraceEvent {
  rule_id: string;
  stage: string;
  matched: boolean;
  duration_us: number;
  outcome: string | { Failed: string };
}

export interface FlowRuleTrace {
  flow_id: string;
  events: RuleTraceEvent[];
  truncated: boolean;
  note?: string;
}

/**
 * One line a person can read: what was evaluated, whether it matched, and what happened.
 *
 * `NotMatched` is not repeated as a verdict — "did not match" already says it — while a failure
 * keeps its reason, because that is the part a reader acts on.
 */
export function describeRuleEvaluation(event: RuleTraceEvent): string {
  const verdict =
    typeof event.outcome === 'string'
      ? event.outcome === 'NotMatched'
        ? null
        : event.outcome.toLowerCase().replaceAll('_', ' ')
      : `failed: ${event.outcome.Failed}`;
  const base = `${event.rule_id} ${event.matched ? 'matched' : 'did not match'} at ${event.stage}`;
  return verdict ? `${base} (${verdict}, ${event.duration_us}µs)` : base;
}
