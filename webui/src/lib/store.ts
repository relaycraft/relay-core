import { createStore, produce } from 'solid-js/store';
import { grpcOutcome } from '@/lib/flowSummary';
import type { FlowSummary, CoreMetrics, CoreStatusSnapshot, InterceptItem, Rule } from '@/types/api';

export type ViewId = 'flows' | 'workshop' | 'rules' | 'scripts' | 'settings';

export const MAX_FLOWS = 5000;

/** How long changes to the open flow wait before one detail refresh. */
export const DETAIL_REFRESH_MS = 200;

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
  });

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
