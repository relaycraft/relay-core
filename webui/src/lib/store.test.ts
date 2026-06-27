import { describe, expect, it } from 'vitest';
import { createAppStore, MAX_FLOWS } from './store';
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
