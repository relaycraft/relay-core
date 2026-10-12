import { createSignal, createResource, createMemo, createEffect, on, Show, For, Switch, Match, ErrorBoundary } from 'solid-js';
import { getFlow, replayFlow, exportFlowHar } from '@/lib/api';
import { bodyBytes, decodeBodyText } from '@/lib/bodyText';
import { buildCurlCommand } from '@/lib/flowActions';
import { flowTrace } from '@/lib/flowTrace';
import { grpcOutcome } from '@/lib/flowSummary';
import { store } from '@/lib/store';
import type { Flow, HttpLayer, HttpResponse, BodyData } from '@/types/api';
import { parseSseEvent, type SseEventRecord } from '@/lib/sseEvents';

type DetailTab = 'headers' | 'payload' | 'timing' | 'messages' | 'trace';

/** What a body is, from its content type. */
type MediaKind = 'image' | 'pdf' | 'audio' | 'video' | 'json' | 'text' | 'binary';

function mediaKind(contentType: string): MediaKind {
  const type = contentType.toLowerCase();
  if (type.startsWith('image/')) return 'image';
  if (type.includes('application/pdf')) return 'pdf';
  if (type.startsWith('audio/')) return 'audio';
  if (type.startsWith('video/')) return 'video';
  if (type.includes('json')) return 'json';
  if (
    type.startsWith('text/') ||
    type.includes('xml') ||
    type.includes('javascript') ||
    type.includes('x-www-form-urlencoded')
  ) {
    return 'text';
  }
  return 'binary';
}

/** Kinds the browser can render directly, so a body is shown rather than described. */
const RENDERABLE: ReadonlySet<MediaKind> = new Set<MediaKind>(['image', 'pdf', 'audio', 'video']);

/**
 * Which view to open on, when the reader has not chosen one.
 *
 * Text and JSON get their readable view; everything else gets hex. Opening a PNG on a "JSON" view only
 * ever produced mojibake, which is what made binary responses look broken rather than binary.
 */
function defaultViewFor(contentType: string): 'json' | 'hex' | 'text' {
  const kind = mediaKind(contentType);
  if (kind === 'json') return 'json';
  if (kind === 'text') return 'text';
  return 'hex';
}

/** A header's value, matched case-insensitively as HTTP requires. */
function headerValue(headers: [string, string][] | undefined, name: string): string {
  return headers?.find(([key]) => key.toLowerCase() === name)?.[1] ?? '';
}

