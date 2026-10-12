import type { Flow, CoreStatusSnapshot, AuditEvent } from '@/types/api';
import { isTunnel, showTunnels } from '@/lib/tunnels';

export type SseEvent =
  | { type: 'flow'; data: Flow }
  | { type: 'ws-message'; data: { flow_id: string; message: unknown } }
  | { type: 'http-body'; data: { flow_id: string } }
  | { type: 'body-budget-exceeded'; data: { flow_id: string } }
  | { type: 'response-trailers'; data: { flow_id: string; trailers: [string, string][] } }
  | { type: 'stream-reset'; data: { flow_id: string; detail: string } }
  /* A typed lifecycle transition, `{"event": "...", "flow_id": "..."}`. Deliberately `unknown`:
     the shape is the engine's, and `describeFlowEvent` is the one place that reads it, so a variant
     this build does not know is skipped there rather than typed into existence here. */
  | { type: 'flow-event'; data: unknown }
  | { type: 'audit'; data: AuditEvent }
  | { type: 'lifecycle'; data: CoreStatusSnapshot }
  | { type: 'lagged'; data: string }
  | { type: 'audit-lagged'; data: string }
  | { type: 'connected' }
  /* The stream dropped. Reported because the browser hides it otherwise: `onerror` fires, the
     connection is retried, and everything downstream keeps believing the last `connected`. */
  | { type: 'disconnected' };

export type SseEventHandler = (event: SseEvent) => void;

export interface SseConnectOptions {
  onConnected?: () => void;
}

export function connectSse(handler: SseEventHandler, options?: SseConnectOptions): () => void {
  let stopped = false;
  let eventSource: EventSource | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let hasConnectedOnce = false;

  function connect() {
    if (stopped) return;

    const es = new EventSource('/api/v1/events');
    eventSource = es;

    es.addEventListener('flow', (e) => {
      try {
        const flow = JSON.parse(e.data);
        // The live stream carries every capture, including the CONNECT instruction of a tunnel the
        // reader asked not to see. Filtering here (rather than in the list) keeps the store honest:
        // what is in it is what the list shows.
        if (!showTunnels() && isTunnel(flow)) return;
        handler({ type: 'flow', data: flow });
      } catch {}
    });

    es.addEventListener('ws-message', (e) => {
      try {
        handler({ type: 'ws-message', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('http-body', (e) => {
      try {
        handler({ type: 'http-body', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('flow-event', (e) => {
      try {
        handler({ type: 'flow-event', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('response-trailers', (e) => {
      try {
        handler({ type: 'response-trailers', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('stream-reset', (e) => {
      try {
        handler({ type: 'stream-reset', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('body-budget-exceeded', (e) => {
      try {
        handler({ type: 'body-budget-exceeded', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('audit', (e) => {
      try {
        handler({ type: 'audit', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('lifecycle', (e) => {
      try {
        handler({ type: 'lifecycle', data: JSON.parse(e.data) });
      } catch {}
    });

    es.addEventListener('lagged', (e) => {
      handler({ type: 'lagged', data: e.data });
    });

    es.addEventListener('audit-lagged', (e) => {
      handler({ type: 'audit-lagged', data: e.data });
    });

    es.onopen = () => {
      if (hasConnectedOnce) {
        options?.onConnected?.();
      } else {
        hasConnectedOnce = true;
      }
      handler({ type: 'connected' });
    };

    es.onerror = () => {
      if (stopped) return;
      es.close();
      eventSource = null;
      // Nothing downstream can see a stream that stopped: `sseConnected` stayed true, so the status
      // bar showed a healthy SSE dot and an empty flow list blamed the proxy for a browser that was
      // no longer listening.
      handler({ type: 'disconnected' });
      reconnectTimer = setTimeout(connect, 2000);
    };
  }

  connect();

  return () => {
    stopped = true;
    if (eventSource) {
      eventSource.close();
      eventSource = null;
    }
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  };
}
