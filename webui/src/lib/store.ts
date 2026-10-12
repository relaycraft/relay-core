import { createStore, produce } from 'solid-js/store';
import { grpcOutcome } from '@/lib/flowSummary';
import { isTunnel } from '@/lib/tunnels';
import {
  applyNotice,
  createFlowEventState,
  describeFlowEvent,
  type FlowEventNotice,
} from '@/lib/flowEvents';
import type {
  FlowSummary,
  CoreMetrics,
  CoreStatusSnapshot,
  InterceptItem,
  Rule,
} from '@/types/api';
import { parseSseEvent, type SseEventRecord } from '@/lib/sseEvents';

export type ViewId = 'flows' | 'workshop' | 'rules' | 'scripts' | 'settings';

export const MAX_FLOWS = 5000;

/** How long changes to the open flow wait before one detail refresh. */
export const DETAIL_REFRESH_MS = 200;

/**
 * How many parsed events to keep per flow.
 *
 * Bounded because an event stream is the one body shape that can run for hours: a server pushing a
 * tick per second would otherwise grow this tab without limit, which is the same defect the capture
 * queue had before it was bounded.
 */
export const MAX_SSE_EVENTS_PER_FLOW = 200;

export interface AppState {
  activeView: ViewId;
  flows: Map<string, FlowSummary>;
  flowOrder: string[];
  selectedFlowId: string | null;
  rules: Rule[];
  intercepts: InterceptItem[];
  metrics: CoreMetrics | null;
  status: CoreStatusSnapshot | null;
  sseConnected: boolean;
  sseLagged: number;
  filterText: string;
  showCommandPalette: boolean;
  showHelp: boolean;
  flowDetailGeneration: number;
  bodyBudgetExceeded: Set<string>;
  /** Bumped when the list must be refetched, e.g. after asking to see tunnels again. */
  flowReloadGeneration: number;
  /** Flows currently paused by a breakpoint, in pause order. */
  pausedFlowIds: string[];
  /** Lifecycle notices per flow, newest last. */
  flowNotices: Map<string, FlowEventNotice[]>;
  /** Events parsed from a proxied `text/event-stream`, per flow, newest last. */
  sseEvents: Map<string, SseEventRecord[]>;
}

