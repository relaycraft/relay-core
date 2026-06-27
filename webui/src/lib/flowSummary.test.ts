import { describe, expect, it } from 'vitest';
import { flowToSummary } from './flowSummary';
import type { Flow } from '@/types/api';

describe('flowToSummary', () => {
  it('maps HTTP flow fields', () => {
    const flow: Flow = {
      id: 'flow-1',
      start_time: '2024-01-01T12:00:00.000Z',
      end_time: '2024-01-01T12:00:00.050Z',
      network: {
        client_ip: '127.0.0.1',
        client_port: 1234,
        server_ip: '93.184.216.34',
        server_port: 443,
        protocol: 'TCP',
        tls: true,
        tls_version: 'TLS1.3',
        sni: 'example.com',
      },
      layer: {
        type: 'Http',
        data: {
          request: {
            method: 'GET',
            url: 'https://example.com/hello?q=1',
            version: 'HTTP/1.1',
            headers: [],
            cookies: [],
            query: [],
            body: null,
          },
          response: {
            status: 200,
            status_text: 'OK',
            version: 'HTTP/1.1',
            headers: [],
            cookies: [],
            body: null,
            timing: {
              time_to_first_byte: 10,
              time_to_last_byte: 50,
            },
          },
          error: null,
        },
      },
      tags: [],
      resilience_trace: null,
      rule_variables: {},
      matched_rules: [],
    };

    const summary = flowToSummary(flow);
    expect(summary.id).toBe('flow-1');
    expect(summary.method).toBe('GET');
    expect(summary.host).toBe('example.com');
    expect(summary.path).toBe('/hello?q=1');
    expect(summary.status).toBe(200);
    expect(summary.duration_ms).toBe(50);
  });
});