/** Bytes a person can read: 1536 -> "1.5 KiB". */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return String(bytes);
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KiB', 'MiB', 'GiB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || Number.isInteger(value) ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export default function FlowDetail(props: { flowId: string }) {
  const [activeTab, setActiveTab] = createSignal<DetailTab>('headers');
  // Null until the reader picks a view: the default follows the body's content type, so an image or a
  // binary response does not open on a "JSON" view that can only show mojibake.
  const [payloadView, setPayloadView] = createSignal<'json' | 'hex' | 'text' | null>(null);

  const [flow] = createResource(
    () => ({ id: props.flowId, gen: store.state.flowDetailGeneration }),
    ({ id }) => getFlow(id),
  );

  /** The flow for this selection. A refresh keeps the previous value, so an id check avoids
   *  showing another connection while the new one is still loading. */
  const loadedFlow = () => {
    const current = flow();
    return current && current.id === props.flowId ? current : undefined;
  };

  // Which body this pane is about — the response when there is one, since that is what "Payload"
  // usually means to someone inspecting traffic.
  const payloadResponse = () => (flow()?.layer as HttpLayer)?.data?.response;

  /** The view in effect: the reader's pick, or what the body's content type calls for. */
  const effectiveView = (): 'json' | 'hex' | 'text' => {
    const chosen = payloadView();
    if (chosen) return chosen;
    const http = (flow()?.layer as HttpLayer)?.data;
    const contentType =
      headerValue(http?.response?.headers, 'content-type') ||
      headerValue(http?.request?.headers, 'content-type');
    return defaultViewFor(contentType);
  };

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

  /**
   * Retry a view that threw, as soon as the reader moves to another tab or another flow.
   *
   * The boundary below keeps a failure inside the pane, but a fallback that outlives its cause is its
   * own kind of freeze: the reader switches tabs and nothing happens. This makes the retry automatic.
   */
  let retryView: (() => void) | undefined;
  createEffect(on(() => [activeTab(), props.flowId], () => retryView?.(), { defer: true }));

  /**
   * Forget the picked body view when the reader selects another flow.
   *
   * The view is a statement about *this* body — "the text view is enough" or "show me the hex" — and
   * the default is derived from the new body's content type precisely so that an image does not open
   * on a JSON view that can only render mojibake. Keeping the previous pick across a selection
   * defeats that: pick TEXT on a JSON response, select a binary one, and the pane decodes the bytes
   * as UTF-8 even though it would have opened on HEX by itself.
   */
  createEffect(on(() => props.flowId, () => setPayloadView(null), { defer: true }));

  return (
    <div class="h-full flex flex-col">
      {/* Tab bar */}
      <div class="h-7 flex items-center bg-surface border-b border-border px-1 shrink-0">
        <For each={tabs}>
          {(tab) => (
            <button
              // The underline is unconditional and only changes colour, so the row does not shift as
              // the active tab moves; and it is 2px because a 1px accent line on a dark strip was
              // almost invisible, which made it unclear which tab was open.
              class={`px-3 h-full text-[13px] border-b-2 transition-colors ${
                activeTab() === tab
                  ? 'text-accent border-accent font-semibold'
                  : 'text-text-dim border-transparent hover:text-text'
              }`}
              aria-current={activeTab() === tab ? 'page' : undefined}
              onClick={() => setActiveTab(tab)}
            >
              {tab.charAt(0).toUpperCase() + tab.slice(1)}
            </button>
          )}
        </For>
        <div class="flex-1" />
        <div class="flex items-center gap-0.5 pr-0.5">
          <button
            class="px-2 h-6 text-[12px] text-text-dim hover:text-text hover:bg-hover rounded transition-colors"
            onClick={copyCurl}
            title="Copy as cURL"
          >
            cURL
          </button>
          <button
            class="px-2 h-6 text-[12px] text-text-dim hover:text-text hover:bg-hover rounded transition-colors"
            onClick={handleReplay}
            title="Replay"
          >
            Replay
          </button>
          <button
            class="px-2 h-6 text-[12px] text-text-dim hover:text-text hover:bg-hover rounded transition-colors"
            onClick={handleExportHar}
            title="Export HAR"
          >
            HAR
          </button>
        </div>
      </div>

      {/* Tab content */}
      <div class="flex-1 overflow-y-auto p-2">
        {/* `loading` is also true while a body event refreshes a flow that is already on screen.
            Gating the pane on it replaced the detail with "Loading..." for the whole connection. */}
        <Show
          when={loadedFlow()}
          fallback={
            <Show
              when={flow.error}
              fallback={<div class="text-text-dim text-xs p-2">Loading...</div>}
            >
              <div class="text-warn text-xs p-2">Could not load this flow.</div>
            </Show>
          }
        >
          {(current) => (
            /**
             * One view's fault stays in that view.
             *
             * The Trace tab threw while reading a field the API omits when it is empty, and because
             * the throw happened *during* a Solid update it aborted the update queue: from then on
             * every tab click in this pane did nothing. A reader cannot tell that apart from the UI
             * freezing, so the failure is contained and reported here instead.
             */
            <ErrorBoundary
              fallback={(error, reset) => {
                retryView = reset;
                return (
                  <div class="p-2 space-y-1 text-xs">
                    <div class="text-error">This view could not be rendered.</div>
                    <div class="text-text-dim break-all">
                      {error instanceof Error ? error.message : String(error)}
                    </div>
                    <button
                      class="px-2 py-0.5 rounded border border-border text-text-dim hover:text-text hover:bg-hover"
                      onClick={() => reset()}
                    >
                      Try again
                    </button>
                  </div>
                );
              }}
            >
              <Switch>
                <Match when={activeTab() === 'headers'}>
                  <HeadersView flow={current()} />
                </Match>
                <Match when={activeTab() === 'payload'}>
                  <PayloadView flow={current()} view={effectiveView()} />
                </Match>
                <Match when={activeTab() === 'timing'}>
                  <TimingView flow={current()} />
                </Match>
                <Match when={activeTab() === 'messages'}>
                  <MessagesView flow={current()} />
                </Match>
                <Match when={activeTab() === 'trace'}>
                  <TraceView flow={current()} />
                </Match>
              </Switch>
            </ErrorBoundary>
          )}
        </Show>
      </div>

      {/* Payload view switcher (only visible on payload tab) */}
      <Show when={activeTab() === 'payload'}>
        <div class="h-6 flex items-center px-2 bg-surface border-t border-border text-[12px] shrink-0 gap-2">
          <span class="text-text-dim">View:</span>
          {(['json', 'hex', 'text'] as const).map((v) => (
            <button
              class={`px-2 py-0.5 rounded ${effectiveView() === v ? 'bg-accent/20 text-accent' : 'text-text-dim hover:text-text'}`}
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
  /**
   * Read through the props on every access rather than destructuring once.
   *
   * A view is created when the reader selects a row and lives until they select another, so a
   * destructured `const res = http?.response` is a snapshot of the flow as it was at that moment: the
   * response section never appeared for a flow opened while it was still in flight, and the timing
   * rows stayed empty after the answer arrived. Nothing here may be captured outside a thunk.
   */
  const http = () => (props.flow.layer as HttpLayer)?.data;
  const req = () => http()?.request;
  const res = () => http()?.response;

  /**
   * The gRPC outcome, through the shared reader (`lib/flowSummary.ts`).
   *
   * This used to parse the trailers itself with `parseInt` and `decodeURIComponent`, which read `14x`
   * as 14 and threw on a `%FF`, so the pane could show a gRPC status the badge beside it did not.
   */
  const grpc = () => grpcOutcome(res()?.trailers);
  const grpcStatus = () => grpc().status;
  const grpcMessage = () => grpc().message ?? '';

  const reasonPhrase = () => {
    const response = res();
    const text = response?.status_text ?? '';
    const code = response?.status != null ? String(response.status) : '';
    if (code && text.startsWith(code)) return text.slice(code.length).trim();
    return text;
  };

  /** An endpoint, or `unknown` when the flow never learned one. */
  const endpoint = (ip?: string, port?: number) => {
    const isPlaceholder = !ip || ip === '0.0.0.0' || ip === '::' || port === 0 || port == null;
    return isPlaceholder ? 'unknown' : `${ip}:${port}`;
  };

  return (
    <div class="space-y-3 text-[13px]">
      <Show when={http()?.error}>
        <div class="bg-error/10 border border-error/30 rounded p-2 text-error text-xs">
          {http()?.error}
        </div>
      </Show>

      <section class="rounded-md border border-border/60 overflow-hidden">
        <h3 class="px-2 py-1 text-[12px] font-bold uppercase tracking-wide text-accent bg-accent/10 border-b border-border/60">
          Request
        </h3>
        <div class="px-2 py-1.5">
          <span class="font-bold">{req()?.method}</span>{' '}
          <span class="text-text-dim break-all">{req()?.url}</span>{' '}
          <span class="text-text-dim/60">{req()?.version}</span>
        </div>
        <HeaderTable headers={req()?.headers ?? []} />
      </section>

      <Show when={res()}>
        <section class="rounded-md border border-border/60 overflow-hidden">
          <h3 class="px-2 py-1 text-[12px] font-bold uppercase tracking-wide text-accent bg-accent/10 border-b border-border/60">
            Response
          </h3>
          <div class="px-2 py-1.5">
            <span class="font-bold">{res()?.status}</span>{' '}
            <span class="text-text-dim">{reasonPhrase()}</span>{' '}
            <span class="text-text-dim/60">{res()?.version}</span>
            <Show when={grpcStatus() != null}>
              <span
                class={`ml-2 font-mono ${grpcStatus() === 0 ? 'text-text-dim' : 'text-error'}`}
              >
                grpc-status {grpcStatus()}
                <Show when={grpcMessage()}>{(msg) => <span> {msg()}</span>}</Show>
              </span>
            </Show>
          </div>
          <HeaderTable headers={res()?.headers ?? []} />
          <Show when={(res()?.trailers?.length ?? 0) > 0}>
            <div class="px-2 py-1 text-[12px] text-text-dim border-t border-border/60">Trailers</div>
            <HeaderTable headers={res()?.trailers ?? []} />
          </Show>
        </section>
      </Show>

      <section class="rounded-md border border-border/60 overflow-hidden">
        <h3 class="px-2 py-1 text-[12px] font-bold uppercase tracking-wide text-text-dim bg-surface-alt border-b border-border/60">
          Connection
        </h3>
        <div class="grid grid-cols-4 gap-x-2 gap-y-1 p-2 text-[13px]">
          <span class="text-text-dim">Client:</span>
          <span class="text-text"
            >{endpoint(props.flow.network.client_ip, props.flow.network.client_port)}</span
          >
          <span class="text-text-dim">Server:</span>
          {/* The target the request aimed at, which is known; the resolved peer address is not, for a
              forward-proxy flow, so it is only a fallback. */}
          <span class="text-text"
            >{props.flow.network.server_host ??
              endpoint(props.flow.network.server_ip, props.flow.network.server_port)}</span
          >
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
    <Show
      when={props.headers.length > 0}
      fallback={<div class="px-2 py-1.5 text-[13px] text-text-dim/70">No headers recorded.</div>}
    >
      <div class="py-1">
        <For each={props.headers}>
          {([name, value]) => (
            <div class="flex gap-2 hover:bg-hover px-2 py-0.5 text-[13px]">
              <span class="w-56 shrink-0 text-text-dim">{name}</span>
              <span class="min-w-0 flex-1 text-text break-all">{value}</span>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

function PayloadView(props: { flow: Flow; view: 'json' | 'hex' | 'text' }) {
  // Read reactively: a body that streams in after the row was selected arrives as a new flow, and a
  // captured `http` here would keep showing "No request body" for it.
  const http = () => (props.flow.layer as HttpLayer)?.data;
  const budgetExceeded = () => store.state.bodyBudgetExceeded.has(props.flow.id);

  return (
    <div class="text-xs space-y-4">
      <Show when={budgetExceeded()}>
        <div class="bg-warn/10 border border-warn/30 rounded p-2 text-warn text-xs">
          Body exceeded rule inspection budget; rules were skipped for this flow.
        </div>
      </Show>
      <Show
        when={http()?.request?.body}
        fallback={<div class="text-text-dim text-xs">No request body</div>}
      >
        {(body) => (
          <section>
            <h3 class="text-accent text-[13px] font-bold mb-1">Request Body</h3>
            <BodyDisplay
              body={body()}
              view={props.view}
              contentType={headerValue(http()?.request?.headers, 'content-type')}
            />
          </section>
        )}
      </Show>

      <Show when={http()?.response?.body}>
        {(body) => (
          <section>
            <h3 class="text-accent text-[13px] font-bold mb-1">Response Body</h3>
            <BodyDisplay
              body={body()}
              view={props.view}
              contentType={headerValue(http()?.response?.headers, 'content-type')}
            />
          </section>
        )}
      </Show>
    </div>
  );
}

/** Bytes the hex view will lay out; beyond this it is noise rather than information. */
const HEX_PREVIEW_BYTES = 4096;

/**
 * Render one body, according to what it is.
 *
 * A body that is not text is not shown as text: an image, a PDF, audio or video is displayed, and
 * anything else binary opens on a hex view with its type and size stated. Previously every body was
 * decoded as UTF-8 and printed, so an image or a protobuf payload appeared as mojibake and the honest
 * conclusion from the pane was that the capture was broken.
 */
function BodyDisplay(props: { body: BodyData; view: 'json' | 'hex' | 'text'; contentType: string }) {
  const kind = () => mediaKind(props.contentType);
  const renderable = () => RENDERABLE.has(kind());

  const textContent = () => decodeBodyText(props.body, props.contentType);

  /** The body as a data URL, for the tags that render it. Binary arrives base64 already. */
  const dataUrl = () =>
    props.body.encoding === 'base64'
      ? `data:${props.contentType || 'application/octet-stream'};base64,${props.body.content}`
      : `data:${props.contentType || 'text/plain'},${encodeURIComponent(props.body.content)}`;

  const hexPreview = () => {
    const bytes = bodyBytes(props.body);
    const shown = bytes.subarray(0, HEX_PREVIEW_BYTES);
    const hex = Array.from(shown)
      .map((b) => b.toString(16).padStart(2, '0'))
      .join(' ');
    return { hex, shown: shown.length, total: bytes.length };
  };

  return (
    <div class="space-y-2">
      {/* What it is, so the pane says something even when it cannot draw the body. */}
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[12px] text-text-dim">
        <span class="font-mono text-text">{props.contentType || 'no content-type'}</span>
        <span>{formatBytes(props.body.size)}</span>
        <span>stored as {props.body.encoding}</span>
        <Show when={props.body.grpc}>
          <span class="text-accent">
            gRPC · {props.body.grpc!.messages.length} message
            {props.body.grpc!.messages.length === 1 ? '' : 's'}
            <Show when={props.body.grpc!.unparsed_bytes > 0}>
              {' '}
              · {props.body.grpc!.unparsed_bytes} bytes still open
            </Show>
          </span>
        </Show>
      </div>
      <Show when={props.body.grpc?.messages.some((message) => message.text)}>
        <div class="space-y-1">
          <For each={props.body.grpc!.messages.filter((message) => message.text != null)}>
            {(message) => (
              <pre class="rounded-md border border-border/60 bg-surface-alt p-2 text-[12px] whitespace-pre-wrap break-all">
                <span class="text-text-dim">#{message.index}</span> {message.text}
              </pre>
            )}
          </For>
        </div>
      </Show>

      {/* Rendered, not described: the browser can draw all of these. */}
      <Show when={renderable()}>
        <div class="rounded-md border border-border/60 overflow-hidden">
          <div class="px-2 py-1 text-[12px] text-text-dim bg-surface-alt border-b border-border/60">
            Rendered preview
          </div>
          <div class="p-2">
            <Show when={kind() === 'image'}>
              <img
                src={dataUrl()}
                alt="Captured image body"
                class="max-w-full max-h-[420px] rounded border border-border/40"
              />
            </Show>
            <Show when={kind() === 'pdf'}>
              <iframe src={dataUrl()} title="Captured PDF body" class="w-full h-[420px] rounded" />
            </Show>
            <Show when={kind() === 'audio'}>
              <audio controls src={dataUrl()} class="w-full" />
            </Show>
            <Show when={kind() === 'video'}>
              <video controls src={dataUrl()} class="max-w-full max-h-[420px] rounded" />
            </Show>
          </div>
        </div>
      </Show>

      {/* A binary body has no text view worth showing; say so rather than printing mojibake. */}
      <Show when={kind() === 'binary' && props.view === 'text'}>
        <div class="rounded border border-border/60 bg-surface-alt/40 px-2 py-1.5 text-[12px] text-text-dim">
          This body is binary ({props.contentType || 'no content-type'}). The text view decodes it as
          UTF-8 and is not meaningful; use HEX.
        </div>
      </Show>

      <Show when={props.view === 'json'}>
        {(() => {
          let formatted = textContent();
          try {
            formatted = JSON.stringify(JSON.parse(formatted), null, 2);
          } catch {}
          return (
            <pre class="whitespace-pre-wrap break-all text-[13px] text-text font-mono">{formatted}</pre>
          );
        })()}
      </Show>

      <Show when={props.view === 'hex'}>
        <div>
          <pre class="whitespace-pre-wrap break-all text-[13px] text-text-dim font-mono">
            {hexPreview().hex}
          </pre>
          <Show when={hexPreview().total > hexPreview().shown}>
            <div class="mt-1 text-[12px] text-text-dim">
              Showing the first {formatBytes(hexPreview().shown)} of {formatBytes(hexPreview().total)}.
            </div>
          </Show>
        </div>
      </Show>

      <Show when={props.view === 'text' && kind() !== 'binary'}>
        <pre class="whitespace-pre-wrap break-all text-[13px] text-text font-mono">{textContent()}</pre>
      </Show>
    </div>
  );
}

function TimingView(props: { flow: Flow }) {
  // Reactive for the same reason as the headers: the timing only exists once the response does, which
  // is usually after the row was selected.
  const timing = () => (props.flow.layer as HttpLayer)?.data?.response?.timing;

  return (
    <div class="text-xs">
      <Show when={timing()} fallback={<div class="text-text-dim">No timing data available</div>}>
        {(value) => (
          <div class="grid grid-cols-2 gap-2">
            <div class="text-text-dim">TTFB:</div>
            <div class="text-text">{value().time_to_first_byte ?? '-'} ms</div>
            <div class="text-text-dim">TTLB:</div>
            <div class="text-text">{value().time_to_last_byte ?? '-'} ms</div>
            <div class="text-text-dim">Connect:</div>
            <div class="text-text">{value().connect_time_ms ?? '-'} ms</div>
            <div class="text-text-dim">SSL:</div>
            <div class="text-text">{value().ssl_time_ms ?? '-'} ms</div>
          </div>
        )}
      </Show>

      <Show when={props.flow.resilience_trace}>
        <h3 class="text-accent text-[13px] font-bold mt-4 mb-1">Resilience</h3>
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
  // The layer check is reactive too: an early `return` in a component body is a decision made once, at
  // mount, and this pane has to follow the flow it was opened on. A WebSocket's messages arrive over
  // the whole life of the connection, which is the case this view exists for.
  type Message = { opcode: string; direction: string; content: BodyData };
  const messages = (): Message[] =>
    props.flow.layer.type === 'WebSocket'
      ? (((props.flow.layer as { data: { messages?: unknown[] } }).data?.messages ?? []) as Message[])
      : [];

  // A proxied event stream is the other thing that produces messages, and it has two sources, for
  // two different questions. While the stream is *open* the engine pushes each event as it parses it
  // (an SSE response may never end, so waiting for the body would mean showing nothing), and the
  // store accumulates those. Once it has *ended*, the events are recorded on the response, which is
  // what makes a capture opened afterwards readable — the live list is empty by then, and an empty
  // list that means "not recorded" would be indistinguishable from "nothing happened".
  //
  // So the recorded list wins when it exists: it covers the whole stream, and the live list at that
  // point is a subset of it.
  const recordedEvents = createMemo<SseEventRecord[]>(() => {
    const layer = props.flow.layer as { data?: { response?: HttpResponse } };
    const raw = props.flow.layer.type === 'Http' ? (layer.data?.response?.sse_events ?? []) : [];
    // Through the same validator as the live path: one shape for rendering, and the API's payload is
    // checked rather than asserted, exactly as the frame's is.
    return raw
      .map(parseSseEvent)
      .filter((event): event is SseEventRecord => event !== null);
  });
  const events = createMemo(() => {
    const recorded = recordedEvents();
    return recorded.length > 0 ? [...recorded].reverse() : store.sseEventsFor(props.flow.id);
  });
  /** Whether anything at all is known about this stream's events. */
  const eventsRecorded = createMemo(() => recordedEvents().length > 0);
  const isEventStream = createMemo(
    () =>
      events().length > 0 ||
      // An event stream that has not emitted yet is still an event stream: saying "not a WebSocket"
      // about it would be true and useless.
      (props.flow.layer.type === 'Http' &&
        ((props.flow.layer as { data: { response?: HttpResponse } }).data?.response?.headers ?? []).some(
          ([name, value]) =>
            name.toLowerCase() === 'content-type' &&
            value.split(';')[0].trim().toLowerCase() === 'text/event-stream',
        )),
  );

  return (
    <Show
      when={props.flow.layer.type === 'WebSocket' || isEventStream()}
      fallback={
        <div class="text-text-dim text-xs">
          No messages: not a WebSocket connection, and no event stream was observed.
        </div>
      }
    >
      <Show when={props.flow.layer.type === 'WebSocket'}>
        <div class="text-xs">
          <For each={messages()}>
            {(msg) => (
              <div class="flex items-start gap-2 py-1 border-b border-border/20 text-[13px]">
                <span
                  class={`w-12 shrink-0 ${msg.direction === 'ClientToServer' ? 'text-accent' : 'text-warn'}`}
                >
                  {msg.direction === 'ClientToServer' ? '→' : '←'}
                </span>
                <span class="w-12 shrink-0 text-text-dim">{msg.opcode}</span>
                <pre class="flex-1 whitespace-pre-wrap break-all text-text">{msg.content?.content?.slice(0, 500) ?? ''}</pre>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={props.flow.layer.type !== 'WebSocket' && isEventStream()}>
        <div class="text-xs">
          <div class="text-text-dim mb-1">
            {events().length} event{events().length === 1 ? '' : 's'}
            {eventsRecorded()
              ? ', newest first'
              : events().length > 0
                ? ', newest first, live'
                : ' so far'}
          </div>
          <Show
            when={events().length > 0}
            fallback={
              // The honest distinction: a stream that is still open has genuinely emitted nothing
              // yet, while a finished one whose events were not recorded is a gap in the capture —
              // and saying "no event yet" about the second would be a lie.
              <div class="text-text-dim">
                {props.flow.end_time
                  ? 'This event stream ended without any event being recorded.'
                  : 'The response is an event stream, and no event has been emitted yet.'}
              </div>
            }
          >
            <For each={events()}>
              {(event) => (
                <div class="flex items-start gap-2 py-1 border-b border-border/20 text-[13px]">
                  {/* Always the server's direction: an event stream only travels one way, and the
                      engine parses only the response direction. */}
                  <span class="w-12 shrink-0 text-warn">←</span>
                  <span class="w-20 shrink-0 text-text-dim truncate" title={event.kind}>
                    {event.kind}
                  </span>
                  <pre class="flex-1 whitespace-pre-wrap break-all text-text">
                    {event.data.slice(0, 500)}
                  </pre>
                  <Show when={event.id}>
                    <span class="shrink-0 text-text-dim" title="last-event-id">
                      #{event.id}
                    </span>
                  </Show>
                </div>
              )}
            </For>
          </Show>
        </div>
      </Show>
    </Show>
  );
}

function TraceView(props: { flow: Flow }) {
  // The typed lifecycle events for this flow, newest first. This is the answer to "who changed this,
  // and why": `MutationApplied` carries the actor and the field names, and until this list existed
  // the interface had no way to show either.
  const notices = createMemo(() => store.noticesFor(props.flow.id));

  // Read through `flowTrace` rather than off the flow: the API omits `matched_rules` and
  // `rule_variables` entirely when they are empty, and reading them directly threw on every flow no
  // rule had touched. See `lib/flowTrace.ts`.
  const trace = createMemo(() => flowTrace(props.flow));

  return (
    <div class="text-xs">
      <Show when={notices().length > 0}>
        <h3 class="text-accent text-[13px] font-bold mb-1">Lifecycle</h3>
        <For each={notices()}>
          {(notice) => (
            <div class="flex gap-2 px-1 py-0.5">
              <span class="text-text-dim w-20 shrink-0">{notice.kind}</span>
              <span class="text-text">{notice.summary}</span>
            </div>
          )}
        </For>
      </Show>

      <Show
        when={trace().matchedRules.length > 0}
        fallback={
          <div class="text-text-dim">
            {notices().length > 0 ? '' : 'No rules matched'}
          </div>
        }
      >
        <h3 class="text-accent text-[13px] font-bold mt-3 mb-1">Matched Rules</h3>
        <For each={trace().matchedRules}>
          {(ruleId) => (
            <div class="text-text px-1 py-0.5">{ruleId}</div>
          )}
        </For>
      </Show>

      <Show when={trace().ruleVariables.length > 0}>
        <h3 class="text-accent text-[13px] font-bold mt-3 mb-1">Rule Variables</h3>
        <For each={trace().ruleVariables}>
          {([key, value]) => (
            <div class="flex text-[13px]">
              <span class="w-40 text-text-dim">{key}</span>
              <span class="text-text">{value}</span>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
