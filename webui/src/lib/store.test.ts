import { afterEach, describe, expect, it, vi } from 'vitest';
import { BODY_DETAIL_REFRESH_MS, createAppStore, MAX_FLOWS } from './store';
import type { FlowSummary } from '@/types/api';

function makeSummary(id: string, startTimeMs = Date.now()): FlowSummary {
  return {
    id,
    method: 'GET',
    url: `http://example.com/${id}`,
    host: 'example.com',
    path: `/${id}`,
    status: 200,
    duration_ms: 1,
    tags: [],
    start_time_ms: startTimeMs,
    has_error: false,
    is_websocket: false,
  };
}

/** A page as the API sends it: newest first, which is how `search_flows` sorts. */
function page(...entries: [string, number][]): FlowSummary[] {
  return entries.map(([id, startTimeMs]) => makeSummary(id, startTimeMs));
}

describe('createAppStore', () => {
  it('evicts oldest flows when exceeding MAX_FLOWS', () => {
    const app = createAppStore();
    for (let i = 0; i < MAX_FLOWS + 1; i++) {
      app.upsertFlow(makeSummary(`flow-${i}`));
    }
    expect(app.state.flowOrder.length).toBeLessThanOrEqual(MAX_FLOWS);
    expect(app.state.flows.has('flow-0')).toBe(false);
    expect(app.state.flows.has(`flow-${MAX_FLOWS}`)).toBe(true);
  });

  it('pins selected flow during FIFO eviction', () => {
    const app = createAppStore();
    app.upsertFlow(makeSummary('pinned'));
    for (let i = 0; i < MAX_FLOWS - 1; i++) {
      app.upsertFlow(makeSummary(`other-${i}`));
    }
    app.selectFlow('pinned');
    app.upsertFlow(makeSummary('newest'));
    expect(app.state.flows.has('pinned')).toBe(true);
  });

  it('refreshes an open flow once per burst of body events', () => {
    vi.useFakeTimers();
    const app = createAppStore();
    app.upsertFlow(makeSummary('a'));
    app.selectFlow('a');
    app.notifyHttpBody('a');
    app.notifyHttpBody('a');
    expect(app.state.flowDetailGeneration).toBe(0);
    vi.advanceTimersByTime(BODY_DETAIL_REFRESH_MS);
    expect(app.state.flowDetailGeneration).toBe(1);
  });

  it('does not refresh a flow that is not open', () => {
    vi.useFakeTimers();
    const app = createAppStore();
    app.upsertFlow(makeSummary('a'));
    app.notifyHttpBody('a');
    vi.advanceTimersByTime(BODY_DETAIL_REFRESH_MS);
    expect(app.state.flowDetailGeneration).toBe(0);
  });

  it('clearFlows resets list state', () => {
    const app = createAppStore();
    app.upsertFlow(makeSummary('a'));
    app.selectFlow('a');
    app.clearFlows();
    expect(app.state.flowOrder).toEqual([]);
    expect(app.state.flows.size).toBe(0);
    expect(app.state.selectedFlowId).toBeNull();
  });
});

/**
 * The list has one ordering rule: index 0 is the newest capture. Live events and a refresh must
 * both obey it — when they disagreed, the list showed the newest flow at the top until a refresh
 * put it at the bottom, and the next capture jumped back to the top.
 */
describe('flow order', () => {
  it('keeps the newest flow first after a sync', () => {
    const app = createAppStore();
    app.syncFlows(page(['newest', 3000], ['middle', 2000], ['oldest', 1000]));
    expect(app.state.flowOrder).toEqual(['newest', 'middle', 'oldest']);
  });

  it('orders a sync by capture time even when the page arrives out of order', () => {
    const app = createAppStore();
    app.syncFlows(page(['oldest', 1000], ['newest', 3000], ['middle', 2000]));
    expect(app.state.flowOrder).toEqual(['newest', 'middle', 'oldest']);
  });

  it('leaves the order alone across a refresh of the same captures', () => {
    const app = createAppStore();
    app.upsertFlow(makeSummary('oldest', 1000));
    app.upsertFlow(makeSummary('middle', 2000));
    app.upsertFlow(makeSummary('newest', 3000));
    const live = [...app.state.flowOrder];

    app.syncFlows(page(['newest', 3000], ['middle', 2000], ['oldest', 1000]));
    expect(app.state.flowOrder).toEqual(live);
  });

  it('puts a capture made after a refresh at the top', () => {
    const app = createAppStore();
    app.syncFlows(page(['b', 2000], ['a', 1000]));
    app.upsertFlow(makeSummary('c', 3000));
    expect(app.state.flowOrder).toEqual(['c', 'b', 'a']);
  });

  it('keeps a flow captured while the page was being fetched', () => {
    const app = createAppStore();
    app.upsertFlow(makeSummary('captured-during-sync', 5000));
    app.syncFlows(page(['b', 2000], ['a', 1000]));
    expect(app.state.flowOrder).toEqual(['captured-during-sync', 'b', 'a']);
  });

  it('drops the oldest flows when a sync brings more than the cap allows', () => {
    const app = createAppStore();
    const items: [string, number][] = [];
    for (let i = 0; i < MAX_FLOWS + 10; i++) {
      items.push([`flow-${i}`, 10_000_000 - i]);
    }
    app.syncFlows(page(...items));

    expect(app.state.flowOrder.length).toBe(MAX_FLOWS);
    expect(app.state.flows.has('flow-0')).toBe(true);
    expect(app.state.flows.has(`flow-${MAX_FLOWS + 9}`)).toBe(false);
  });
});

afterEach(() => {
  vi.useRealTimers();
});
