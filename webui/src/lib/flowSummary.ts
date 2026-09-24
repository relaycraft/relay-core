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
      // `server_ip` is a placeholder for a forward-proxy flow, so the recorded target is what ever
      // has a value; falling back to it keeps summaries from showing `0.0.0.0`.
      host =
        flow.network?.sni ??
        flow.network?.server_host ??
        (flow.network?.server_ip && flow.network.server_ip !== '0.0.0.0'
          ? flow.network.server_ip
          : '') ??
        '';
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

  const http = flow.layer.type === 'Http' ? (flow.layer as HttpLayer).data : undefined;
  const outcome = grpcOutcome(http?.response?.trailers);
  const streamReset = (flow.tags ?? []).includes('stream-reset');
  const hasError =
    (status !== null && status >= 500) ||
    flow.tags.includes('error') ||
    !!http?.error ||
    (outcome.status != null && outcome.status !== 0) ||
    streamReset;

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
    grpc_status: outcome.status,
    grpc_message: outcome.message,
  };
}

function grpcOutcome(trailers?: [string, string][]): {
  status: number | null;
  message: string | null;
} {
  if (!trailers) return { status: null, message: null };
  const statusRaw = trailers.find(([name]) => name.toLowerCase() === 'grpc-status')?.[1];
  const messageRaw = trailers.find(([name]) => name.toLowerCase() === 'grpc-message')?.[1];
  const parsed = statusRaw != null ? Number.parseInt(statusRaw.trim(), 10) : NaN;
  const status = Number.isFinite(parsed) ? parsed : null;
  let message: string | null = null;
  if (messageRaw && messageRaw.trim() !== '') {
    try {
      message = decodeURIComponent(messageRaw.trim());
    } catch {
      message = messageRaw.trim();
    }
  }
  return { status, message };
}
