import type {
  Flow,
  FlowSummary,
  HttpLayer,
  HttpRequest,
  WebSocketLayer,
} from '@/types/api';

/**
 * Whether a flow should be presented as failed.
 *
 * Mirror of `relay_core_api::flow::flow_is_error` (`relay-core-api/src/flow.rs`), and the two must
 * stay in step: this decides the error badge on a row that has just arrived over SSE, while the
 * server's `FlowSummary::has_error` decides it for the same row after a refresh. They used to
 * disagree about an upstream failure — the browser counted it (it had the `Flow`, including
 * `http.error`) and the server did not (it only looked at the status) — so the badge disappeared
 * when the page was reloaded, on the exact rows a user is most likely to be investigating.
 */
export function flowIsError(flow: Flow): boolean {
  const tags = flow.tags ?? [];
  if (tags.includes('error')) return true;
  // `close_reason` is an internally tagged enum on the wire: `Reset` arrives as `{"reason":"reset"}`.
  // A reset is the peer cancelling mid-exchange — the captured bytes are a prefix, not a result.
  if (flow.close_reason?.reason === 'reset' || tags.includes('stream-reset')) return true;

  if (flow.layer.type === 'Http') {
    const http = (flow.layer as HttpLayer).data;
    if (http.error) return true;
    const response = http.response;
    if (!response) return false;
    if (response.status >= 500) return true;
    const outcome = grpcOutcome(response.trailers);
    return outcome.status != null && outcome.status !== 0;
  }
  if (flow.layer.type === 'WebSocket') {
    return ((flow.layer as WebSocketLayer).data.handshake_response?.status ?? 0) >= 500;
  }
  return false;
}

/** The request half of the layer, for the two layers that carry one. */
function layerRequest(flow: Flow): HttpRequest | null {
  if (flow.layer.type === 'Http') return (flow.layer as HttpLayer).data.request ?? null;
  if (flow.layer.type === 'WebSocket') {
    return (flow.layer as WebSocketLayer).data.handshake_request ?? null;
  }
  return null;
}

/**
 * Summarise a captured flow into the same row shape the server derives for a stored one.
 *
 * This function exists because a row has two origins: it arrives as a whole `Flow` over SSE, and
 * comes back from `GET /api/v1/flows` as a `FlowSummary`. The same request must produce the same
 * row either way, so every field here mirrors `flow_to_summary` in
 * `relay-core-runtime/src/actors/flow_store.rs` field for field.
 *
 * `host` and `path` are the values the rest of the engine means by those names — `Url::host_str()`
 * and `Url::path()`, so no port and no query string; they are what policy, rules, the filter
 * language and the storage `LIKE` all match against. They are *not* what a row prints: the displayed
 * target is derived from `url` by `flowTargetParts`. Deriving them differently here is what made one
 * row read `127.0.0.1:18090/tick-48?marker=48` while it was live and `127.0.0.1/tick-48` after a
 * refresh.
 */
export function flowToSummary(flow: Flow): FlowSummary {
  // `UNKNOWN` rather than `?`: the server says `UNKNOWN` for a flow with no application layer, and a
  // row that changes its method column on refresh is a row that looks like two different requests.
  let method = 'UNKNOWN';
  let url = '';
  let host = '';
  let path = '';
  let status: number | null = null;
  let isWebsocket = flow.layer.type === 'WebSocket';

  const request = layerRequest(flow);
  if (request) {
    method = request.method ?? 'UNKNOWN';
    url = request.url ?? '';
    if (url) {
      try {
        const parsed = new URL(url);
        // `URL.hostname` is the WHATWG host serialisation, exactly what `host_str()` returns for the
        // `url` crate this engine parses with: lowercased, punycoded, IPv6 in brackets, no port.
        host = parsed.hostname;
        path = parsed.pathname;
      } catch {
        // Unreachable in practice: the engine stores a `Url`, so anything it can serialise is
        // parseable here. If it ever is not, both sides keep the empty strings the server produces
        // for a flow it could not read a URL from, and the row's target falls back to the network.
      }
    }
  }

  if (flow.layer.type === 'Http') {
    status = (flow.layer as HttpLayer).data.response?.status ?? null;
  } else if (flow.layer.type === 'WebSocket') {
    status = (flow.layer as WebSocketLayer).data.handshake_response?.status ?? null;
  }

  const startMs = Date.parse(flow.start_time);
  const endMs = flow.end_time ? Date.parse(flow.end_time) : NaN;
  // Truncation is a property of the timestamps, not of the difference: the server subtracts
  // millisecond-truncated timestamps too, so the same row cannot read `1 ms` live and `0 ms` after a
  // refresh.
  const durationMs =
    Number.isFinite(startMs) && Number.isFinite(endMs) ? Math.max(0, endMs - startMs) : null;

  const trailers = flow.layer.type === 'Http'
    ? (flow.layer as HttpLayer).data.response?.trailers
    : undefined;
  const outcome = grpcOutcome(trailers);

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
    has_error: flowIsError(flow),
    is_websocket: isWebsocket,
    grpc_status: outcome.status,
    grpc_message: outcome.message,
  };
}

