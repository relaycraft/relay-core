import type { FlowSummary } from '@/types/api';

/** Client-side flow filter matching relay-core-api parse_flow_filter semantics. */
export function filterFlows(flows: FlowSummary[], filterText: string): FlowSummary[] {
  const filter = filterText.trim();
  if (!filter) return flows;

  const tokens = filter.split(/\s+/).filter(Boolean);
  return flows.filter((f) =>
    tokens.every((token) => {
      if (token.startsWith('host:')) {
        return f.host.toLowerCase().includes(token.slice(5).toLowerCase());
      }
      if (token.startsWith('method:')) {
        return f.method.toUpperCase() === token.slice(7).toUpperCase();
      }
      if (token.startsWith('status:')) {
        const s = token.slice(7);
        if (s.startsWith('>=')) return (f.status ?? 0) >= parseInt(s.slice(2), 10);
        if (s.startsWith('<=')) return (f.status ?? 0) <= parseInt(s.slice(2), 10);
        if (s.startsWith('>')) return (f.status ?? 0) > parseInt(s.slice(1), 10);
        if (s.startsWith('<')) return (f.status ?? 0) < parseInt(s.slice(1), 10);
        if (s.includes('-')) {
          const [min, max] = s.split('-').map(Number);
          return (f.status ?? 0) >= min && (f.status ?? 0) <= max;
        }
        return f.status === parseInt(s, 10);
      }
      if (token === 'err') return f.has_error;
      if (token === 'ws') return f.is_websocket;
      const q = token.toLowerCase();
      return (
        f.host.toLowerCase().includes(q) ||
        f.path.toLowerCase().includes(q) ||
        f.method.toLowerCase() === q
      );
    }),
  );
}

export function orderedFlows(
  flowOrder: string[],
  flows: Map<string, FlowSummary>,
): FlowSummary[] {
  return flowOrder.map((id) => flows.get(id)).filter(Boolean) as FlowSummary[];
}