export function createAppStore() {
  const [state, setState] = createStore<AppState>({
    activeView: 'flows',
    flows: new Map(),
    flowOrder: [],
    selectedFlowId: null,
    rules: [],
    intercepts: [],
    metrics: null,
    status: null,
    sseConnected: false,
    sseLagged: 0,
    filterText: '',
    showCommandPalette: false,
    showHelp: false,
    flowDetailGeneration: 0,
    bodyBudgetExceeded: new Set<string>(),
    flowReloadGeneration: 0,
    pausedFlowIds: [],
    flowNotices: new Map(),
    sseEvents: new Map(),
  });

  // The aggregate lives outside the reactive store and is copied in on change: folding an event into
  // a Map in place would mutate state Solid cannot see.
  const flowEvents = createFlowEventState();

  /**
   * Drop the CONNECT flows from the list, because the reader just asked not to see them.
   *
   * Flipping the setting the other way needs a reload instead: the rows are not in the client any
   * more once they are gone, so only the server can supply them — and it will, for the same reason
   * `tunnels.ts` sends the flag rather than filtering a page locally.
   */
  function hideTunnels() {
    setState(
      produce((s) => {
        const hidden = s.flowOrder.filter((id) => {
          const flow = s.flows.get(id);
          return flow ? isTunnel(flow) : false;
        });
        for (const id of hidden) {
          s.flows.delete(id);
        }
        s.flowOrder = s.flowOrder.filter((id) => !hidden.includes(id));
        if (s.selectedFlowId && hidden.includes(s.selectedFlowId)) {
          s.selectedFlowId = null;
        }
        s.flowDetailGeneration++;
      }),
    );
  }

  /**
   * Apply one typed lifecycle event.
   *
   * Returns whether it was understood: an unknown variant is dropped here, in one place, rather than
   * being given an invented meaning by a caller that happens to pattern-match on the raw payload.
   */
  function applyFlowEvent(raw: unknown): boolean {
    const notice = describeFlowEvent(raw);
    if (!notice) return false;

    applyNotice(flowEvents, notice);
    setState(
      produce((s) => {
        s.pausedFlowIds = [...flowEvents.paused];
        s.flowNotices = new Map(flowEvents.notices);
      }),
    );
    return true;
  }

  /**
   * Record one parsed event from a proxied event stream.
   *
   * These are not on the flow snapshot and cannot be: the engine records the response *body* (bounded
   * by the body budget) and pushes the parsed events incrementally, because making a capture wait for
   * a stream that may never end would mean showing nothing at all. So the client accumulates them,
   * exactly as it does for WebSocket frames.
   */
  function applySseEvent(flowId: string, raw: unknown): boolean {
    // Validated here, at the boundary where the wire enters: a cast at the call site would assert a
    // shape the frame has not promised, and the panel renders whatever is in here.
    const event = parseSseEvent(raw);
    if (!flowId || !event) return false;
    setState(
      produce((s) => {
        const existing = s.sseEvents.get(flowId) ?? [];
        existing.push(event);
        if (existing.length > MAX_SSE_EVENTS_PER_FLOW) {
          existing.splice(0, existing.length - MAX_SSE_EVENTS_PER_FLOW);
        }
        s.sseEvents = new Map(s.sseEvents).set(flowId, [...existing]);
      }),
    );
    return true;
  }

  /** Events recorded for one flow, newest first. */
  function sseEventsFor(flowId: string | null): SseEventRecord[] {
    if (!flowId) return [];
    return [...(state.sseEvents.get(flowId) ?? [])].reverse();
  }

  /** Notices recorded for one flow, newest first. */
  function noticesFor(flowId: string | null): FlowEventNotice[] {
    if (!flowId) return [];
    return [...(state.flowNotices.get(flowId) ?? [])].reverse();
  }

  /** Ask for the list to be refetched. See `views/Settings.tsx` for why showing tunnels needs it. */
  function requestFlowReload() {
    setState('flowReloadGeneration', (n) => n + 1);
  }

  function upsertFlow(summary: FlowSummary) {
    setState(
      produce((s) => {
        if (!s.flows.has(summary.id)) {
          s.flowOrder.unshift(summary.id);
        }
        s.flows.set(summary.id, summary);
        evictOverflow(s);
      }),
    );
  }

  /**
   * Replace the list with a page from the API, keeping the one ordering rule the list has: index 0
   * is the newest capture.
   *
   * The order is computed here rather than inherited from the page. Applying a newest-first page one
   * flow at a time through `upsertFlow` put each one at the *front*, so a refresh reversed the whole
   * list — the newest flow ended up at the bottom, and the next live capture jumped back to the top.
   * Sorting by capture time makes both paths agree, and it also keeps a flow that arrived while the
   * page was in flight instead of dropping it.
   */
  function syncFlows(summaries: FlowSummary[]) {
    setState(
      produce((s) => {
        const known = new Map(s.flows);
        for (const summary of summaries) known.set(summary.id, summary);

        const newestFirst = [...known.values()].sort(
          (a, b) => b.start_time_ms - a.start_time_ms,
        );

        s.flows = new Map();
        s.flowOrder = [];
        for (const summary of newestFirst) {
          s.flows.set(summary.id, summary);
          s.flowOrder.push(summary.id);
        }
        evictOverflow(s);
      }),
    );
  }

  /** Keep the newest `MAX_FLOWS`, never evicting the flow the reader has open. */
  function evictOverflow(s: AppState) {
    while (s.flowOrder.length > MAX_FLOWS) {
      let evicted = false;
      for (let i = s.flowOrder.length - 1; i >= 0 && s.flowOrder.length > MAX_FLOWS; i--) {
        const id = s.flowOrder[i];
        if (id === s.selectedFlowId) continue;
        s.flowOrder.splice(i, 1);
        s.flows.delete(id);
        evicted = true;
      }
      if (!evicted) break;
    }
  }

  function clearFlows() {
    setState('flows', new Map());
    setState('flowOrder', []);
    setState('selectedFlowId', null);
  }

  function setActiveView(view: ViewId) {
    setState('activeView', view);
    if (view !== 'flows') {
      setState('selectedFlowId', null);
    }
  }

  function selectFlow(id: string | null) {
    setState('selectedFlowId', id);
  }

  let detailRefreshTimer: ReturnType<typeof setTimeout> | undefined;

  /**
   * The open flow changed on the wire: re-fetch it once the events stop arriving.
   *
   * Called for every event that can alter a flow already on screen — a body chunk, trailers, a stream
   * reset, a WebSocket message, and the snapshot that carries the response headers. A live connection
   * emits one of these per chunk, and refreshing on every one keeps the detail request in flight, so
   * the pane never leaves "Loading..."; one refresh per quiet gap is enough.
   *
   * The snapshot case is what made a flow selected while it was in flight stay blank: its response
   * arrived as a `flow` event, the list row updated, and the pane kept the body it had fetched at
   * selection time — which had no response in it — until the reader selected the row again.
   */
  function notifyFlowChanged(flowId: string) {
    if (state.selectedFlowId !== flowId) return;
    if (detailRefreshTimer !== undefined) return;
    detailRefreshTimer = setTimeout(() => {
      detailRefreshTimer = undefined;
      if (state.selectedFlowId === flowId) {
        setState('flowDetailGeneration', (n) => n + 1);
      }
    }, DETAIL_REFRESH_MS);
  }

  function applyGrpcTrailers(flowId: string, trailers: [string, string][]) {
    // One parser for both paths into a row: this event patches a row that `flowToSummary` already
    // summarised, and a laxer read here put a `g14` badge on a row that a refresh then dropped.
    const { status: grpcStatus, message: grpcMessage } = grpcOutcome(trailers);
    setState(
      produce((s) => {
        const summary = s.flows.get(flowId);
        if (!summary) return;
        summary.grpc_status = grpcStatus;
        summary.grpc_message = grpcMessage;
        if (grpcStatus != null && grpcStatus !== 0) summary.has_error = true;
      }),
    );
  }

  function markStreamReset(flowId: string) {
    setState(
      produce((s) => {
        const summary = s.flows.get(flowId);
        if (!summary) return;
        summary.has_error = true;
        if (!summary.tags.includes('stream-reset')) summary.tags.push('stream-reset');
      }),
    );
  }

  function markBodyBudgetExceeded(flowId: string) {
    setState(
      produce((s) => {
        s.bodyBudgetExceeded = new Set(s.bodyBudgetExceeded).add(flowId);
      }),
    );
  }

  return {
    state,
    setState,
    upsertFlow,
    syncFlows,
    hideTunnels,
    applyFlowEvent,
    applySseEvent,
    sseEventsFor,
    noticesFor,
    requestFlowReload,
    clearFlows,
    setActiveView,
    selectFlow,
    notifyFlowChanged,
    applyGrpcTrailers,
    markStreamReset,
    markBodyBudgetExceeded,
  };
}

export const store = createAppStore();
