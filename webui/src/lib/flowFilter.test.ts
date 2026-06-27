import { describe, expect, it } from 'vitest';
import { filterFlows } from './flowFilter';
import type { FlowSummary } from '@/types/api';

function summary(overrides: Partial<FlowSummary> & { id: string }): FlowSummary {
  return {
    method: 'GET',
    url: 'http://api.example.com/v1',
    host: 'api.example.com',
    path: '/v1',
    status: 200,
    duration_ms: 10,
    tags: [],
    start_time_ms: Date.now(),
    has_error: false,
    is_websocket: false,
    ...overrides,
  };
}

describe('filterFlows', () => {
  const flows = [
    summary({ id: '1', method: 'GET', host: 'api.github.com', status: 200 }),
    summary({ id: '2', method: 'POST', host: 'api.github.com', status: 404, has_error: true }),
    summary({ id: '3', method: 'GET', host: 'example.com', status: 500, has_error: true }),
    summary({ id: '4', method: 'GET', host: 'ws.example.com', is_websocket: true }),
  ];

  it('matches host: token', () => {
    const result = filterFlows(flows, 'host:github');
    expect(result.map((f) => f.id)).toEqual(['1', '2']);
  });

  it('matches method: token', () => {
    const result = filterFlows(flows, 'method:POST');
    expect(result.map((f) => f.id)).toEqual(['2']);
  });

  it('matches status range', () => {
    const result = filterFlows(flows, 'status:>=400');
    expect(result.map((f) => f.id)).toEqual(['2', '3']);
  });

  it('matches err and ws flags', () => {
    expect(filterFlows(flows, 'err').map((f) => f.id)).toEqual(['2', '3']);
    expect(filterFlows(flows, 'ws').map((f) => f.id)).toEqual(['4']);
  });

  it('combines tokens with AND', () => {
    const result = filterFlows(flows, 'host:github err');
    expect(result.map((f) => f.id)).toEqual(['2']);
  });
});
