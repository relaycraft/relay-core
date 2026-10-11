import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { isTunnel, setShowTunnels, showTunnels, tunnelQuery, CONNECT_TAG } from '@/lib/tunnels';

/**
 * A stand-in for the browser's `localStorage`.
 *
 * The suite runs in a node environment (`vite.config.ts`), where there is no storage at all — which
 * is also a case this module has to survive, so both are covered below rather than assumed away.
 */
function stubStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
    removeItem: (key: string) => void data.delete(key),
    clear: () => data.clear(),
  });
  return data;
}

describe('tunnels', () => {
  beforeEach(() => {
    stubStorage();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('recognises the CONNECT instruction by its tag', () => {
    expect(isTunnel({ tags: [CONNECT_TAG] })).toBe(true);
    expect(isTunnel({ tags: ['proxy', CONNECT_TAG, 'mitm'] })).toBe(true);
    expect(isTunnel({ tags: ['proxy'] })).toBe(false);
    expect(isTunnel({})).toBe(false);
  });

  it('hides tunnels by default, because a request list should hold requests', () => {
    expect(showTunnels()).toBe(false);
  });

  it('remembers the choice', () => {
    setShowTunnels(true);
    expect(showTunnels()).toBe(true);
    setShowTunnels(false);
    expect(showTunnels()).toBe(false);
  });

  it('treats any stored value that is not "true" as hidden', () => {
    // A stale or hand-edited value must not turn into "show": the default is the safe answer.
    stubStorage({ 'relay-core.show-tunnels': 'yes' });
    expect(showTunnels()).toBe(false);
  });

  it('survives having no storage at all', () => {
    vi.stubGlobal('localStorage', undefined);
    expect(showTunnels()).toBe(false);
    expect(() => setShowTunnels(true)).not.toThrow();
  });

  it('sends the choice to the server explicitly rather than relying on a default', () => {
    // The server defaults to hiding them too, but depending on that would break silently the day
    // the default changed — and a page fetched with the wrong flag is a page of the wrong rows.
    expect(tunnelQuery(false)).toEqual({ include_connect: 'false' });
    expect(tunnelQuery(true)).toEqual({ include_connect: 'true' });
  });
});
