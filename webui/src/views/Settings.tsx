import { createSignal, createEffect, Show } from 'solid-js';
import { store } from '@/lib/store';
import { getPolicy, patchPolicy } from '@/lib/api';
import type { ProxyPolicy } from '@/types/api';

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
    <div class="h-full overflow-y-auto p-4 text-sm max-w-2xl">
      <h2 class="text-accent font-bold mb-3">Proxy Policy</h2>

      <Show when={loading()} fallback={
        <Show when={policy()} fallback={<div class="text-text-dim">Failed to load policy.</div>}>
          {(p) => (
            <div class="space-y-4">
              <section class="border border-border rounded p-3 space-y-2">
                <h3 class="text-xs font-bold text-text-dim uppercase tracking-wide">Redaction</h3>
                <label class="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={p().redaction.enabled}
                    onChange={toggleRedaction}
                  />
                  <span>Enable header/query redaction</span>
                </label>
                <label class="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={p().redaction.redact_bodies}
                    onChange={toggleRedactBodies}
                  />
                  <span>Redact request/response bodies</span>
                </label>
              </section>

              <section class="border border-border rounded p-3 space-y-1 text-xs">
                <h3 class="text-xs font-bold text-text-dim uppercase tracking-wide mb-2">Runtime</h3>
                <div class="grid grid-cols-2 gap-1">
                  <span class="text-text-dim">Max body size</span>
                  <span>{p().max_body_size} bytes</span>
                  <span class="text-text-dim">Body inspect budget</span>
                  <span>{p().rule_body_inspect_budget} bytes</span>
                  <span class="text-text-dim">Request timeout</span>
                  <span>{p().request_timeout_ms} ms</span>
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
