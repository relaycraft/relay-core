import { createSignal, createEffect, For, Show, onMount, onCleanup } from 'solid-js';
import { store } from '@/lib/store';
import { listIntercepts, setIntercept, resumeIntercept } from '@/lib/api';
import { isEditingTarget } from '@/lib/editing';
import type { InterceptItem, FlowModification } from '@/types/api';

export default function WorkshopView() {
  const [urlPattern, setUrlPattern] = createSignal('');
  const [phase, setPhase] = createSignal<'request' | 'response'>('request');
  const [intercepts, setIntercepts] = createSignal<InterceptItem[]>([]);
  const [selectedKey, setSelectedKey] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal(false);

  // Modification state
  const [modMethod, setModMethod] = createSignal('');
  const [modUrl, setModUrl] = createSignal('');
  const [modStatusCode, setModStatusCode] = createSignal('');
  const [modReqHeaders, setModReqHeaders] = createSignal('');
  const [modReqBody, setModReqBody] = createSignal('');
  const [modResHeaders, setModResHeaders] = createSignal('');
  const [modResBody, setModResBody] = createSignal('');

  let pollTimer: ReturnType<typeof setInterval> | null = null;

  createEffect(() => {
    if (store.state.activeView === 'workshop') {
      pollIntercepts();
      pollTimer = setInterval(pollIntercepts, 1000);
    } else {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    }
  });

  async function pollIntercepts() {
    try {
      const snapshot = await listIntercepts();
      setIntercepts(snapshot.items ?? []);
    } catch {}
  }

  async function handleSetBreakpoint() {
    if (!urlPattern().trim()) return;
    try {
      await setIntercept({ url_pattern: urlPattern().trim(), phase: phase() });
      setUrlPattern('');
      pollIntercepts();
    } catch {}
  }

  async function handleResume(action: 'continue' | 'drop' | 'reject') {
    const key = selectedKey();
    if (!key) return;

    let modifications: FlowModification | undefined;
    if (action === 'continue' && editing()) {
      modifications = {};
      if (modMethod()) modifications.method = modMethod();
      if (modUrl()) modifications.url = modUrl();
      if (modStatusCode()) modifications.status_code = parseInt(modStatusCode());
      if (modReqHeaders()) {
        try { modifications.request_headers = JSON.parse(modReqHeaders()); } catch {}
      }
      if (modReqBody()) modifications.request_body = modReqBody();
      if (modResHeaders()) {
        try { modifications.response_headers = JSON.parse(modResHeaders()); } catch {}
      }
      if (modResBody()) modifications.response_body = modResBody();
    }

    try {
      await resumeIntercept(key, action, action === 'continue' ? modifications : undefined);
      setSelectedKey(null);
      setEditing(false);
      pollIntercepts();
    } catch {}
  }

  function selectIntercept(item: InterceptItem) {
    setSelectedKey(item.key);
    setEditing(false);
    setModMethod('');
    setModUrl('');
    setModStatusCode('');
    setModReqHeaders('');
    setModReqBody('');
    setModResHeaders('');
    setModResBody('');
  }

  function selectInterceptByOffset(offset: number) {
    const items = intercepts();
    if (items.length === 0) return;
    const current = selectedKey();
    let index = current ? items.findIndex((i) => i.key === current) : -1;
    if (index < 0) index = offset > 0 ? -1 : items.length;
    index = Math.max(0, Math.min(items.length - 1, index + offset));
    selectIntercept(items[index]);
  }

  function handleWorkshopKeyDown(e: KeyboardEvent) {
    if (store.state.activeView !== 'workshop') return;
    if (isEditingTarget(e.target)) return;

    const mod = e.metaKey || e.ctrlKey;

    if (mod && e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void handleResume('continue');
      return;
    }
    if (mod && e.shiftKey && e.key === 'Enter') {
      e.preventDefault();
      void handleResume('drop');
      return;
    }
    if (e.key === 'R' && !mod) {
      e.preventDefault();
      void handleResume('reject');
      return;
    }
    if (e.key === 'j' || e.key === 'ArrowDown') {
      e.preventDefault();
      selectInterceptByOffset(1);
    } else if (e.key === 'k' || e.key === 'ArrowUp') {
      e.preventDefault();
      selectInterceptByOffset(-1);
    }
  }

  onMount(() => {
    document.addEventListener('keydown', handleWorkshopKeyDown);
  });

  onCleanup(() => {
    document.removeEventListener('keydown', handleWorkshopKeyDown);
  });

  return (
    <div class="h-full flex">
      {/* Left: Pending intercepts + breakpoint form */}
      <div class="w-[40%] min-w-[250px] flex flex-col border-r border-border">
        {/* Set breakpoint form */}
        <div class="p-2 border-b border-border">
          <div class="flex gap-1 mb-1">
            <input
              class="flex-1 bg-surface border border-border rounded px-2 py-1 text-xs text-text placeholder-text-dim"
              placeholder="URL pattern (e.g. /api/*)"
              value={urlPattern()}
              onInput={(e) => setUrlPattern(e.currentTarget.value)}
            />
          </div>
          <div class="flex gap-2">
            <select
              class="bg-surface border border-border rounded px-2 py-1 text-xs text-text"
              value={phase()}
              onChange={(e) => setPhase(e.currentTarget.value as 'request' | 'response')}
            >
              <option value="request">Request</option>
              <option value="response">Response</option>
            </select>
            <button
              class="px-3 py-1 bg-accent/20 text-accent text-xs rounded hover:bg-accent/30 transition-colors"
              onClick={handleSetBreakpoint}
            >
              Set Breakpoint
            </button>
          </div>
        </div>

        {/* Pending intercepts list */}
        <div class="flex-1 overflow-y-auto">
          <Show when={intercepts().length > 0} fallback={
            <div class="flex items-center justify-center h-full text-text-dim text-sm">
              Waiting for intercepts...
            </div>
          }>
            <For each={intercepts()}>
              {(item) => (
                <button
                  class={`w-full text-left p-2 border-b border-border/30 text-xs transition-colors ${
                    selectedKey() === item.key ? 'bg-accent/15 text-text' : 'hover:bg-hover text-text-dim'
                  }`}
                  onClick={() => selectIntercept(item)}
                >
                  <div class="flex items-center gap-2">
                    <span class={`px-1 py-0.5 rounded text-[10px] ${
                      item.phase === 'request' ? 'bg-accent/20 text-accent' : 'bg-warn/20 text-warn'
                    }`}>
                      {item.phase}
                    </span>
                    <span class="font-bold">{item.method}</span>
                    <span class="truncate text-text-dim">{item.url}</span>
                  </div>
                </button>
              )}
            </For>
          </Show>
        </div>
      </div>

      {/* Right: Edit / Actions */}
      <div class="flex-1 flex flex-col min-w-0">
        <Show when={selectedKey()} fallback={
          <div class="flex-1 flex items-center justify-center text-text-dim text-sm">
            Select a pending intercept to edit
          </div>
        }>
          <div class="flex-1 overflow-y-auto p-2 space-y-3">
            <Show when={editing()} fallback={
              <div class="text-text-dim text-sm p-4">
                Click "Edit" to modify this request/response, or use the action buttons below.
              </div>
            }>
              <div class="space-y-2 text-xs">
                <div class="grid grid-cols-2 gap-2">
                  <div>
                    <label class="text-text-dim text-[10px] block">Method</label>
                    <input class="w-full bg-surface border border-border rounded px-2 py-1 text-text" value={modMethod()} onInput={(e) => setModMethod(e.currentTarget.value)} />
                  </div>
                  <div>
                    <label class="text-text-dim text-[10px] block">Status Code</label>
                    <input class="w-full bg-surface border border-border rounded px-2 py-1 text-text" value={modStatusCode()} onInput={(e) => setModStatusCode(e.currentTarget.value)} placeholder="e.g. 200" />
                  </div>
                </div>
                <div>
                  <label class="text-text-dim text-[10px] block">URL</label>
                  <input class="w-full bg-surface border border-border rounded px-2 py-1 text-text" value={modUrl()} onInput={(e) => setModUrl(e.currentTarget.value)} />
                </div>
                <div>
                  <label class="text-text-dim text-[10px] block">Request Headers (JSON)</label>
                  <textarea class="w-full h-16 bg-surface border border-border rounded px-2 py-1 text-text font-mono text-[11px]" value={modReqHeaders()} onInput={(e) => setModReqHeaders(e.currentTarget.value)} placeholder='{"X-Custom": "value"}' />
                </div>
                <div>
                  <label class="text-text-dim text-[10px] block">Request Body</label>
                  <textarea class="w-full h-16 bg-surface border border-border rounded px-2 py-1 text-text font-mono text-[11px]" value={modReqBody()} onInput={(e) => setModReqBody(e.currentTarget.value)} />
                </div>
                <div>
                  <label class="text-text-dim text-[10px] block">Response Headers (JSON)</label>
                  <textarea class="w-full h-16 bg-surface border border-border rounded px-2 py-1 text-text font-mono text-[11px]" value={modResHeaders()} onInput={(e) => setModResHeaders(e.currentTarget.value)} placeholder='{"Content-Type": "application/json"}' />
                </div>
                <div>
                  <label class="text-text-dim text-[10px] block">Response Body</label>
                  <textarea class="w-full h-16 bg-surface border border-border rounded px-2 py-1 text-text font-mono text-[11px]" value={modResBody()} onInput={(e) => setModResBody(e.currentTarget.value)} />
                </div>
              </div>
            </Show>
          </div>

          {/* Action bar */}
          <div class="h-10 flex items-center gap-2 px-2 bg-surface border-t border-border shrink-0">
            <button
              class="px-3 py-1 text-xs text-text-dim hover:text-text transition-colors"
              onClick={() => setEditing(!editing())}
            >
              {editing() ? 'Cancel Edit' : 'Edit'}
            </button>
            <div class="flex-1" />
            <button
              class="px-3 py-1 bg-success/20 text-success text-xs rounded hover:bg-success/30 transition-colors font-bold"
              onClick={() => handleResume('continue')}
              title="Cmd+Enter"
            >
              Accept
            </button>
            <button
              class="px-3 py-1 bg-error/20 text-error text-xs rounded hover:bg-error/30 transition-colors"
              onClick={() => handleResume('drop')}
              title="Cmd+Backspace"
            >
              Drop
            </button>
            <button
              class="px-3 py-1 bg-warn/20 text-warn text-xs rounded hover:bg-warn/30 transition-colors"
              onClick={() => handleResume('reject')}
            >
              Reject
            </button>
          </div>
        </Show>
      </div>
    </div>
  );
}
