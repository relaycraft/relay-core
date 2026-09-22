import { afterEach, describe, expect, it, vi } from 'vitest';
import { BODY_DETAIL_REFRESH_MS, createAppStore, MAX_FLOWS } from './store';
import type { FlowSummary } from '@/types/api';

function makeSummary(id: string): FlowSummary {
  return {
    id,
    method: 'GET',
    url: `http://example.com/${id}`,
    host: 'example.com',
    path: `/${id}`,
    status: 200,
    duration_ms: 1,
    tags: [],
    start_time_ms: Date.now(),
    has_error: false,
    is_websocket: false,
  };
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

afterEach(() => {
  vi.useRealTimers();
});
