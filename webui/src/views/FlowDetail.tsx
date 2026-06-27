import { createSignal, createResource, Show, For, Switch, Match } from 'solid-js';
import { getFlow, replayFlow, exportFlowHar } from '@/lib/api';
import { buildCurlCommand } from '@/lib/flowActions';
import { store } from '@/lib/store';
import type { Flow, HttpLayer, BodyData } from '@/types/api';

type DetailTab = 'headers' | 'payload' | 'timing' | 'messages' | 'trace';

export default function FlowDetail(props: { flowId: string }) {
  const [activeTab, setActiveTab] = createSignal<DetailTab>('headers');
  const [payloadView, setPayloadView] = createSignal<'json' | 'hex' | 'text'>('json');

  const [flow] = createResource(
    () => ({ id: props.flowId, gen: store.state.flowDetailGeneration }),
    ({ id }) => getFlow(id),
  );

  function copyCurl() {
    const f = flow();
    if (!f) return;
    const cmd = buildCurlCommand(f);
    if (cmd) navigator.clipboard.writeText(cmd).catch(() => {});
  }

  async function handleExportHar() {
    try {
      const res = await exportFlowHar(props.flowId);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `flow-${props.flowId}.har`;
      a.click();
      URL.revokeObjectURL(url);
    } catch {}
  }

  async function handleReplay() {
    try {
      await replayFlow(props.flowId);
    } catch {}
  }

  const tabs: DetailTab[] = ['headers', 'payload', 'timing', 'messages', 'trace'];

  return (
    <div class="h-full flex flex-col">
      {/* Tab bar */}
      <div class="h-7 flex items-center bg-surface border-b border-border px-1 shrink-0">
        <For each={tabs}>
          {(tab) => (
            <button
              class={`px-3 h-full text-xs transition-colors ${
                activeTab() === tab
                  ? 'text-accent border-b border-accent'
                  : 'text-text-dim hover:text-text'
              }`}
              onClick={() => setActiveTab(tab)}
            >
              {tab.charAt(0).toUpperCase() + tab.slice(1)}
            </button>
          )}
        </For>
        <div class="flex-1" />
        <button
          class="px-2 h-6 text-[10px] text-text-dim hover:text-text transition-colors"
          onClick={copyCurl}
          title="Copy as cURL"
        >
          cURL
        </button>
        <button
          class="px-2 h-6 text-[10px] text-text-dim hover:text-text transition-colors"
          onClick={handleReplay}
          title="Replay"
        >
          Replay
        </button>
        <button
          class="px-2 h-6 text-[10px] text-text-dim hover:text-text transition-colors"
          onClick={handleExportHar}
          title="Export HAR"
        >
          HAR
        </button>
      </div>

      {/* Tab content */}
      <div class="flex-1 overflow-y-auto p-2">
        <Show when={!flow.loading} fallback={<div class="text-text-dim text-xs p-2">Loading...</div>}>
          <Show when={flow()}>
            <Switch>
              <Match when={activeTab() === 'headers'}>
                <HeadersView flow={flow()!} />
              </Match>
              <Match when={activeTab() === 'payload'}>
                <PayloadView flow={flow()!} view={payloadView()} />
              </Match>
              <Match when={activeTab() === 'timing'}>
                <TimingView flow={flow()!} />
              </Match>
              <Match when={activeTab() === 'messages'}>
                <MessagesView flow={flow()!} />
              </Match>
              <Match when={activeTab() === 'trace'}>
                <TraceView flow={flow()!} />
              </Match>
            </Switch>
          </Show>
        </Show>
      </div>

      {/* Payload view switcher (only visible on payload tab) */}
      <Show when={activeTab() === 'payload'}>
        <div class="h-6 flex items-center px-2 bg-surface border-t border-border text-[10px] shrink-0 gap-2">
          <span class="text-text-dim">View:</span>
          {(['json', 'hex', 'text'] as const).map((v) => (
            <button
              class={`px-2 py-0.5 rounded ${payloadView() === v ? 'bg-accent/20 text-accent' : 'text-text-dim hover:text-text'}`}
              onClick={() => setPayloadView(v)}
            >
              {v.toUpperCase()}
            </button>
          ))}
        </div>
      </Show>
    </div>
  );
}

