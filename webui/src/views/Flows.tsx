import { createMemo, For, Show, createSignal, onMount, onCleanup } from 'solid-js';
import { store } from '@/lib/store';
import { filterFlows, orderedFlows } from '@/lib/flowFilter';
import { formatDurationMs, formatFlowTime } from '@/lib/format';
import { copyCurlForFlow, replayFlowById } from '@/lib/flowActions';
import { isEditingTarget } from '@/lib/editing';
import type { FlowSummary } from '@/types/api';
import FlowDetail from './FlowDetail';

const ROW_HEIGHT = 28;
const VISIBLE_BUFFER = 10;

export default function FlowsView() {
  const [scrollTop, setScrollTop] = createSignal(0);
  const [containerHeight, setContainerHeight] = createSignal(600);
  let containerRef!: HTMLDivElement;

  const filteredFlows = createMemo(() =>
    filterFlows(orderedFlows(store.state.flowOrder, store.state.flows), store.state.filterText),
  );

  const visibleFlows = createMemo(() => {
    const items = filteredFlows();
    const st = scrollTop();
    const ch = containerHeight();
    const start = Math.max(0, Math.floor(st / ROW_HEIGHT) - VISIBLE_BUFFER);
    const end = Math.min(items.length, Math.ceil((st + ch) / ROW_HEIGHT) + VISIBLE_BUFFER);
    const visible = items.slice(start, end);
    return { items: visible, startIndex: start, total: items.length, totalHeight: items.length * ROW_HEIGHT };
  });

  function scrollToIndex(index: number) {
    if (!containerRef) return;
    const top = index * ROW_HEIGHT;
    containerRef.scrollTop = top;
    setScrollTop(top);
  }

  function selectByOffset(offset: number) {
    const items = filteredFlows();
    if (items.length === 0) return;

    const currentId = store.state.selectedFlowId;
    let index = currentId ? items.findIndex((f) => f.id === currentId) : -1;
    if (index < 0) index = offset > 0 ? -1 : items.length;

    index = Math.max(0, Math.min(items.length - 1, index + offset));
    store.selectFlow(items[index].id);
    scrollToIndex(index);
  }

  function selectAbsolute(index: number) {
    const items = filteredFlows();
    if (items.length === 0) return;
    const i = Math.max(0, Math.min(items.length - 1, index));
    store.selectFlow(items[i].id);
    scrollToIndex(i);
  }

  function focusFilter() {
    document.getElementById('flow-filter')?.focus();
  }

  function handleFlowsKeyDown(e: KeyboardEvent) {
    if (store.state.activeView !== 'flows') return;
    if (isEditingTarget(e.target)) return;

    switch (e.key) {
      case 'j':
      case 'ArrowDown':
        e.preventDefault();
        selectByOffset(1);
        break;
      case 'k':
      case 'ArrowUp':
        e.preventDefault();
        selectByOffset(-1);
        break;
      case 'g':
        e.preventDefault();
        selectAbsolute(0);
        break;
      case 'G':
      case 'End':
        e.preventDefault();
        selectAbsolute(filteredFlows().length - 1);
        break;
      case 'Home':
        e.preventDefault();
        selectAbsolute(0);
        break;
      case '/':
        e.preventDefault();
        focusFilter();
        break;
      case 'Enter':
        if (!store.state.selectedFlowId && filteredFlows().length > 0) {
          e.preventDefault();
          selectAbsolute(0);
        }
        break;
      case 'r':
        if (store.state.selectedFlowId) {
          e.preventDefault();
          void replayFlowById(store.state.selectedFlowId);
        }
        break;
      case 'c':
        if (store.state.selectedFlowId) {
          e.preventDefault();
          void copyCurlForFlow(store.state.selectedFlowId);
        }
        break;
      case 'x':
        e.preventDefault();
        store.clearFlows();
        break;
    }
  }

  onMount(() => {
    document.addEventListener('keydown', handleFlowsKeyDown);
    if (containerRef) {
      setContainerHeight(containerRef.clientHeight);
    }
  });

  onCleanup(() => {
    document.removeEventListener('keydown', handleFlowsKeyDown);
  });

  function handleScroll() {
    setScrollTop(containerRef.scrollTop);
  }

  function handleResize() {
    setContainerHeight(containerRef.clientHeight);
  }

  function statusClass(status: number | null): string {
    if (status === null) return 'text-text-dim';
    if (status < 300) return 'text-success';
    if (status < 400) return 'text-info';
    if (status < 500) return 'text-warn';
    return 'text-error';
  }

  function methodClass(method: string): string {
    switch (method.toUpperCase()) {
      case 'GET':
        return 'text-success';
      case 'POST':
        return 'text-accent';
      case 'PUT':
        return 'text-info';
      case 'DELETE':
        return 'text-error';
      case 'PATCH':
        return 'text-warn';
      default:
        return 'text-text-dim';
    }
  }

  return (
    <div class="h-full flex">
      <div class="w-[45%] min-w-[300px] flex flex-col border-r border-border">
        <div
          ref={containerRef}
          class="flex-1 overflow-y-auto font-mono"
          onScroll={handleScroll}
          onResize={handleResize}
        >
          <div style={{ height: `${visibleFlows().totalHeight}px`, position: 'relative' }}>
            <For each={visibleFlows().items}>
              {(flow, idx) => {
                const i = () => visibleFlows().startIndex + idx();
                const isSelected = () => store.state.selectedFlowId === flow.id;
                return (
                  <div
                    class={`absolute left-0 right-0 flex items-center h-[28px] px-2 cursor-pointer text-xs border-b border-border/30 transition-colors ${
                      isSelected() ? 'bg-accent/15 text-text' : 'hover:bg-hover text-text-dim'
                    } ${i() % 2 === 0 ? '' : 'bg-surface/50'}`}
                    style={{ top: `${i() * ROW_HEIGHT}px` }}
                    onClick={() => store.selectFlow(flow.id)}
                  >
                    <span class="w-14 shrink-0 text-[10px] text-text-dim/50">
                      {formatFlowTime(flow.start_time_ms)}
                    </span>
                    <span class={`w-14 shrink-0 font-bold ${methodClass(flow.method)}`}>
                      {flow.method}
                    </span>
                    <span class={`w-10 shrink-0 font-bold ${statusClass(flow.status)}`}>
                      {flow.status ?? '---'}
                    </span>
                    <span class="flex-1 truncate">
                      {flow.host}
                      <span class="text-text-dim/50">{flow.path}</span>
                    </span>
                    <span class="w-14 shrink-0 text-right text-[10px] text-text-dim/40">
                      {flow.duration_ms != null ? formatDurationMs(flow.duration_ms) : ''}
                    </span>
                    {flow.has_error && <span class="ml-1 text-error text-[10px]">ERR</span>}
                    {flow.is_websocket && <span class="ml-1 text-info text-[10px]">WS</span>}
                  </div>
                );
              }}
            </For>
          </div>
          <Show when={filteredFlows().length === 0}>
            <div class="flex items-center justify-center h-full text-text-dim text-sm">
              {store.state.sseConnected
                ? 'No flows captured yet. Start browsing through the proxy.'
                : 'Connecting...'}
            </div>
          </Show>
        </div>
        <div class="h-5 flex items-center px-2 bg-surface border-t border-border text-[10px] text-text-dim/50">
          {filteredFlows().length} flows · <span class="ml-1 opacity-60">j/k navigate · / filter · r replay · c cURL</span>
        </div>
      </div>

      <div class="flex-1 flex flex-col min-w-0">
        <Show when={store.state.selectedFlowId} fallback={<EmptyDetail />}>
          <FlowDetail flowId={store.state.selectedFlowId!} />
        </Show>
      </div>
    </div>
  );
}

function EmptyDetail() {
  return (
    <div class="flex-1 flex items-center justify-center text-text-dim text-sm">
      Select a flow to inspect
    </div>
  );
}
