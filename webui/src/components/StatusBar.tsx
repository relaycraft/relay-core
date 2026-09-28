import { createSignal, createEffect } from 'solid-js';
import { store } from '@/lib/store';
import { createRateMeter } from '@/lib/rate';
import { IconWarning } from '@/components/Icons';

export default function StatusBar() {
  const status = () => store.state.status;
  const metrics = () => store.state.metrics;
  const sseConnected = () => store.state.sseConnected;
  const sseLagged = () => store.state.sseLagged;

  const [qps, setQps] = createSignal<number | null>(null);
  // The counter is a lifetime total, so the first reading is a baseline rather than a rate. See
  // `lib/rate.ts` — this used to divide that total by the time since the page opened, and displayed a
  // request rate that had never happened for the first interval.
  const sampleRate = createRateMeter();

  createEffect(() => {
    const m = metrics();
    if (!m) return;
    const rate = sampleRate(m.proxy_http_request_total, Date.now());
    if (rate != null) setQps(rate);
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
          <span class="text-warn inline-flex items-center gap-1">
            <IconWarning size={13} /> {sseLagged()} dropped
          </span>
        )}
      </div>

      <div class="flex-1" />

      <div class="text-text-dim/60">
        {status()?.phase ?? '?'}
      </div>
    </div>
  );
}
