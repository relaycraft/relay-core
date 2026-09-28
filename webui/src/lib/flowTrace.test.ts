import { describe, expect, it } from 'vitest';
import { flowTrace } from './flowTrace';
import type { Flow } from '@/types/api';

/**
 * A flow exactly as the API sends an ordinary capture.
 *
 * `matched_rules` and `rule_variables` are **absent**, not empty: the Rust side marks both
 * `skip_serializing_if`, so a flow with no rule activity carries neither key. This is the shape that
 * reached the Trace tab and threw, so it is the shape the test is built from — a hand-written
 * `matched_rules: []` would have hidden the whole bug.
 */
function capturedFlow(): Flow {
  return {
    id: 'flow-1',
    start_time: '2024-01-01T12:00:00.000Z',
    end_time: null,
    network: {
      client_ip: '127.0.0.1',
      client_port: 1234,
      server_ip: '0.0.0.0',
      server_port: 0,
      protocol: 'TCP',
      tls: false,
      tls_version: null,
      sni: null,
    },
    layer: {
      type: 'Http',
      data: {
        request: {
          method: 'GET',
          url: 'http://127.0.0.1:18090/hello',
          version: 'HTTP/1.1',
          headers: [],
          cookies: [],
          query: [],
          body: null,
        },
        response: null,
        error: null,
      },
    },
    tags: [],
  };
}

describe('flowTrace', () => {
  it('reads a flow whose rule fields were omitted as having no rule activity', () => {
    const trace = flowTrace(capturedFlow());
    expect(trace.matchedRules).toEqual([]);
    expect(trace.ruleVariables).toEqual([]);
  });

  it('keeps the rules and variables when the flow carries them', () => {
    const flow: Flow = {
      ...capturedFlow(),
      matched_rules: ['rule-a', 'rule-b'],
      rule_variables: { token: 'abc', region: 'eu' },
    };
    const trace = flowTrace(flow);
    expect(trace.matchedRules).toEqual(['rule-a', 'rule-b']);
    expect(trace.ruleVariables).toEqual([
      ['token', 'abc'],
      ['region', 'eu'],
    ]);
  });

  it('treats explicitly empty collections the same as omitted ones', () => {
    const flow: Flow = { ...capturedFlow(), matched_rules: [], rule_variables: {} };
    const trace = flowTrace(flow);
    expect(trace.matchedRules).toEqual([]);
    expect(trace.ruleVariables).toEqual([]);
  });

  it('returns a fresh array each call, so a caller cannot mutate what a flow holds', () => {
    const flow: Flow = { ...capturedFlow(), matched_rules: ['rule-a'] };
    flowTrace(flow).matchedRules.push('rule-b');
    expect(flow.matched_rules).toEqual(['rule-a']);
  });
});
