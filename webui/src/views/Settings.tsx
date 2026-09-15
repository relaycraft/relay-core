import { createSignal, createEffect, Show } from 'solid-js';
import { store } from '@/lib/store';
import { getPolicy, patchPolicy } from '@/lib/api';
import type { ProxyPolicy } from '@/types/api';

/** Bytes the way a person reads them: `10485760 bytes` is `10 MiB`. */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return String(bytes);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const shown = value >= 10 || Number.isInteger(value) ? Math.round(value) : Number(value.toFixed(1));
  return `${shown} ${units[unit]}`;
}

/** Milliseconds the way a person reads them: `30000 ms` is `30 s`. */
function formatMillis(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return String(ms);
  if (ms < 1000) return `${ms} ms`;
  const seconds = ms / 1000;
  return `${Number.isInteger(seconds) ? seconds : seconds.toFixed(1)} s`;
}

export default function SettingsView() {
  const [policy, setPolicy] = createSignal<ProxyPolicy | null>(null);
  const [error, setError] = createSignal('');
  const [saved, setSaved] = createSignal(false);
  const [loading, setLoading] = createSignal(true);

  createEffect(() => {
    if (store.state.activeView === 'settings') {
      void loadPolicy();
    }
  });

  async function loadPolicy() {
    setLoading(true);
    setError('');
    try {
      setPolicy(await getPolicy());
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function toggleRedaction() {
    const p = policy();
    if (!p) return;
    setError('');
    setSaved(false);
    try {
      const updated = await patchPolicy({
        redaction: { enabled: !p.redaction.enabled },
      });
      setPolicy(updated);
      setSaved(true);
    } catch (e: unknown) {
      const msg = String(e);
      if (msg.includes('409')) {
        setError('Upstream proxy change requires restarting the relay process.');
      } else {
        setError(msg);
      }
    }
  }

  async function toggleRedactBodies() {
    const p = policy();
    if (!p) return;
    setError('');
    setSaved(false);
    try {
      const updated = await patchPolicy({
        redaction: { redact_bodies: !p.redaction.redact_bodies },
      });
      setPolicy(updated);
      setSaved(true);
    } catch (e: unknown) {
      setError(String(e));
    }
  }

  return (
    <div class="h-full overflow-y-auto p-4 text-[13px] max-w-2xl mx-auto">
      <h2 class="text-accent font-bold mb-3">Proxy Policy</h2>

      <Show when={loading()} fallback={
        <Show when={policy()} fallback={<div class="text-text-dim">Failed to load policy.</div>}>
          {(p) => (
            <div class="space-y-4">
              <section class="border border-border/60 rounded-md overflow-hidden">
                <h3 class="px-2 py-1 text-[12px] font-bold text-text-dim uppercase tracking-wide bg-surface-alt border-b border-border/60">
                  Redaction
                </h3>
                <div class="p-3 space-y-2">
                <label class="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    class="accent-accent w-3.5 h-3.5"
                    checked={p().redaction.enabled}
                    onChange={toggleRedaction}
                  />
                  <span>Enable header/query redaction</span>
                </label>
                <label class="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    class="accent-accent w-3.5 h-3.5"
                    checked={p().redaction.redact_bodies}
                    onChange={toggleRedactBodies}
                  />
                  <span>Redact request/response bodies</span>
                </label>
                </div>
              </section>

              <section class="border border-border/60 rounded-md overflow-hidden">
                <h3 class="px-2 py-1 text-[12px] font-bold text-text-dim uppercase tracking-wide bg-surface-alt border-b border-border/60">
                  Runtime
                </h3>
                <div class="grid grid-cols-[minmax(0,12rem)_1fr] gap-x-3 gap-y-1 p-3">
                  <span class="text-text-dim">Max body size</span>
                  <span class="tabular-nums">{formatBytes(p().max_body_size)}</span>
                  <span class="text-text-dim">Body inspect budget</span>
                  <span class="tabular-nums">{formatBytes(p().rule_body_inspect_budget)}</span>
                  <span class="text-text-dim">Request timeout</span>
                  <span class="tabular-nums">{formatMillis(p().request_timeout_ms)}</span>
                  <span class="text-text-dim">Transparent proxy</span>
                  <span>{p().transparent_enabled ? 'enabled' : 'disabled'}</span>
                  <span class="text-text-dim">Upstream</span>
                  <span>{p().upstream?.proxy_url ?? 'none'}</span>
                </div>
              </section>

              {saved() && <div class="text-success text-xs">Policy saved.</div>}
            </div>
          )}
        </Show>
      }>
        <div class="text-text-dim text-xs">Loading policy...</div>
      </Show>

      {error() && (
        <div class="mt-3 p-2 bg-error/10 border border-error/30 text-error text-xs">{error()}</div>
      )}
    </div>
  );
}
