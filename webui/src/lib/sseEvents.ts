/**
 * Events parsed from a *proxied* `text/event-stream`, as `event: sse-event` delivers them.
 *
 * The engine pushes these incrementally — the response body is a stream that may never end, so a
 * capture cannot wait for it — which means they are not on the flow snapshot and the client has to
 * accumulate them itself, exactly as it does for WebSocket frames.
 *
 * The payload is validated here rather than trusted: it arrives as `unknown` off an SSE frame, and a
 * cast at the call site would be a claim the wire has not made. `parseSseEvent` returns `null` for
 * anything it does not recognise, so a newer engine's extra fields — or a malformed frame — cannot
 * put `undefined` into a render path.
 */

/** One event, reduced to what the panel draws. */
export interface SseEventRecord {
  /** The app's own `event:` name, or `message` for the spec's unnamed event. */
  kind: string;
  /** The payload. Kept whole: truncating for display is the renderer's job, not the store's. */
  data: string;
  /** The last-event-id in effect, when the stream set one. */
  id?: string;
  /** The reconnection time in effect, in milliseconds. */
  retry?: number;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

/**
 * Validate one wire payload into a record, or `null` when it is not one.
 *
 * `data` may be the empty string — an event whose `data` line was empty is still an event, and
 * dropping it would make the panel disagree with what the client received.
 */
export function parseSseEvent(raw: unknown): SseEventRecord | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const payload = raw as Record<string, unknown>;

  const data = asString(payload.data);
  if (data === null) return null;

  const record: SseEventRecord = {
    kind: asString(payload.event) ?? 'message',
    data,
  };
  const id = asString(payload.id);
  if (id !== null) record.id = id;
  if (typeof payload.retry === 'number' && Number.isFinite(payload.retry)) {
    record.retry = payload.retry;
  }
  return record;
}
