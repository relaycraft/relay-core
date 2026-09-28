import { describe, expect, it } from 'vitest';
import { flowIsError, flowTarget, flowTargetParts, flowToSummary } from './flowSummary';
import type { Flow, HttpLayer, NetworkInfo } from '@/types/api';

const network: NetworkInfo = {
  client_ip: '127.0.0.1',
  client_port: 1234,
  server_ip: '93.184.216.34',
  server_port: 443,
  protocol: 'TCP',
  tls: true,
  tls_version: 'TLS1.3',
  sni: 'example.com',
};

function httpFlow(overrides: {
  url?: string;
  method?: string;
  status?: number | null;
  error?: string | null;
  trailers?: [string, string][];
  tags?: string[];
  closeReason?: Flow['close_reason'];
  endTime?: string | null;
}): Flow {
  return {
    id: 'flow-1',
    start_time: '2024-01-01T12:00:00.000Z',
    end_time: overrides.endTime === undefined ? '2024-01-01T12:00:00.050Z' : overrides.endTime,
    network,
    layer: {
      type: 'Http',
      data: {
        request: {
          method: overrides.method ?? 'GET',
          url: overrides.url ?? 'https://example.com/hello?q=1',
          version: 'HTTP/1.1',
          headers: [],
          cookies: [],
          query: [],
          body: null,
        },
        response: overrides.status === null
          ? null
          : {
              status: overrides.status ?? 200,
              status_text: 'OK',
              version: 'HTTP/1.1',
              headers: [],
              cookies: [],
              body: null,
              timing: { time_to_first_byte: 10, time_to_last_byte: 50 },
              trailers: overrides.trailers,
            },
        error: overrides.error ?? null,
      },
    },
    tags: overrides.tags ?? [],
    close_reason: overrides.closeReason ?? null,
  } as Flow;
}

/** The `host`/`path` the server derives, mirroring `Url::host_str()` and `Url::path()`. */
describe('flowToSummary', () => {
  it('maps HTTP flow fields the way the server summarises them', () => {
    const summary = flowToSummary(httpFlow({}));
    expect(summary.id).toBe('flow-1');
    expect(summary.method).toBe('GET');
    expect(summary.url).toBe('https://example.com/hello?q=1');
    // No port — the URL has none — and no query string: `host_str()` and `path()`, which is what
    // every matcher in the engine means by these two names.
    expect(summary.host).toBe('example.com');
    expect(summary.path).toBe('/hello');
    expect(summary.status).toBe(200);
    expect(summary.duration_ms).toBe(50);
    expect(summary.is_websocket).toBe(false);
  });

  it('drops the port and the query string from host and path', () => {
    // The exact row that read `127.0.0.1:18090/tick-48?marker=48` while live and `127.0.0.1/tick-48`
    // after a refresh: the live summary took the display value, the stored one took the match value.
    const summary = flowToSummary(
      httpFlow({ url: 'http://127.0.0.1:18090/tick-48?marker=48' }),
    );
    expect(summary.host).toBe('127.0.0.1');
    expect(summary.path).toBe('/tick-48');
  });

  it('keeps the displayed target complete, port and query included', () => {
    const summary = flowToSummary(
      httpFlow({ url: 'http://127.0.0.1:18090/tick-48?marker=48' }),
    );
    expect(flowTargetParts(summary)).toEqual({
      host: '127.0.0.1:18090',
      path: '/tick-48?marker=48',
    });
    expect(flowTarget(summary)).toBe('127.0.0.1:18090/tick-48?marker=48');
  });

  it('says UNKNOWN rather than ? for a flow with no application layer', () => {
    const flow = httpFlow({});
    const summary = flowToSummary({
      ...flow,
      layer: { type: 'Tcp', data: { bytes_up: 1, bytes_down: 2 } },
    } as unknown as Flow);
    expect(summary.method).toBe('UNKNOWN');
    // The server puts empty strings here for a layer it cannot read a URL from, and a live row must
    // not read differently from the stored one — the detail pane still shows the network endpoint.
    expect(summary.host).toBe('');
    expect(summary.path).toBe('');
    expect(summary.url).toBe('');
    expect(summary.has_error).toBe(false);
  });

  it('never reports a negative duration, even for out-of-order timestamps', () => {
    const summary = flowToSummary(
      httpFlow({ endTime: '2024-01-01T11:59:59.000Z' }),
    );
    expect(summary.duration_ms).toBe(0);
  });

  it('leaves the duration unset while the flow is still in flight', () => {
    const summary = flowToSummary(httpFlow({ endTime: null }));
    expect(summary.duration_ms).toBeNull();
  });
});