function HeadersView(props: { flow: Flow }) {
  const http = (props.flow.layer as HttpLayer)?.data;
  const req = http?.request;
  const res = http?.response;

  return (
    <div class="text-xs space-y-3">
      <Show when={http?.error}>
        <div class="bg-error/10 border border-error/30 rounded p-2 text-error text-xs">{http?.error}</div>
      </Show>

      <section>
        <h3 class="text-accent text-[11px] font-bold mb-1">Request</h3>
        <div class="text-text">
          <span class="font-bold">{req?.method}</span>{' '}
          <span class="text-text-dim">{req?.url}</span>{' '}
          <span class="text-text-dim/50">{req?.version}</span>
        </div>
        <HeaderTable headers={req?.headers ?? []} />
      </section>

      <Show when={res}>
        <section>
          <h3 class="text-accent text-[11px] font-bold mb-1">Response</h3>
          <div class="text-text">
            <span class="font-bold">{res?.status}</span>{' '}
            <span class="text-text-dim">{res?.status_text}</span>{' '}
            <span class="text-text-dim/50">{res?.version}</span>
          </div>
          <HeaderTable headers={res?.headers ?? []} />
        </section>
      </Show>

      <section>
        <h3 class="text-text-dim text-[11px] font-bold mb-1">Connection</h3>
        <div class="grid grid-cols-4 gap-1 text-[11px]">
          <span class="text-text-dim">Client:</span>
          <span class="text-text">{props.flow.network.client_ip}:{props.flow.network.client_port}</span>
          <span class="text-text-dim">Server:</span>
          <span class="text-text">{props.flow.network.server_ip}:{props.flow.network.server_port}</span>
          <span class="text-text-dim">TLS:</span>
          <span class="text-text">{props.flow.network.tls ? props.flow.network.tls_version ?? 'yes' : 'no'}</span>
          <span class="text-text-dim">SNI:</span>
          <span class="text-text">{props.flow.network.sni ?? '-'}</span>
        </div>
      </section>
    </div>
  );
}

function HeaderTable(props: { headers: [string, string][] }) {
  return (
    <div class="mt-1">
      <For each={props.headers}>
        {([name, value]) => (
          <div class="flex hover:bg-hover px-1 rounded text-[11px]">
            <span class="w-48 shrink-0 text-accent/70 truncate">{name}</span>
            <span class="text-text-dim truncate">{value}</span>
          </div>
        )}
      </For>
    </div>
  );
}

function PayloadView(props: { flow: Flow; view: 'json' | 'hex' | 'text' }) {
  const http = (props.flow.layer as HttpLayer)?.data;
  const budgetExceeded = () => store.state.bodyBudgetExceeded.has(props.flow.id);

  return (
    <div class="text-xs space-y-4">
      <Show when={budgetExceeded()}>
        <div class="bg-warn/10 border border-warn/30 rounded p-2 text-warn text-xs">
          Body exceeded rule inspection budget; rules were skipped for this flow.
        </div>
      </Show>
      <Show when={http?.request?.body} fallback={<div class="text-text-dim text-xs">No request body</div>}>
        <section>
          <h3 class="text-accent text-[11px] font-bold mb-1">Request Body</h3>
          <BodyDisplay body={http!.request!.body!} view={props.view} />
        </section>
      </Show>

      <Show when={http?.response?.body}>
        <section>
          <h3 class="text-accent text-[11px] font-bold mb-1">Response Body</h3>
          <BodyDisplay body={http!.response!.body!} view={props.view} />
        </section>
      </Show>
    </div>
  );
}

function bodyBytes(data: BodyData): Uint8Array {
  if (data.encoding === 'base64') {
    try {
      const bin = atob(data.content);
      return Uint8Array.from(bin, (c) => c.charCodeAt(0));
    } catch {
      return new TextEncoder().encode(data.content);
    }
  }
  return new TextEncoder().encode(data.content);
}

