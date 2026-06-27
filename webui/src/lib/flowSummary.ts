import type { Flow, FlowSummary, HttpLayer, WebSocketLayer } from '@/types/api';

export function flowToSummary(flow: Flow): FlowSummary {
  let method = '?';
  let url = '';
  let host = '';
  let path = '';
  let status: number | null = null;
  let isWebsocket = false;

  if (flow.layer.type === 'Http') {
    const http = (flow.layer as HttpLayer).data;
    method = http.request?.method ?? '?';
    url = http.request?.url ?? '';
    try {
      const parsed = new URL(url);
      host = parsed.host;
      path = parsed.pathname + parsed.search;
    } catch {
      host = flow.network?.sni ?? flow.network?.server_ip ?? '';
    }
    status = http.response?.status ?? null;
  } else if (flow.layer.type === 'WebSocket') {
    const ws = (flow.layer as WebSocketLayer).data;
    method = ws.handshake_request?.method ?? 'WS';
    url = ws.handshake_request?.url ?? '';
    try {
      const parsed = new URL(url);
      host = parsed.host;
      path = parsed.pathname + parsed.search;
    } catch {
      host = flow.network?.sni ?? '';
    }
    status = ws.handshake_response?.status ?? null;
    isWebsocket = true;
  } else {
    host = flow.network?.sni ?? flow.network?.server_ip ?? '';
  }

  const startMs = Date.parse(flow.start_time);
  const endMs = flow.end_time ? Date.parse(flow.end_time) : NaN;
  const durationMs = Number.isFinite(startMs) && Number.isFinite(endMs)
    ? Math.max(0, endMs - startMs)
    : null;

  const hasError =
    (status !== null && status >= 500) ||
    flow.tags.includes('error') ||
    (flow.layer.type === 'Http' && !!(flow.layer as HttpLayer).data?.error);

  return {
    id: flow.id,
    method,
    url,
    host,
    path,
    status,
    duration_ms: durationMs,
    tags: flow.tags ?? [],
    start_time_ms: Number.isFinite(startMs) ? startMs : Date.now(),
    has_error: hasError,
    is_websocket: isWebsocket || flow.layer.type === 'WebSocket',
  };
}
