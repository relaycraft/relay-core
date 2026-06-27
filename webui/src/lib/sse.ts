import type { Flow, CoreStatusSnapshot, AuditEvent } from '@/types/api';

export type SseEvent =
  | { type: 'flow'; data: Flow }
  | { type: 'ws-message'; data: { flow_id: string; message: unknown } }
  | { type: 'http-body'; data: { flow_id: string } }
  | { type: 'body-budget-exceeded'; data: { flow_id: string } }
  | { type: 'audit'; data: AuditEvent }
  | { type: 'lifecycle'; data: CoreStatusSnapshot }
  | { type: 'lagged'; data: string }
  | { type: 'audit-lagged'; data: string }
  | { type: 'connected' };

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
        handler({ type: 'flow', data: JSON.parse(e.data) });
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
