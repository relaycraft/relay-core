//! The script contract an agent needs before it calls `set_script`.
//!
//! The probe does not link the script engine. This text is the call shape that engine actually
//! invokes; the probe test below pins the phrases an agent has been getting wrong.

pub fn script_api_guide() -> String {
    r#"# Script API

`set_script` replaces the whole script. It does not restart the daemon. A throw tags only that flow with `script-error`. The next flow is unaffected.

`context` is currently empty. Read `flow`.

## Hooks

- `onRequestHeaders(context, flow)` — return the flow to apply changes, or return nothing to leave it.
- `onResponseHeaders(context, flow)` — return the flow. Header changes apply. Leave `response.body` unset to keep streaming the upstream body.
- `onRequest(body, flow)` — `body` is the first argument, not the third. `body.text()` and `body.json()` read a buffered copy up to 1 MiB. Return the flow.
- `onResponse(body, flow)` — same shape as `onRequest`. Return the flow.
- `onWebSocketMessage(context, flow, message)` — return the message, the string `DROP`, or nothing. Returning the flow is the HTTP header-hook shape and tags only that flow. A WebSocket flow's layer type is `WebSocket`, not `Http`.
- `onConnect(context, conn)` — return nothing to allow, or `{ "drop": true, "reason": "..." }` to drop. `conn` has `id`, `client_addr`, `server_addr`, `tls_sni`.
- `onDisconnect(context, conn, stats)` — `stats` has `duration_ms`, `bytes_sent`, `bytes_received`, `flows_count`. The return value is ignored.
- `onWebSocketStart(context, flow)`
- `onWebSocketEnd(context, flow, closeCode, reason)`
- `onWebSocketError(context, flow, error)`
- `onError(context, flow, error, stage)` — `error` and `stage` are strings.

`onRequest` and `onResponse` may be async. A hook that does not finish is aborted and the original body is still forwarded.

When `onResponseHeaders` or `onResponse` is defined, the decoded response body is on the flow before those hooks run.

## Flow

An HTTP flow's layer is `{ "type": "Http", "data": { "request", "response" } }`.

```js
const http = flow.layer.data;
http.request.method = "POST";
http.request.url = "https://example.com/v2/health";
http.request.headers.push(["X-Test", "1"]);
http.response.body = { encoding: "utf-8", content: "rewritten", size: 9 };
```

`headers` is an array of `[name, value]` pairs. `request.body` and `response.body` are `{ encoding, content, size }` or null.

`content` is plaintext unless `encoding` is `"base64"`, in which case `content` is standard base64. `atob` and `btoa` are the browser Latin-1 globals (`btoa("hi") === "aGk="`). `relay.base64.encode` and `relay.base64.decode` are UTF-8. They are not substitutes for `atob` and `btoa`.

A WebSocket flow uses `flow.layer.data.handshake_request`, not `flow.layer.data.request`.

## relay and sharedState

- `relay.log(...)`
- `relay.env(name)` — returns the value only when that name is on the daemon's script env allowlist. Otherwise `undefined`.
- `relay.uuid()`
- `relay.hash(alg, data)` — `alg` is `sha1`, `sha256`, `sha512`, or `md5`.
- `relay.json.parseSafe(str)` and `relay.json.stringifyPretty(obj)`
- `relay.fetch(url)` — off unless the daemon was started with a host allowlist. The result is JSON. A disabled fetch is `{ "ok": false, "error": "script fetch disabled" }`. A host that is not listed is `{ "ok": false, "error": "host not in allowlist" }`. `*` allows any host.
- `sharedState.get(key)`, `set(key, value)`, `delete(key)`, `clear()`, `keys()`, `size()` — one map for this isolate.
"#
    .to_string()
}
