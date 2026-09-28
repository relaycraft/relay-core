import { describe, expect, it } from 'vitest';
import { filterFlows, flowMatchesFilter, parseFlowFilter } from './flowFilter';
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
    summary({ id: '4', method: 'GET', host: 'ws.example.com', is_websocket: true, status: null }),
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

  it('matches path: and its aliases', () => {
    expect(filterFlows(flows, 'path:/v1').map((f) => f.id)).toEqual(['1', '2', '3', '4']);
    // `error` and `websocket` are accepted spellings in the engine's parser.
    expect(filterFlows(flows, 'error').map((f) => f.id)).toEqual(['2', '3']);
    expect(filterFlows(flows, 'websocket').map((f) => f.id)).toEqual(['4']);
  });

  it('leaves the list alone for a status constraint it cannot read', () => {
    // `status:70000` does not fit a `u16`, so the engine ignores the constraint entirely; a client
    // that demanded `status >= 70000` would show nothing where the TUI shows everything.
    expect(filterFlows(flows, 'status:70000').map((f) => f.id)).toEqual(['1', '2', '3', '4']);
  });

  it('searches for a key with no value as plain text', () => {
    // Also the engine's reading: `status:` carries no value, so it is a word to look for rather than
    // a constraint, and no row contains it. Recording the behaviour because "empty list" is the
    // visible consequence of typing a token one keystroke too early.
    expect(filterFlows(flows, 'status:')).toEqual([]);
  });
});

describe('parseFlowFilter', () => {
  it('keeps a plain sentence as one needle', () => {
    // `parse_plain_text_single_blob`: with nothing structured, `api example` is one phrase to find,
    // not two independent words — the two only look alike until a row contains both apart.
    const filter = parseFlowFilter('  api example ');
    expect(filter.textTokens).toEqual(['api example']);
    expect(filter.host).toBeNull();
  });

  it('reads structured tokens and their aliases', () => {
    const filter = parseFlowFilter('host:api.example path:/v1 method:POST status:>=400 err ws');
    expect(filter.host).toBe('api.example');
    expect(filter.path).toBe('/v1');
    expect(filter.method).toBe('POST');
    expect(filter.statusMin).toBe(400);
    expect(filter.statusMax).toBeNull();
    expect(filter.hasError).toBe(true);
    expect(filter.isWebsocket).toBe(true);
    expect(filter.textTokens).toEqual([]);
  });

  it('reads every status form', () => {
    expect(parseFlowFilter('status:404')).toMatchObject({ statusMin: 404, statusMax: 404 });
    expect(parseFlowFilter('status:400-499')).toMatchObject({ statusMin: 400, statusMax: 499 });
    expect(parseFlowFilter('status:>399')).toMatchObject({ statusMin: 400, statusMax: null });
    expect(parseFlowFilter('status:<400')).toMatchObject({ statusMin: null, statusMax: 399 });
    expect(parseFlowFilter('status:<=399')).toMatchObject({ statusMin: null, statusMax: 399 });
    // Saturating at both ends of the `u16` range, like `saturating_add` / `saturating_sub`.
    expect(parseFlowFilter('status:>65535')).toMatchObject({ statusMin: 65535 });
    expect(parseFlowFilter('status:<0')).toMatchObject({ statusMax: 0 });
    // Unparsable in every shape the engine rejects.
    for (const token of ['status:abc', 'status:>abc', 'status:404abc', 'status:0x1a', 'status:-100', 'status:1-2-3']) {
      expect(parseFlowFilter(token)).toMatchObject({ statusMin: null, statusMax: null });
    }
  });

  it('treats an unknown key and an empty value as plain text', () => {
    expect(parseFlowFilter('foo:bar').textTokens).toEqual(['foo:bar']);
    // A half-typed `host:` must not be read as "every host" — it is a word to search for.
    const half = parseFlowFilter('host:');
    expect(half.host).toBeNull();
    expect(half.textTokens).toEqual(['host:']);
  });

  it('treats an unknown key as structured for the plain-text join', () => {
    // `had_structured` is set as soon as a `key:value` token is seen, known key or not, so a mixed
    // expression does not collapse into one phrase.
    expect(parseFlowFilter('foo:bar baz').textTokens).toEqual(['foo:bar', 'baz']);
  });

  it('accepts an empty filter', () => {
    const filter = parseFlowFilter('   ');
    expect(filter.textTokens).toEqual([]);
    expect(filter.host).toBeNull();
  });
});

describe('flowMatchesFilter', () => {
  it('matches the method as a substring, as the engine does', () => {
    const row = summary({ id: '1', method: 'PATCH' });
    expect(flowMatchesFilter(row, parseFlowFilter('atch'))).toBe(true);
    expect(flowMatchesFilter(row, parseFlowFilter('method:PATCH'))).toBe(true);
    expect(flowMatchesFilter(row, parseFlowFilter('method:PAT'))).toBe(false);
  });

  it('matches a plain token spanning the host and the path', () => {
    // The engine matches against the concatenation, so a token may cross the boundary between them.
    const row = summary({ id: '1', host: 'api.example.com', path: '/v1/users' });
    expect(flowMatchesFilter(row, parseFlowFilter('com/v1'))).toBe(true);
    expect(flowMatchesFilter(row, parseFlowFilter('/v1/users'))).toBe(true);
  });

  it('excludes a row with no status from any status constraint', () => {
    const pending = summary({ id: '1', status: null });
    expect(flowMatchesFilter(pending, parseFlowFilter('status:<400'))).toBe(false);
    expect(flowMatchesFilter(pending, parseFlowFilter('status:>=200'))).toBe(false);
    expect(flowMatchesFilter(pending, parseFlowFilter('err'))).toBe(false);
  });

  it('matches host and path case-insensitively', () => {
    const row = summary({ id: '1', host: 'API.Example.com', path: '/V1' });
    expect(flowMatchesFilter(row, parseFlowFilter('host:api.example'))).toBe(true);
    expect(flowMatchesFilter(row, parseFlowFilter('path:/v1'))).toBe(true);
  });
});
