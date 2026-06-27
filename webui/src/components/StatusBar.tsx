import { createSignal, createEffect, onCleanup } from 'solid-js';
import { store } from '@/lib/store';

export default function StatusBar() {
  const status = () => store.state.status;
  const metrics = () => store.state.metrics;
  const sseConnected = () => store.state.sseConnected;
  const sseLagged = () => store.state.sseLagged;

  const [qps, setQps] = createSignal<number | null>(null);
  let prevTotal = 0;
  let prevAt = Date.now();

  createEffect(() => {
    const m = metrics();
    if (!m) return;
    const now = Date.now();
    const dt = (now - prevAt) / 1000;
    if (dt >= 1) {
      const rate = (m.proxy_http_request_total - prevTotal) / dt;
      setQps(Math.max(0, Math.round(rate)));
      prevTotal = m.proxy_http_request_total;
      prevAt = now;
    }
  });

  onCleanup(() => {
    prevTotal = 0;
    prevAt = Date.now();
  });

  return (
    <div class="h-6 flex items-center px-3 bg-surface border-t border-border text-xs text-text-dim shrink-0 gap-4 font-mono">
      <div class="flex items-center gap-1.5">
        <span
          class={`inline-block w-2 h-2 rounded-full ${
            status()?.running ? 'bg-success' : 'bg-text-dim'
          }`}
        />
        <span>Proxy {status()?.port ? `:${status()!.port}` : ''}</span>
      </div>

      <div>QPS {qps() ?? '-'}</div>

      <div>Pending {metrics()?.intercepts_pending ?? 0}</div>

      <div class="flex items-center gap-1.5">
        <span
          class={`inline-block w-2 h-2 rounded-full ${
            sseConnected() ? 'bg-success' : 'bg-error'
          }`}
        />
        SSE
        {sseLagged() > 0 && (
          <span class="text-warn">⚠ {sseLagged()} dropped</span>
        )}
      </div>

      <div class="flex-1" />

      <div class="text-text-dim/60">
        {status()?.phase ?? '?'}
      </div>
    </div>
  );
}
