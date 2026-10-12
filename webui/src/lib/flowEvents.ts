/**
 * The typed lifecycle events the engine publishes as `event: flow-event`.
 *
 * These are the answer to "what just happened to this flow", and they exist because the alternative
 * was inferring it: a paused breakpoint used to be invisible (the UI had to poll), and "who changed
 * this request" had no answer at all — the engine publishes `MutationApplied` with the actor and the
 * field names precisely so that a reader does not have to guess from a before/after diff.
 *
 * Two rules this module follows, both learned from defects in this codebase:
 *
 * 1. **An unknown event is ignored, not guessed at.** A newer engine may add a variant; a UI that
 *    invented a meaning for it, or threw on it, would be the `Trace` tab's freeze all over again.
 *    `describeFlowEvent` returns `null` and the caller drops it.
 * 2. **The wire shape is snake_case tagged** (`{"event":"mutation_applied","flow_id":…}`), which is
 *    what `#[serde(tag = "event", rename_all = "snake_case")]` on the engine's enum produces. This
 *    module reads that shape rather than a hand-drawn type that could disagree with it.
 */

/** One event, reduced to what a person needs to read. */
export interface FlowEventNotice {
  /** `Uuid` of the flow the event is about, as the wire sends it. */
  flowId: string;
  /** The engine's own variant name, snake_case, e.g. `intercept_paused`. */
  kind: FlowEventKind;
  /** One sentence, already written for a reader rather than for a parser. */
  summary: string;
  /** Wall-clock time the client received it. The wire does not carry one for every variant. */
  receivedAt: number;
}

export type FlowEventKind =
  | 'started'
  | 'headers_received'
  | 'body_chunk'
  | 'message_received'
  | 'mutation_applied'
  | 'intercept_paused'
  | 'intercept_resolved'
  | 'completed'
  | 'errored';

const KINDS: FlowEventKind[] = [
  'started',
  'headers_received',
  'body_chunk',
  'message_received',
  'mutation_applied',
  'intercept_paused',
  'intercept_resolved',
  'completed',
  'errored',
];

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

/**
 * Turn one raw payload into a notice, or `null` when this build does not know the event.
 *
 * The `null` is the contract: a client must skip what it does not understand rather than fault the
 * stream (the engine's own SSE decoder says the same thing in the other direction).
 */
export function describeFlowEvent(raw: unknown): FlowEventNotice | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const payload = raw as Record<string, unknown>;

  const kind = asString(payload.event) as FlowEventKind | null;
  if (!kind || !KINDS.includes(kind)) return null;

  const flowId = asString(payload.flow_id);
  if (!flowId) return null;

  const direction = asString(payload.direction);
  let summary: string;

  switch (kind) {
    case 'started':
      summary = 'started';
      break;
    case 'headers_received':
      summary = direction ? `${direction} headers received` : 'headers received';
      break;
    case 'body_chunk': {
      const observed = typeof payload.observed_bytes === 'number' ? payload.observed_bytes : null;
      const truncated = payload.truncated === true ? ' (truncated)' : '';
      summary =
        observed === null
          ? `body chunk${truncated}`
          : `body chunk, ${observed} bytes observed${truncated}`;
      break;
    }
    case 'message_received':
      summary = direction ? `${direction} message` : 'message';
      break;
    case 'mutation_applied': {
      const actor = asString(payload.actor) ?? 'unknown actor';
      const fields = Array.isArray(payload.fields)
        ? payload.fields.filter((f): f is string => typeof f === 'string')
        : [];
      // This is the whole point of the variant: naming who and what, rather than leaving a reader to
      // work it out from a diff.
      summary =
        fields.length > 0
          ? `${actor} changed ${fields.join(', ')}`
          : `${actor} changed this exchange`;
      break;
    }
    case 'intercept_paused': {
      const phase = asString(payload.phase) ?? 'unknown phase';
      summary = `paused at ${phase}`;
      break;
    }
    case 'intercept_resolved': {
      const phase = asString(payload.phase) ?? 'unknown phase';
      summary = payload.mutated === true ? `resumed at ${phase} with edits` : `resumed at ${phase}`;
      break;
    }
    case 'completed':
      summary = 'completed';
      break;
    case 'errored': {
      const reason = payload.reason;
      const detail =
        typeof reason === 'object' && reason !== null
          ? asString((reason as Record<string, unknown>).reason)
          : null;
      summary = detail ? `failed (${detail})` : 'failed';
      break;
    }
  }

  return { flowId, kind, summary, receivedAt: Date.now() };
}

/** Aggregate of the events that change what the interface should show right now. */
export interface FlowEventState {
  /** Flows currently paused by a breakpoint, in the order they were paused. */
  paused: string[];
  /** The latest notice per flow, newest last. */
  notices: Map<string, FlowEventNotice[]>;
}

/** How many notices to keep per flow: enough to read a sequence, bounded so a long stream cannot grow. */
export const MAX_NOTICES_PER_FLOW = 50;

export function createFlowEventState(): FlowEventState {
  return { paused: [], notices: new Map() };
}

/**
 * Fold one notice into the aggregate, in place.
 *
 * Pausing and resolving are the two events whose *aggregate* matters — the count of waiting flows is
 * what a reader acts on — and keeping it here rather than deriving it from `metrics.intercepts_pending`
 * is what makes the interface update the moment a breakpoint fires instead of on the next poll.
 */
export function applyNotice(state: FlowEventState, notice: FlowEventNotice): void {
  const existing = state.notices.get(notice.flowId) ?? [];
  existing.push(notice);
  // Bounded per flow: a stream can emit many events and a night of traffic must not grow the tab.
  if (existing.length > MAX_NOTICES_PER_FLOW) {
    existing.splice(0, existing.length - MAX_NOTICES_PER_FLOW);
  }
  state.notices.set(notice.flowId, existing);

  if (notice.kind === 'intercept_paused') {
    if (!state.paused.includes(notice.flowId)) state.paused.push(notice.flowId);
  } else if (notice.kind === 'intercept_resolved' || notice.kind === 'completed') {
    state.paused = state.paused.filter((id) => id !== notice.flowId);
  }
}
