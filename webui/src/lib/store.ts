import { createStore, produce } from 'solid-js/store';
import type { FlowSummary, CoreMetrics, CoreStatusSnapshot, InterceptItem, Rule } from '@/types/api';

export type ViewId = 'flows' | 'workshop' | 'rules' | 'scripts' | 'settings';

export const MAX_FLOWS = 5000;

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
      }),
    );
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

  function notifyHttpBody(flowId: string) {
    if (state.selectedFlowId === flowId) {
      setState('flowDetailGeneration', (n) => n + 1);
    }
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
    clearFlows,
    setActiveView,
    selectFlow,
    notifyHttpBody,
    markBodyBudgetExceeded,
  };
}

export const store = createAppStore();
