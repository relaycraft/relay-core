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

/** Which list a panel should draw, and what to call it. */
export interface SseEventSelection {
  /** `live` while the stream is running, `recorded` once it has ended. */
  source: 'live' | 'recorded';
  /** Newest first. */
  events: SseEventRecord[];
  /**
   * How many events the other source holds that this one does not show.
   *
   * A tab that connects to a stream already in progress sees only what arrives after it; a stream
   * that has ended has a complete recorded list. Either way a reader looking at four rows should be
   * able to tell whether that is everything the stream sent, so the count is reported rather than
   * left to be inferred from a number that looks authoritative.
   */
  hidden: number;
}

/**
 * Choose between the live and the recorded lists by the stream's own state.
 *
 * Not by which list is longer: while a stream is running the point of watching it is the events
 * arriving now, and once it has ended the recorded list is the complete one. Deciding by length would
 * flip the panel's meaning as a stream grew, which is exactly the kind of silent change of subject
 * this codebase keeps finding.
 */
export function selectSseEvents(input: {
  ended: boolean;
  live: SseEventRecord[];
  recorded: SseEventRecord[];
}): SseEventSelection {
  const { ended, live, recorded } = input;
  const preferLive = ended ? live.length > 0 && recorded.length === 0 : live.length > 0;
  const source = preferLive ? 'live' : 'recorded';
  const events = [...(source === 'live' ? live : recorded)].reverse();
  const other = source === 'live' ? recorded : live;
  return { source, events, hidden: Math.max(0, other.length - events.length) };
}
