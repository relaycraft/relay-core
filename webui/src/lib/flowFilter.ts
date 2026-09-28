import type { FlowSummary } from '@/types/api';

/**
 * The filter expression, parsed the way `parse_flow_filter` parses it.
 *
 * Mirrors `relay-core-api/src/flow_query.rs` field for field — the filter bar is documented as reusing
 * the TUI's filter language (`.ai/archived/planning/webui-design.md` §5.1), so the two must accept the
 * same input and select the same rows. They did not: this side matched the method exactly where the
 * engine matches it as a substring, ignored `path:` entirely, treated a status-less row as status `0`
 * under a range, and matched the query string — which is not part of `path` anywhere else in the
 * engine, so a row showing `?marker=48` was found by the search box and not by `rc flows --filter`.
 */
export interface FlowFilter {
  host: string | null;
  path: string | null;
  method: string | null;
  statusMin: number | null;
  statusMax: number | null;
  isWebsocket: boolean | null;
  hasError: boolean | null;
  textTokens: string[];
}

const EMPTY_FILTER: FlowFilter = {
  host: null,
  path: null,
  method: null,
  statusMin: null,
  statusMax: null,
  isWebsocket: null,
  hasError: null,
  textTokens: [],
};

/** Parse a filter bar expression into structured constraints plus plain-text tokens. */
export function parseFlowFilter(input: string): FlowFilter {
  const trimmed = input.trim();
  if (!trimmed) return { ...EMPTY_FILTER, textTokens: [] };

  const filter: FlowFilter = { ...EMPTY_FILTER, textTokens: [] };
  let hadStructured = false;

  for (const token of trimmed.split(/\s+/).filter(Boolean)) {
    const colon = token.indexOf(':');
    if (colon >= 0) {
      const key = token.slice(0, colon).toLowerCase();
      const value = token.slice(colon + 1);
      if (value === '') {
        // `host:` with nothing after it is not a constraint on everything — the engine treats it as
        // a plain word to search for, and so must this, or typing a half-finished token would show
        // the whole list as if the filter had been applied.
        filter.textTokens.push(token);
        continue;
      }
      hadStructured = true;
      switch (key) {
        case 'host':
          filter.host = value;
          break;
        case 'path':
          filter.path = value;
          break;
        case 'method':
          filter.method = value;
          break;
        case 'status':
          applyStatusToken(filter, value);
          break;
        default:
          // An unknown `key:value` is not structured input: it is a word that happens to contain a
          // colon, and it is searched for as one.
          filter.textTokens.push(token);
      }
    } else {
      switch (token.toLowerCase()) {
        case 'err':
        case 'error':
          hadStructured = true;
          filter.hasError = true;
          break;
        case 'ws':
        case 'websocket':
          hadStructured = true;
          filter.isWebsocket = true;
          break;
        default:
          filter.textTokens.push(token);
      }
    }
  }

  // With nothing structured to go on, the whole input is one needle that may contain spaces —
  // `api example` searches for that phrase, it does not search for `api` and `example` separately.
  if (!hadStructured && filter.textTokens.length > 0) {
    filter.textTokens = [filter.textTokens.join(' ')];
  }

  return filter;
}

/**
 * `status:` in every form the engine accepts: `404`, `>=400`, `<=399`, `>399`, `<400`, `400-499`.
 *
 * An unparsable value leaves *no* constraint rather than an impossible one, exactly as
 * `apply_status_token` does — `status:70000` does not fit a `u16`, so the engine ignores it, and a
 * client that instead demanded `status >= 70000` would show an empty list for a filter the TUI
 * answers with everything.
 */
function applyStatusToken(filter: FlowFilter, rawValue: string): void {
  const value = rawValue.trim();
  const bound = (text: string): number | null => parseU16(text);

  if (value.startsWith('>=')) {
    const n = bound(value.slice(2));
    if (n != null) filter.statusMin = n;
    return;
  }
  if (value.startsWith('<=')) {
    const n = bound(value.slice(2));
    if (n != null) filter.statusMax = n;
    return;
  }
  if (value.startsWith('>')) {
    const n = bound(value.slice(1));
    // Saturating, like `u16::saturating_add`: `status:>65535` cannot exclude more than `>=65535`.
    if (n != null) filter.statusMin = Math.min(65535, n + 1);
    return;
  }
  if (value.startsWith('<')) {
    const n = bound(value.slice(1));
    if (n != null) filter.statusMax = Math.max(0, n - 1);
    return;
  }
  const dash = value.indexOf('-');
  if (dash >= 0) {
    const min = bound(value.slice(0, dash).trim());
    const max = bound(value.slice(dash + 1).trim());
    if (min != null && max != null) {
      filter.statusMin = min;
      filter.statusMax = max;
      return;
    }
  }
  const exact = bound(value);
  if (exact != null) {
    filter.statusMin = exact;
    filter.statusMax = exact;
  }
}

/**
 * A `u16` as `str::parse::<u16>()` reads one: optional `+`, digits, nothing else, and in range.
 *
 * Not `parseInt`, which reads `404abc` as 404, `0x1a` as 26 and `1e3` as 1 — every one of which the
 * engine reads as "no constraint at all".
 */
function parseU16(text: string): number | null {
  if (!/^\+?\d+$/.test(text)) return null;
  const value = Number.parseInt(text, 10);
  return Number.isSafeInteger(value) && value >= 0 && value <= 65535 ? value : null;
}

/**
 * Does this row satisfy the filter?
 *
 * The matching half of the mirror: `flow_matches_query` plus the plain-text rule of
 * `flow_matches_filter`, applied to the summary fields that correspond to the engine's own
 * `flow_layer_fields` — `host` and `path` without a port or a query string, the same values the
 * server's `?host=`/`?path=` search matches on.
 */
export function flowMatchesFilter(flow: FlowSummary, filter: FlowFilter): boolean {
  const host = flow.host.toLowerCase();
  const path = flow.path.toLowerCase();
  const method = flow.method.toLowerCase();

  if (filter.host != null && !host.includes(filter.host.toLowerCase())) return false;
  if (filter.path != null && !path.includes(filter.path.toLowerCase())) return false;
  if (filter.method != null && method !== filter.method.toLowerCase()) return false;
  // A row with no status fails any status constraint — it is not a `0`. `status:<400` used to match
  // a request that had not been answered yet, which is the one row the user cannot be looking for.
  if (filter.statusMin != null && (flow.status == null || flow.status < filter.statusMin)) {
    return false;
  }
  if (filter.statusMax != null && (flow.status == null || flow.status > filter.statusMax)) {
    return false;
  }
  if (filter.isWebsocket != null && flow.is_websocket !== filter.isWebsocket) return false;
  if (filter.hasError != null && flow.has_error !== filter.hasError) return false;

  if (filter.textTokens.length === 0) return true;
  const blob = host + path;
  return filter.textTokens.every(
    (token) => blob.includes(token.toLowerCase()) || method.includes(token.toLowerCase()),
  );
}

/** Keep the rows a filter bar expression selects, in the order given. */
export function filterFlows(flows: FlowSummary[], filterText: string): FlowSummary[] {
  const trimmed = filterText.trim();
  if (!trimmed) return flows;
  const filter = parseFlowFilter(trimmed);
  return flows.filter((flow) => flowMatchesFilter(flow, filter));
}

export function orderedFlows(
  flowOrder: string[],
  flows: Map<string, FlowSummary>,
): FlowSummary[] {
  return flowOrder.map((id) => flows.get(id)).filter(Boolean) as FlowSummary[];
}
