import { describe, it, expect } from 'vitest';
import { parseSseEvent } from '@/lib/sseEvents';

describe('parseSseEvent', () => {
  it('reads a named event with its payload', () => {
    expect(parseSseEvent({ event: 'tick', data: '{"n":1}', id: '7', retry: 1500 })).toEqual({
      kind: 'tick',
      data: '{"n":1}',
      id: '7',
      retry: 1500,
    });
  });

  it('calls an unnamed event by the name the spec gives it', () => {
    // The SSE spec calls an event with no `event:` field a "message"; showing a blank column for it
    // would leave the reader to guess whether the field was missing or empty.
    expect(parseSseEvent({ data: 'hello' })?.kind).toBe('message');
  });

  it('keeps an event whose payload is empty', () => {
    // An empty `data:` line is still an event the client received. Dropping it would make the panel
    // disagree with the stream.
    const record = parseSseEvent({ event: 'ping', data: '' });
    expect(record).toEqual({ kind: 'ping', data: '' });
  });

  it('omits id and retry rather than inventing them', () => {
    const record = parseSseEvent({ data: 'x' });
    expect(record).not.toHaveProperty('id');
    expect(record).not.toHaveProperty('retry');
  });

  it('rejects a payload that is not an event', () => {
    // Everything here arrives as `unknown` off an SSE frame. A cast at the call site would assert a
    // shape the wire never promised, and the value would reach a render path.
    expect(parseSseEvent(null)).toBeNull();
    expect(parseSseEvent('data: x')).toBeNull();
    expect(parseSseEvent({})).toBeNull();
    expect(parseSseEvent({ event: 'tick' })).toBeNull();
    expect(parseSseEvent({ data: 42 })).toBeNull();
  });

  it('ignores a non-numeric retry instead of coercing it', () => {
    expect(parseSseEvent({ data: 'x', retry: 'soon' })).not.toHaveProperty('retry');
    expect(parseSseEvent({ data: 'x', retry: Number.NaN })).not.toHaveProperty('retry');
  });

  it('tolerates fields a newer engine adds', () => {
    const record = parseSseEvent({ data: 'x', comment: 'whatever', id: '3' });
    expect(record).toEqual({ kind: 'message', data: 'x', id: '3' });
  });
});
