import { onMount, onCleanup } from 'solid-js';
import { connectSse } from '@/lib/sse';
import { flowToSummary } from '@/lib/flowSummary';
import { store } from '@/lib/store';
import { searchFlows, getMetrics, getStatus, listRules, listIntercepts } from '@/lib/api';
import type { Flow } from '@/types/api';
import Layout from '@/components/Layout';

const MAX_UPSERTS_PER_FRAME = 100;

export default function App() {
  let disconnectSse: (() => void) | null = null;
  let metricsTimer: ReturnType<typeof setInterval> | null = null;
  let pendingFlows: Flow[] = [];
  let rafId: number | null = null;

  function flushFlowBatch() {
    rafId = null;
    const batch = pendingFlows.splice(0, MAX_UPSERTS_PER_FRAME);
    for (const flow of batch) {
      store.upsertFlow(flowToSummary(flow));
    }
    if (pendingFlows.length > 0) {
      rafId = requestAnimationFrame(flushFlowBatch);
    }
  }

  function queueFlowUpdate(flow: Flow) {
    pendingFlows.push(flow);
    if (rafId === null) {
      rafId = requestAnimationFrame(flushFlowBatch);
    }
  }

  async function fullSync() {
    try {
      const [page, metrics, status, rules, interceptSnapshot] = await Promise.all([
        searchFlows({ limit: '200' }),
        getMetrics(),
        getStatus(),
        listRules(),
        listIntercepts(),
      ]);
      store.setState('flows', new Map());
      store.setState('flowOrder', []);
      for (const f of page.items) {
        store.upsertFlow(f);
      }
      store.setState('metrics', metrics);
      store.setState('status', status);
      store.setState('rules', rules);
      store.setState('intercepts', interceptSnapshot.items ?? []);
    } catch {
      // API not available yet — retry later
    }
  }

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
          case 'flow':
            queueFlowUpdate(event.data);
            break;
          case 'lifecycle':
            store.setState('status', event.data);
            break;
          case 'lagged':
            store.setState('sseLagged', (n) => n + 1);
            pendingFlows = [];
            if (rafId !== null) {
              cancelAnimationFrame(rafId);
              rafId = null;
            }
            fullSync();
            break;
          case 'http-body':
            store.notifyHttpBody(event.data.flow_id);
            break;
          case 'body-budget-exceeded':
            store.markBodyBudgetExceeded(event.data.flow_id);
            break;
          case 'ws-message':
            if (store.state.selectedFlowId === event.data.flow_id) {
              store.notifyHttpBody(event.data.flow_id);
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
    if (rafId !== null) cancelAnimationFrame(rafId);
  });

  return <Layout />;
}