/**
 * The target a row shows, split so the host and the path can be styled differently.
 *
 * Read from `url`, which is the one field that carries everything (host, port, path, query) and is
 * present in both the live `Flow` and the stored summary. Going through `url` is what makes a row
 * render identically however it was produced, and it is safe under redaction: the API redacts the
 * `url` of a summary and the whole SSE payload, so a redacted URL is a URL both sides see redacted.
 */
export function flowTargetParts(summary: FlowSummary): { host: string; path: string } {
  if (summary.url) {
    try {
      const parsed = new URL(summary.url);
      // `host` here is the WHATWG property — hostname *with* the port when it is not the scheme
      // default, which is the only place the port survives for display.
      return { host: parsed.host, path: parsed.pathname + parsed.search };
    } catch {
      // Fall through: an unparseable URL contributes nothing beyond what the fields already hold.
    }
  }
  return { host: summary.host, path: summary.path };
}

/** The same target as one string, for tooltips, tests and anything that does not split the styling. */
export function flowTarget(summary: FlowSummary): string {
  const parts = flowTargetParts(summary);
  return parts.host + parts.path;
}

/**
 * The gRPC outcome carried in trailers.
 *
 * Exported because the live gRPC trailer event (`store.applyGrpcTrailers`) has to read the same two
 * headers the same way: it patches a row that has already been summarised, and a second, laxer parser
 * there would put the badge back on the row on one path and not the other.
 *
 * Mirror of `outcome_from_trailers` in `relay-core-api/src/grpc.rs`, down to the parsing rules,
 * because these two values reach the same row from two directions: the badge is drawn from the live
 * `Flow` and from the stored summary after a refresh.
 *
 * The status is parsed the way `str::parse::<i32>` parses it — an optional sign and digits, in i32
 * range — and not the way `parseInt` does: `parseInt('14x')` is 14 and `parseInt('0x10')` is 0, so a
 * malformed trailer would raise a badge in the browser and none after a refresh. `+14` is a valid
 * i32 and must stay one.
 */
export function grpcOutcome(trailers?: [string, string][]): {
  status: number | null;
  message: string | null;
} {
  if (!trailers) return { status: null, message: null };
  const statusRaw = trailers.find(([name]) => name.toLowerCase() === 'grpc-status')?.[1];
  const messageRaw = trailers.find(([name]) => name.toLowerCase() === 'grpc-message')?.[1];
  const trimmed = statusRaw?.trim() ?? '';
  const parsed = /^[+-]?\d+$/.test(trimmed) ? Number.parseInt(trimmed, 10) : NaN;
  const status =
    Number.isSafeInteger(parsed) && parsed >= -2147483648 && parsed <= 2147483647 ? parsed : null;
  const message = messageRaw?.trim() ? decodeGrpcMessage(messageRaw.trim()) : null;
  return { status, message: message || null };
}

/**
 * Percent-decode a `grpc-message` trailer, byte by byte, as `decode_grpc_message` does.
 *
 * Deliberately not `decodeURIComponent`, which is a different function with different opinions: it
 * throws on a lone `%FF` or a truncated multi-byte sequence, and the browser's `catch` then left the
 * escapes in place while the server replaced them — the same trailer, decoded differently depending
 * on whether the row had been refreshed. This decodes the bytes and reads them as lossy UTF-8, which
 * is what the Rust side does, and an invalid escape is left as written on both sides.
 */
function decodeGrpcMessage(raw: string): string {
  const bytes = new TextEncoder().encode(raw);
  const out: number[] = [];
  for (let i = 0; i < bytes.length; i += 1) {
    if (bytes[i] === 0x25 /* % */ && i + 2 < bytes.length) {
      const value = hexPair(bytes[i + 1], bytes[i + 2]);
      if (value != null) {
        out.push(value);
        i += 2;
        continue;
      }
    }
    out.push(bytes[i]);
  }
  return new TextDecoder('utf-8').decode(Uint8Array.from(out));
}

/**
 * The byte two hex digits spell, or `null` when they do not spell one.
 *
 * `+` is accepted before the digits because `u8::from_str_radix` accepts it, and `%+f` is exactly
 * the kind of trailer that would otherwise decode to one value here and another on the server.
 */
function hexPair(hi: number, lo: number): number | null {
  if (hi === 0x2b /* + */) return hexDigit(lo);
  const high = hexDigit(hi);
  const low = hexDigit(lo);
  return high == null || low == null ? null : high * 16 + low;
}

function hexDigit(byte: number): number | null {
  if (byte >= 0x30 && byte <= 0x39) return byte - 0x30;
  if (byte >= 0x61 && byte <= 0x66) return byte - 0x61 + 10;
  if (byte >= 0x41 && byte <= 0x46) return byte - 0x41 + 10;
  return null;
}
