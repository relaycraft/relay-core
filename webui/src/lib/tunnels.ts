/**
 * Whether CONNECT tunnels appear in the flow list.
 *
 * A tunnel is a real exchange and the engine records it — it is what the requests inside a tunnel
 * point back at (`parent_flow_id`), and it is where the tunnel's own duration and outcome live. But
 * a *list of requests* that carries one row per HTTPS destination is noise, which is why the
 * reference tools differ here: mitmproxy creates a flow for CONNECT and documents that it produces
 * none of the usual request events, Proxyman keeps connection detail in a panel of its own, and
 * Fiddler lists the tunnel as an ordinary row and is asked how to turn that off.
 *
 * So this follows the first two: recorded always, listed only when asked. The engine defaults to the
 * same answer (`FlowQuery::include_connect`), which is why the list request below sends it rather
 * than filtering a page after the fact — a filter applied after paging disagrees with the page size.
 */
const STORAGE_KEY = 'relay-core.show-tunnels';

/** The tag the engine puts on the CONNECT instruction flow. */
export const CONNECT_TAG = 'connect';

/** Minimal shape this module needs; kept structural so a full `Flow` works too. */
interface Tagged {
  tags?: string[];
}

/** Is this flow the CONNECT instruction that opened a tunnel? */
export function isTunnel(flow: Tagged): boolean {
  return (flow.tags ?? []).includes(CONNECT_TAG);
}

/** The stored choice. Never chosen means "no", because the useful default is the request list. */
export function showTunnels(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'true';
  } catch {
    // Storage can be unavailable; hiding them is the same answer as the default.
    return false;
  }
}

/** Choose whether tunnels are listed, and remember it. */
export function setShowTunnels(show: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, show ? 'true' : 'false');
  } catch {
    // Still honoured for this session even if it cannot be remembered.
  }
}

/**
 * The query parameters a list request needs, so the server and this client agree on one answer.
 *
 * Sent as an explicit `include_connect` rather than relying on the server's default: a client that
 * silently depended on a default would break the day that default changed.
 */
export function tunnelQuery(show: boolean): Record<string, string> {
  return { include_connect: show ? 'true' : 'false' };
}