describe('flowIsError', () => {
  it('counts an upstream failure that has no status code', () => {
    // The case that made the badge disappear on refresh: the browser saw `http.error` and the
    // summary only looked at the status.
    const summary = flowToSummary(httpFlow({ status: null, error: 'connection refused' }));
    expect(summary.status).toBeNull();
    expect(summary.has_error).toBe(true);
  });

  it('counts a 5xx and a non-zero grpc status', () => {
    expect(flowIsError(httpFlow({ status: 503 }))).toBe(true);
    expect(
      flowIsError(httpFlow({ trailers: [['grpc-status', '14']] })),
    ).toBe(true);
    expect(flowIsError(httpFlow({ trailers: [['grpc-status', '0']] }))).toBe(false);
  });

  it('does not count a 404', () => {
    expect(flowIsError(httpFlow({ status: 404 }))).toBe(false);
  });

  it('counts a reset close reason and the stream-reset tag', () => {
    expect(flowIsError(httpFlow({ closeReason: { reason: 'reset' } }))).toBe(true);
    expect(flowIsError(httpFlow({ tags: ['stream-reset'] }))).toBe(true);
    expect(flowIsError(httpFlow({ closeReason: { reason: 'completed' } }))).toBe(false);
  });
});

describe('grpc trailers', () => {
  it('parses the status strictly, as the server does', () => {
    const trailers: [string, string][] = [['grpc-status', '+14']];
    expect(flowToSummary(httpFlow({ trailers })).grpc_status).toBe(14);
    // `parseInt` would read these two as 14 and 0; `str::parse::<i32>` rejects both, and a badge that
    // appears live and vanishes on refresh is worse than no badge at all.
    expect(flowToSummary(httpFlow({ trailers: [['grpc-status', '14x']] })).grpc_status).toBeNull();
    expect(flowToSummary(httpFlow({ trailers: [['grpc-status', '0x10']] })).grpc_status).toBeNull();
    expect(
      flowToSummary(httpFlow({ trailers: [['grpc-status', '99999999999']] })).grpc_status,
    ).toBeNull();
  });

  it('decodes grpc-message byte-wise, keeping invalid escapes as written', () => {
    expect(
      flowToSummary(httpFlow({ trailers: [['grpc-message', 'unavailable%20now']] })).grpc_message,
    ).toBe('unavailable now');
    // `decodeURIComponent` throws on a lone `%FF`; the server replaces it. Both must agree.
    expect(
      flowToSummary(httpFlow({ trailers: [['grpc-message', 'bad%FFescape']] })).grpc_message,
    ).toBe('bad\ufffdescape');
    expect(
      flowToSummary(httpFlow({ trailers: [['grpc-message', 'keep%zzthis']] })).grpc_message,
    ).toBe('keep%zzthis');
    expect(
      flowToSummary(httpFlow({ trailers: [['grpc-message', '%E4%B8%AD%E6%96%87']] })).grpc_message,
    ).toBe('中文');
    expect(flowToSummary(httpFlow({ trailers: [['grpc-message', '   ']] })).grpc_message).toBeNull();
  });

  it('reads trailer names case-insensitively', () => {
    const summary = flowToSummary(httpFlow({ trailers: [['Grpc-Status', '5']] }));
    expect(summary.grpc_status).toBe(5);
    expect(summary.has_error).toBe(true);
  });
});

describe('flowTargetParts', () => {
  it('falls back to the summary fields when there is no URL to parse', () => {
    const base = flowToSummary(httpFlow({}));
    expect(flowTargetParts({ ...base, url: '', host: 'raw.example', path: '/x' })).toEqual({
      host: 'raw.example',
      path: '/x',
    });
    expect(flowTargetParts({ ...base, url: 'not a url', host: '', path: '' })).toEqual({
      host: '',
      path: '',
    });
  });

  it('shows a WebSocket target the same way as an HTTP one', () => {
    const flow: Flow = {
      id: 'flow-ws',
      start_time: '2024-01-01T12:00:00.000Z',
      end_time: null,
      network,
      layer: {
        type: 'WebSocket',
        data: {
          handshake_request: {
            method: 'GET',
            url: 'wss://example.com:8443/socket?room=1',
            version: 'HTTP/1.1',
            headers: [],
            cookies: [],
            query: [],
            body: null,
          },
          handshake_response: {
            status: 101,
            status_text: 'Switching Protocols',
            version: 'HTTP/1.1',
            headers: [],
            cookies: [],
            body: null,
            timing: { time_to_first_byte: 1, time_to_last_byte: 1 },
          },
          messages: [],
          closed: false,
        },
      },
      tags: [],
    } as unknown as Flow;

    const summary = flowToSummary(flow);
    expect(summary.is_websocket).toBe(true);
    expect(summary.status).toBe(101);
    expect(summary.host).toBe('example.com');
    expect(summary.path).toBe('/socket');
    expect(flowTarget(summary)).toBe('example.com:8443/socket?room=1');
  });

  it('reports a WebSocket handshake failure as an error', () => {
    const flow = httpFlow({});
    const ws = {
      ...flow,
      layer: {
        type: 'WebSocket',
        data: {
          handshake_request: (flow.layer as HttpLayer).data.request,
          handshake_response: { status: 502 },
          messages: [],
          closed: true,
        },
      },
    } as unknown as Flow;
    expect(flowIsError(ws)).toBe(true);
  });
});
