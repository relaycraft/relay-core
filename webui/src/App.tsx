import { onMount, onCleanup } from 'solid-js';
import { connectSse } from '@/lib/sse';
import { flowToSummary } from '@/lib/flowSummary';
import { createFlowBatcher } from '@/lib/flowQueue';
import { store } from '@/lib/store';
import { searchFlows, getMetrics, getStatus, listRules, listIntercepts } from '@/lib/api';
import Layout from '@/components/Layout';

export default function App() {
  let disconnectSse: (() => void) | null = null;
  let metricsTimer: ReturnType<typeof setInterval> | null = null;

  async function fullSync() {
    try {
      const [page, metrics, status, rules, interceptSnapshot] = await Promise.all([
        searchFlows({ limit: '200' }),
        getMetrics(),
        getStatus(),
        listRules(),
        listIntercepts(),
      ]);
      store.setState('metrics', metrics);
      store.setState('status', status);
      store.setState('rules', rules);
      store.setState('intercepts', interceptSnapshot.items ?? []);
      // The list is replaced through the store so the ordering rule lives in one place: the page
      // arrives newest first, but it must not be *applied* one flow at a time, which reversed it.
      store.syncFlows(page.items);
    } catch {
      // API not available yet — retry later
    }
  }

  /**
   * Captures reach the list one frame at a time, and the queue is bounded (`lib/flowQueue.ts`): a
   * background tab does not run frame callbacks, so it is the queue — not the frame — that decides
   * how much memory a night of traffic costs.
   */
  const flows = createFlowBatcher({
    apply: (flow) => store.upsertFlow(flowToSummary(flow)),
    resync: () => void fullSync(),
  });

  async function refreshMetrics() {
    try {
      const metrics = await getMetrics();
      store.setState('metrics', metrics);
    } catch {}
  }

  onMount(() => {
    fullSync();
    metricsTimer = setInterval(refreshMetrics, 2000);

    disconnectSse = connectSse(
      (event) => {
        switch (event.type) {
          case 'connected':
            store.setState('sseConnected', true);
            break;
          case 'disconnected':
            store.setState('sseConnected', false);
            break;
          case 'flow':
            flows.push(event.data);
            // The same snapshot carries the response once it has been captured, so a flow the reader
            // opened while it was in flight must be re-read — otherwise its pane never learns that it
            // was answered.
            store.notifyFlowChanged(event.data.id);
            break;
          case 'lifecycle':
            store.setState('status', event.data);
            break;
          case 'lagged':
            store.setState('sseLagged', (n) => n + 1);
            // The dropped events are gone, so the queued snapshots are a partial list and the API is
            // the authority from here.
            flows.reset();
            fullSync();
            break;
          case 'http-body':
            store.notifyFlowChanged(event.data.flow_id);
            break;
          case 'response-trailers':
            store.applyGrpcTrailers(event.data.flow_id, event.data.trailers);
            store.notifyFlowChanged(event.data.flow_id);
            break;
          case 'stream-reset':
            store.markStreamReset(event.data.flow_id);
            store.notifyFlowChanged(event.data.flow_id);
            break;
          case 'body-budget-exceeded':
            store.markBodyBudgetExceeded(event.data.flow_id);
            break;
          case 'ws-message':
            if (store.state.selectedFlowId === event.data.flow_id) {
              store.notifyFlowChanged(event.data.flow_id);
            }
            break;
        }
      },
      { onConnected: fullSync },
    );
  });

  onCleanup(() => {
    disconnectSse?.();
    if (metricsTimer) clearInterval(metricsTimer);
    flows.reset();
  });

  return <Layout />;
}