function BodyDisplay(props: { body: BodyData; view: 'json' | 'hex' | 'text' }) {
  const textContent = () => {
    const bytes = bodyBytes(props.body);
    return new TextDecoder().decode(bytes);
  };

  if (props.view === 'json') {
    let formatted = textContent();
    try {
      formatted = JSON.stringify(JSON.parse(formatted), null, 2);
    } catch {}
    return <pre class="whitespace-pre-wrap break-all text-[11px] text-text font-mono">{formatted}</pre>;
  }

  if (props.view === 'hex') {
    const bytes = bodyBytes(props.body);
    const hex = Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, '0'))
      .join(' ');
    return <pre class="whitespace-pre-wrap break-all text-[11px] text-text-dim font-mono">{hex}</pre>;
  }

  return <pre class="whitespace-pre-wrap break-all text-[11px] text-text font-mono">{textContent()}</pre>;
}

function TimingView(props: { flow: Flow }) {
  const http = (props.flow.layer as HttpLayer)?.data;
  const timing = http?.response?.timing;

  return (
    <div class="text-xs">
      <Show when={timing} fallback={<div class="text-text-dim">No timing data available</div>}>
        <div class="grid grid-cols-2 gap-2">
          <div class="text-text-dim">TTFB:</div>
          <div class="text-text">{timing?.time_to_first_byte ?? '-'} ms</div>
          <div class="text-text-dim">TTLB:</div>
          <div class="text-text">{timing?.time_to_last_byte ?? '-'} ms</div>
          <div class="text-text-dim">Connect:</div>
          <div class="text-text">{timing?.connect_time_ms ?? '-'} ms</div>
          <div class="text-text-dim">SSL:</div>
          <div class="text-text">{timing?.ssl_time_ms ?? '-'} ms</div>
        </div>
      </Show>

      <Show when={props.flow.resilience_trace}>
        <h3 class="text-accent text-[11px] font-bold mt-4 mb-1">Resilience</h3>
        <div class="grid grid-cols-2 gap-2">
          <div class="text-text-dim">Budget Exceeded:</div>
          <div class="text-text">{props.flow.resilience_trace?.budget_exceeded ? 'Yes' : 'No'}</div>
          <div class="text-text-dim">Circuit Open:</div>
          <div class="text-text">{props.flow.resilience_trace?.circuit_open ? 'Yes' : 'No'}</div>
          <Show when={props.flow.resilience_trace?.timeout_type}>
            <div class="text-text-dim">Timeout:</div>
            <div class="text-warn">{props.flow.resilience_trace?.timeout_type}</div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function MessagesView(props: { flow: Flow }) {
  if (props.flow.layer.type !== 'WebSocket') {
    return <div class="text-text-dim text-xs">Not a WebSocket connection</div>;
  }
  const ws = props.flow.layer as { type: 'WebSocket'; data: { messages?: unknown[] } };
  const messages = (ws.data?.messages ?? []) as { opcode: string; direction: string; content: BodyData }[];

  return (
    <div class="text-xs">
      <For each={messages}>
        {(msg) => (
          <div class="flex items-start gap-2 py-1 border-b border-border/20 text-[11px]">
            <span class={`w-12 shrink-0 ${msg.direction === 'ClientToServer' ? 'text-accent' : 'text-warn'}`}>
              {msg.direction === 'ClientToServer' ? '→' : '←'}
            </span>
            <span class="w-12 shrink-0 text-text-dim">{msg.opcode}</span>
            <pre class="flex-1 whitespace-pre-wrap break-all text-text">{msg.content?.content?.slice(0, 500) ?? ''}</pre>
          </div>
        )}
      </For>
    </div>
  );
}

function TraceView(props: { flow: Flow }) {
  return (
    <div class="text-xs">
      <Show when={props.flow.matched_rules.length > 0} fallback={<div class="text-text-dim">No rules matched</div>}>
        <h3 class="text-accent text-[11px] font-bold mb-1">Matched Rules</h3>
        <For each={props.flow.matched_rules}>
          {(ruleId) => (
            <div class="text-text px-1 py-0.5">{ruleId}</div>
          )}
        </For>
      </Show>

      <Show when={props.flow.rule_variables && Object.keys(props.flow.rule_variables).length > 0}>
        <h3 class="text-accent text-[11px] font-bold mt-3 mb-1">Rule Variables</h3>
        <For each={Object.entries(props.flow.rule_variables)}>
          {([key, value]) => (
            <div class="flex text-[11px]">
              <span class="w-40 text-text-dim">{key}</span>
              <span class="text-text">{value}</span>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
