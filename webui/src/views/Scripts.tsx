import { createSignal } from 'solid-js';
import { loadScript } from '@/lib/api';

const DEFAULT_SCRIPT = `// RelayCore Deno Script
// Use handler functions to intercept traffic:
//
// export function onRequest(ctx) { ... }
// export function onResponse(ctx) { ... }
// export function onWebSocketMessage(ctx) { ... }
//
// ctx provides: method, url, headers, body, etc.
// Use relay.env('VAR_NAME') to access env vars.

export function onRequest(ctx) {
  console.log('Request:', ctx.method, ctx.url);
  // return modified ctx to alter the request
  return ctx;
}

export function onResponse(ctx) {
  console.log('Response:', ctx.status);
  return ctx;
}
`;

export default function ScriptsView() {
  const [content, setContent] = createSignal(DEFAULT_SCRIPT);
  const [logs, setLogs] = createSignal<string[]>([]);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal('');

  async function handleSave() {
    setSaving(true);
    setError('');
    try {
      await loadScript(content());
      addLog('[system] Script loaded successfully');
    } catch (e: unknown) {
      setError(String(e));
      addLog('[error] Failed to load script: ' + String(e));
    } finally {
      setSaving(false);
    }
  }

  function addLog(msg: string) {
    setLogs((prev) => [...prev.slice(-500), msg]);
  }

  function handleKeyDown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') {
      e.preventDefault();
      handleSave();
    }
  }

  return (
    <div class="h-full flex flex-col">
      {/* Top: Script editor */}
      <div class="flex-1 flex flex-col min-h-0">
        <div class="h-7 flex items-center px-2 bg-surface border-b border-border text-xs shrink-0 gap-2">
          <span class="text-accent font-bold">Script Editor</span>
          <span class="text-text-dim/50 text-[10px]">Deno-compatible TypeScript</span>
          <div class="flex-1" />
          <button
            class={`px-3 py-0.5 text-xs rounded transition-colors ${
              saving()
                ? 'bg-text-dim/20 text-text-dim'
                : 'bg-accent/20 text-accent hover:bg-accent/30'
            }`}
            onClick={handleSave}
            disabled={saving()}
          >
            {saving() ? 'Saving...' : 'Save & Reload (Cmd+S)'}
          </button>
        </div>
        <textarea
          class="flex-1 bg-transparent text-text font-mono text-xs p-3 resize-none focus:outline-none"
          value={content()}
          onInput={(e) => setContent(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          spellcheck={false}
        />
      </div>

      {/* Error bar */}
      {error() && (
        <div class="px-3 py-1 bg-error/10 border-t border-error/30 text-error text-xs">{error()}</div>
      )}

      {/* Bottom: Console log */}
      <div class="h-[30%] min-h-[100px] flex flex-col border-t border-border">
        <div class="h-6 flex items-center px-2 bg-surface border-b border-border text-[10px] text-text-dim shrink-0">
          Console
          <button
            class="ml-2 text-text-dim/50 hover:text-text transition-colors"
            onClick={() => setLogs([])}
          >
            Clear
          </button>
        </div>
        <div class="flex-1 overflow-y-auto p-2 font-mono text-[11px]">
          {logs().length === 0 && (
            <div class="text-text-dim/40">Script console output will appear here...</div>
          )}
          {logs().map((line, i) => (
            <div class="text-text-dim/70">{line}</div>
          ))}
        </div>
      </div>
    </div>
  );
}
