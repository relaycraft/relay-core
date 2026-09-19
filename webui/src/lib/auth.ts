/**
 * Web UI authentication.
 *
 * The daemon's control API requires a bearer token, and the Web UI cannot use one directly: a
 * browser cannot attach headers to an `EventSource`, and that is how the flow stream arrives.
 *
 * So the daemon prints the UI URL with the token in the **fragment**
 * (`http://127.0.0.1:8082/#token=…`) — fragments are never sent to a server, so the token stays out
 * of request logs — and this module trades it for a cookie that rides along on both `fetch` and
 * `EventSource`.
 *
 * The cookie is `SameSite=Strict` and same-origin: a page on another site cannot use it, and no URL
 * keeps carrying the secret after the first load.
 */

const COOKIE_NAME = 'relay_core_token';

/** Read the token out of the URL fragment, if this load came from a printed URL. */
export function tokenFromFragment(hash: string): string | null {
  const params = new URLSearchParams(hash.replace(/^#/, ''));
  const token = params.get('token');
  return token && token.length > 0 ? token : null;
}

/** Store the token where both `fetch` and `EventSource` will send it. */
export function storeToken(token: string, document_: Document = document): void {
  // `path=/` so the cookie covers the API as well as the UI, and `SameSite=Strict` so no other
  // origin can ride along. Not `HttpOnly`: the UI has to read it back to put it in a header for
  // requests that need one, and `HttpOnly` would only hide it from the very page that must use it.
  document_.cookie = `${COOKIE_NAME}=${encodeURIComponent(token)}; path=/; SameSite=Strict`;
}

/** Does a token already exist for this origin (from a previous load in this browser)? */
export function hasToken(document_: Document = document): boolean {
  return document_.cookie
    .split(';')
    .some((pair) => pair.trim().startsWith(`${COOKIE_NAME}=`));
}

/**
 * Adopt the token from the URL, then remove it from the address bar.
 *
 * Returns true when a token was adopted, so the caller can decide whether to re-fetch data that
 * failed before authentication.
 */
export function initAuth(
  win: Window = window,
  document_: Document = document,
): boolean {
  // `hash` arrives as `#token=…`; keep the rest of the fragment out of consideration.
  const token = tokenFromFragment(win.location.hash);
  if (!token) {
    return false;
  }

  storeToken(token, document_);
  // Drop the secret from the URL: it is in the cookie now, and the address bar is shared, logged by
  // history, and copied into bug reports.
  win.history.replaceState(null, '', win.location.pathname + win.location.search);
  return true;
}

export { COOKIE_NAME };
