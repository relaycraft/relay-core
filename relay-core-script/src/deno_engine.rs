use crate::engine_trait::ScriptEngineTrait;
use crate::streams::{self, MemoryBodyResource};
use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use deno_core::{
    Extension, JsRuntime, Op, OpState, ResourceId, RuntimeOptions, error::AnyError, op2,
};
use relay_core_api::flow::{BodyData, Flow, Layer, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, RequestAction,
    ResponseAction, WebSocketMessageAction,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Bytes copied for a script before `text()` / `json()` refuse the rest.
/// Matches the default rule body inspect budget.
const SCRIPT_BODY_BUDGET: usize = 1024 * 1024;

/// A hook that neither settles nor fails is aborted so it cannot pin the isolate queue.
const DEFAULT_SCRIPT_HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// `JsRuntime::resolve` only watches the promise. An `async` hook stays pending forever
/// unless something pumps the event loop; sync returns are not promises and skip that wait.
async fn settle_promise(
    runtime: &mut JsRuntime,
    value: deno_core::v8::Global<deno_core::v8::Value>,
) -> Result<deno_core::v8::Global<deno_core::v8::Value>, AnyError> {
    let pending = runtime.resolve(value);
    runtime
        .with_event_loop_promise(pending, Default::default())
        .await
}

#[op2(fast)]
fn op_log_level(#[string] level: String, #[string] msg: String) {
    match level.as_str() {
        "error" => tracing::error!("[User Script] {}", msg),
        "warn" => tracing::warn!("[User Script] {}", msg),
        "info" => tracing::info!("[User Script] {}", msg),
        "debug" => tracing::debug!("[User Script] {}", msg),
        _ => tracing::info!("[User Script] {}", msg),
    }
}

/// Test and internal escape hatch. Not part of the script-facing `relay` API.
/// Capped so a script cannot park the isolate longer than the hook timeout by much.
#[op2(async)]
async fn op_wait_ms(#[smi] ms: u32) {
    let capped = u64::from(ms.min(30_000));
    tokio::time::sleep(Duration::from_millis(capped)).await;
}

#[op2(async)]
#[buffer]
async fn op_read_body(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[smi] limit: usize,
) -> Result<Vec<u8>, AnyError> {
    let resource = {
        let state = state.borrow();
        state.resource_table.get_any(rid)?
    };
    let view = resource.read(limit).await?;
    Ok(view.to_vec())
}

#[op2]
#[string]
fn op_decode_utf8(#[buffer] bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[op2(fast)]
fn op_close_body(state: &mut OpState, #[smi] rid: ResourceId) {
    state.resource_table.take_any(rid).ok();
}

// ── S1: sharedState ops ────────────────────────────────────────

/// Soft cap for sharedState keys — exceeded triggers a warning but does not block.
const SHARED_STATE_SOFT_CAP: usize = 10_000;

fn shared_state(state: &mut OpState) -> &mut HashMap<String, serde_json::Value> {
    state.borrow_mut::<HashMap<String, serde_json::Value>>()
}

#[op2]
#[serde]
fn op_shared_state_get(state: &mut OpState, #[string] key: String) -> Option<serde_json::Value> {
    shared_state(state).get(&key).cloned()
}

#[op2]
fn op_shared_state_set(
    state: &mut OpState,
    #[string] key: String,
    #[serde] value: serde_json::Value,
) {
    let map = shared_state(state);
    // Soft cap warning — does not block, but alerts users to clean up
    if !map.contains_key(&key) && map.len() >= SHARED_STATE_SOFT_CAP {
        tracing::warn!(
            "sharedState exceeds soft cap ({} keys): user scripts should delete unused keys. \
             Use sharedState.size() to check.",
            map.len()
        );
    }
    map.insert(key, value);
}

#[op2(fast)]
fn op_shared_state_delete(state: &mut OpState, #[string] key: String) -> bool {
    shared_state(state).remove(&key).is_some()
}

#[op2(fast)]
fn op_shared_state_clear(state: &mut OpState) {
    shared_state(state).clear();
}

#[op2]
#[serde]
fn op_shared_state_keys(state: &mut OpState) -> Vec<String> {
    shared_state(state).keys().cloned().collect()
}

#[op2(fast)]
fn op_shared_state_size(state: &OpState) -> u32 {
    let map = state.borrow::<HashMap<String, serde_json::Value>>();
    map.len() as u32
}

// ── S5: relay.env — whitelisted environment variable access ────

/// Counter for env access attempts — exposed via Prometheus
static SCRIPT_ENV_ACCESS_TOTAL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn env_allow(state: &OpState) -> &HashSet<String> {
    state.borrow::<HashSet<String>>()
}

#[op2]
#[string]
fn op_env_get(state: &OpState, #[string] key: String) -> Option<String> {
    let allowed = env_allow(state);
    // Increment access counter regardless of allow/deny
    SCRIPT_ENV_ACCESS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if allowed.is_empty() || !allowed.contains(&key) {
        return None;
    }
    std::env::var(&key).ok()
}

/// Get the total number of relay.env access attempts
pub fn get_script_env_access_total() -> usize {
    SCRIPT_ENV_ACCESS_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

// ── S7a: relay.fetch — async sub-requests (default disabled) ─────

/// Configuration for relay.fetch sub-requests.
#[derive(Clone)]
pub struct ScriptFetchConfig {
    /// Whether relay.fetch is enabled at all.
    pub enabled: bool,
    /// Allowed target hostnames. Empty means all allowed (if enabled).
    pub allow_hosts: HashSet<String>,
    /// Maximum concurrent fetch requests (semaphore permits).
    pub max_concurrency: usize,
    /// Timeout per request in milliseconds.
    pub timeout_ms: u64,
    /// Proxy listen port — used to prevent recursive fetch to self.
    pub proxy_listen_port: u16,
}

impl Default for ScriptFetchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_hosts: HashSet::new(),
            max_concurrency: 8,
            timeout_ms: 5000,
            proxy_listen_port: 0,
        }
    }
}

fn fetch_config(state: &OpState) -> &ScriptFetchConfig {
    state.borrow::<ScriptFetchConfig>()
}

/// Metrics for relay.fetch
static SCRIPT_FETCH_TOTAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SCRIPT_FETCH_REJECTED_TOTAL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Fetch validation result — always returns a JSON string (never throws)
/// so it works in synchronous onRequestHeaders context.
#[op2]
#[string]
fn op_script_fetch(state: &OpState, #[string] url: String) -> String {
    SCRIPT_FETCH_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let config = fetch_config(state);

    if !config.enabled {
        SCRIPT_FETCH_REJECTED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return serde_json::json!({"ok": false, "error": "script fetch disabled"}).to_string();
    }

    if !config.allow_hosts.is_empty()
        && let Ok(parsed) = url::Url::parse(&url)
        && let Some(host) = parsed.host_str()
        && !config.allow_hosts.contains(host)
    {
        SCRIPT_FETCH_REJECTED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return serde_json::json!({"ok": false, "error": "host not in allowlist"}).to_string();
    }

    if let Ok(parsed) = url::Url::parse(&url)
        && let Some(port) = parsed.port_or_known_default()
        && port == config.proxy_listen_port
        && matches!(parsed.scheme(), "http" | "https")
    {
        let host = parsed.host_str().unwrap_or("");
        if host == "localhost" || host == "127.0.0.1" || host == "::1" {
            SCRIPT_FETCH_REJECTED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return serde_json::json!({"ok": false, "error": "recursive fetch to self rejected"})
                .to_string();
        }
    }

    // Minimal HTTP GET client for relay.fetch. Uses ureq (blocking) for simplicity.
    // The V8 isolate runs on its own dedicated thread, so blocking is acceptable.
    // Async sub-requests with response streaming are deferred to 1.x.
    let timeout = std::time::Duration::from_millis(config.timeout_ms);
    let agent = ureq::AgentBuilder::new()
        .timeout_read(timeout)
        .timeout_connect(timeout)
        .build();
    match agent.get(&url).call() {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().unwrap_or_default();
            serde_json::json!({"ok": true, "status": status, "body": body}).to_string()
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            serde_json::json!({"ok": false, "status": code, "body": body, "error": format!("HTTP {}", code)}).to_string()
        }
        Err(e) => {
            SCRIPT_FETCH_REJECTED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            serde_json::json!({"ok": false, "error": e.to_string()}).to_string()
        }
    }
}

/// Get the total number of relay.fetch attempts
pub fn get_script_fetch_total() -> usize {
    SCRIPT_FETCH_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Get the total number of rejected relay.fetch attempts
pub fn get_script_fetch_rejected_total() -> usize {
    SCRIPT_FETCH_REJECTED_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

// ── S6: relay utility ops — uuid, hash, base64, json ──────────

#[op2]
#[string]
fn op_uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[op2]
#[string]
fn op_hash(#[string] algorithm: String, #[string] data: String) -> Result<String, AnyError> {
    use sha1::Sha1;
    use sha2::{Sha256, Sha512};
    let bytes = data.as_bytes();
    let hex = match algorithm.as_str() {
        "sha1" => {
            use sha1::Digest;
            data_encoding::HEXLOWER.encode(&Sha1::digest(bytes))
        }
        "sha256" => {
            use sha2::Digest;
            data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
        }
        "sha512" => {
            use sha2::Digest;
            data_encoding::HEXLOWER.encode(&Sha512::digest(bytes))
        }
        "md5" => {
            use md5::Digest;
            data_encoding::HEXLOWER.encode(&md5::Md5::digest(bytes))
        }
        other => {
            return Err(AnyError::msg(format!(
                "unsupported hash algorithm: {}. Supported: sha1, sha256, sha512, md5",
                other
            )));
        }
    };
    Ok(hex)
}

#[op2]
#[string]
fn op_base64_encode(#[string] data: String) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

#[op2]
#[string]
fn op_base64_decode(#[string] data: String) -> Result<String, AnyError> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| AnyError::msg(format!("base64 decode error: {}", e)))?;
    String::from_utf8(bytes).map_err(|e| AnyError::msg(format!("utf-8 error: {}", e)))
}

#[op2]
#[serde]
fn op_json_parse_safe(#[string] data: String) -> serde_json::Value {
    serde_json::from_str(&data).unwrap_or(serde_json::Value::Null)
}

#[op2]
#[string]
fn op_json_stringify_pretty(#[serde] value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::new())
}

#[derive(Debug, Clone, Copy)]
enum BodyHookKind {
    Request,
    Response,
}

type BodyHookReply = oneshot::Sender<Result<(Option<Flow>, Option<Bytes>), String>>;

enum DenoCommand {
    LoadScript(String, oneshot::Sender<Result<(), String>>),
    /// `true` when `globalThis[name]` is a function. Name is one of the engine's own hook names.
    HasHook(BodyHookKind, oneshot::Sender<bool>),
    OnConnect(
        ConnectionInfo,
        oneshot::Sender<Result<ConnectAction, String>>,
    ),
    OnDisconnect(
        ConnectionInfo,
        ConnectionStats,
        oneshot::Sender<Result<(), String>>,
    ),
    OnRequestHeaders(Flow, oneshot::Sender<Result<Option<Flow>, String>>),
    OnRequest(Flow, Bytes, bool, BodyHookReply),
    OnResponseHeaders(Flow, oneshot::Sender<Result<Option<Flow>, String>>),
    OnResponse(Flow, Bytes, bool, BodyHookReply),
    OnWebSocketMessage(
        Flow,
        WebSocketMessage,
        oneshot::Sender<Result<WebSocketMessageAction, String>>,
    ),
    OnWebSocketStart(Flow, oneshot::Sender<Result<Option<Flow>, String>>),
    OnWebSocketEnd(
        Flow,
        u16,
        String,
        oneshot::Sender<Result<Option<Flow>, String>>,
    ),
    OnWebSocketError(Flow, String, oneshot::Sender<Result<Option<Flow>, String>>),
}

#[derive(Clone)]
pub struct DenoScriptEngine {
    tx: mpsc::Sender<DenoCommand>,
}

impl Default for DenoScriptEngine {
    fn default() -> Self {
        Self::new(HashSet::new())
    }
}

fn build_js_runtime(env_allow: HashSet<String>, fetch_config: ScriptFetchConfig) -> JsRuntime {
    let ext = Extension {
        name: "relay_core",
        ops: std::borrow::Cow::Borrowed(&[
            op_log_level::DECL,
            op_wait_ms::DECL,
            op_read_body::DECL,
            op_decode_utf8::DECL,
            op_close_body::DECL,
            op_shared_state_get::DECL,
            op_shared_state_set::DECL,
            op_shared_state_delete::DECL,
            op_shared_state_clear::DECL,
            op_shared_state_keys::DECL,
            op_shared_state_size::DECL,
            op_env_get::DECL,
            op_uuid_v4::DECL,
            op_hash::DECL,
            op_base64_encode::DECL,
            op_base64_decode::DECL,
            op_json_parse_safe::DECL,
            op_json_stringify_pretty::DECL,
            op_script_fetch::DECL,
        ]),
        op_state_fn: Some(Box::new({
            let env_allow = env_allow.clone();
            let fetch_config = fetch_config.clone();
            move |state| {
                state.put(HashMap::<String, serde_json::Value>::new());
                state.put(env_allow.clone());
                state.put(fetch_config.clone());
            }
        })),
        ..Default::default()
    };

    let mut js_runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![ext],
        ..Default::default()
    });

    // Bootstrap JS — S1/S3: sharedState + console levels
    let bootstrap = r#"
    globalThis.console = {
        log: (...args) => {
            Deno.core.ops.op_log_level("log", _format(args));
        },
        info: (...args) => {
            Deno.core.ops.op_log_level("info", _format(args));
        },
        warn: (...args) => {
            Deno.core.ops.op_log_level("warn", _format(args));
        },
        error: (...args) => {
            Deno.core.ops.op_log_level("error", _format(args));
        },
        debug: (...args) => {
            Deno.core.ops.op_log_level("debug", _format(args));
        },
    };

    function _format(args) {
        return args.map(arg => {
            if (typeof arg === 'object') {
                try { return JSON.stringify(arg); }
                catch { return String(arg); }
            }
            return String(arg);
        }).join(" ");
    }

    class RelayBody {
        constructor(rid, truncated) {
            this.rid = rid;
            this.truncated = !!truncated;
        }
        async read(limit) {
            return await Deno.core.ops.op_read_body(this.rid, limit || 65536);
        }
        close() {
            Deno.core.ops.op_close_body(this.rid);
        }
                        async text() {
                            if (this.truncated) {
                                throw new Error("body exceeds script inspect budget");
                            }
                            const bytes = await this.read(10 * 1024 * 1024);
                            return Deno.core.ops.op_decode_utf8(bytes);
                        }
        async json() {
            const txt = await this.text();
            return JSON.parse(txt);
        }
    }
    globalThis.RelayBody = RelayBody;

    // Web globals the scripting guide uses. Each character is one byte (0–255),
    // the same contract as a browser: btoa("hi") === "aGk=".
    const _b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    globalThis.btoa = function (input) {
        const s = String(input);
        let out = "";
        for (let i = 0; i < s.length; i += 3) {
            const c0 = s.charCodeAt(i);
            const c1 = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
            const c2 = i + 2 < s.length ? s.charCodeAt(i + 2) : 0;
            if (c0 > 255 || (i + 1 < s.length && c1 > 255) || (i + 2 < s.length && c2 > 255)) {
                throw new TypeError("btoa failed: character out of range");
            }
            const n = (c0 << 16) | (c1 << 8) | c2;
            out += _b64[(n >> 18) & 63] + _b64[(n >> 12) & 63]
                + (i + 1 < s.length ? _b64[(n >> 6) & 63] : "=")
                + (i + 2 < s.length ? _b64[n & 63] : "=");
        }
        return out;
    };
    globalThis.atob = function (input) {
        const s = String(input).replace(/[\t\n\f\r ]/g, "");
        if (s.length % 4 !== 0) {
            throw new TypeError("atob failed: length is not a multiple of 4");
        }
        let out = "";
        for (let i = 0; i < s.length; i += 4) {
            const chars = [s[i], s[i + 1], s[i + 2], s[i + 3]];
            const vals = chars.map((ch) => (ch === "=" ? 0 : _b64.indexOf(ch)));
            if (vals[0] < 0 || vals[1] < 0 || (chars[2] !== "=" && vals[2] < 0) || (chars[3] !== "=" && vals[3] < 0)) {
                throw new TypeError("atob failed: invalid character");
            }
            const n = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
            out += String.fromCharCode((n >> 16) & 255);
            if (chars[2] !== "=") out += String.fromCharCode((n >> 8) & 255);
            if (chars[3] !== "=") out += String.fromCharCode(n & 255);
        }
        return out;
    };

    globalThis.relay = {
        log: globalThis.console.log,
        env: function(name) {
            return Deno.core.ops.op_env_get(name) ?? undefined;
        },
        uuid: function() {
            return Deno.core.ops.op_uuid_v4();
        },
        hash: function(alg, data) {
            return Deno.core.ops.op_hash(alg, data);
        },
        base64: {
            encode: function(data) {
                return Deno.core.ops.op_base64_encode(data);
            },
            decode: function(data) {
                return Deno.core.ops.op_base64_decode(data);
            },
        },
        json: {
            parseSafe: function(str) {
                return Deno.core.ops.op_json_parse_safe(str);
            },
            stringifyPretty: function(obj) {
                return Deno.core.ops.op_json_stringify_pretty(obj);
            },
        },
        fetch: function(url) {
            return JSON.parse(Deno.core.ops.op_script_fetch(url));
        },
    };

    // S12a: ctx.setTag / ctx.setVariable (script→rule injection) deferred to 1.x.
    // Cross-thread synchronous V8 callback into the rule execution engine would
    // require architectural changes that risk rule engine atomicity.
    // Script-side rule context reading (flow.matched_rules, flow.rule_variables)
    // is fully supported (S10a/S11).

    // S1: sharedState — cross-hook shared map per isolate
    globalThis.sharedState = {
        get(key) {
            return Deno.core.ops.op_shared_state_get(key);
        },
        set(key, value) {
            Deno.core.ops.op_shared_state_set(key, value);
        },
        delete(key) {
            return Deno.core.ops.op_shared_state_delete(key);
        },
        clear() {
            Deno.core.ops.op_shared_state_clear();
        },
        keys() {
            return Deno.core.ops.op_shared_state_keys();
        },
        size() {
            return Deno.core.ops.op_shared_state_size();
        },
    };
"#;
    js_runtime.execute_script("bootstrap", bootstrap).unwrap();
    js_runtime
}

fn discard_body_resource(runtime: &mut JsRuntime, rid: ResourceId) {
    let op_state_rc = runtime.op_state();
    let mut state = op_state_rc.borrow_mut();
    state.resource_table.take::<MemoryBodyResource>(rid).ok();
}

fn authored_body(flow: &Flow, response: bool) -> Option<Bytes> {
    let Layer::Http(http) = &flow.layer else {
        return None;
    };
    let data: &BodyData = if response {
        http.response.as_ref().and_then(|resp| resp.body.as_ref())?
    } else {
        http.request.body.as_ref()?
    };
    if data.content.is_empty() {
        return None;
    }
    Some(decode_body_content(data))
}

fn decode_body_content(data: &BodyData) -> Bytes {
    if data.encoding == "base64" {
        base64::engine::general_purpose::STANDARD
            .decode(&data.content)
            .unwrap_or_default()
            .into()
    } else {
        Bytes::from(data.content.clone())
    }
}

fn note_truncated(flow: &mut Flow, truncated: bool) {
    if truncated
        && !flow
            .tags
            .iter()
            .any(|tag| tag == "script_skipped:body_truncated")
    {
        flow.tags.push("script_skipped:body_truncated".to_string());
    }
}

async fn load_user_script(runtime: &mut JsRuntime, script: &str) -> Result<(), String> {
    runtime
        .execute_script("<anon>", script.to_string())
        .map_err(|e| e.to_string())?;
    runtime
        .run_event_loop(Default::default())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

impl DenoScriptEngine {
    pub fn new(env_allow: HashSet<String>) -> Self {
        Self::new_with_fetch(env_allow, ScriptFetchConfig::default())
    }

    pub fn new_with_fetch(env_allow: HashSet<String>, fetch_config: ScriptFetchConfig) -> Self {
        Self::with_hook_timeout(env_allow, fetch_config, DEFAULT_SCRIPT_HOOK_TIMEOUT)
    }

    pub(crate) fn with_hook_timeout(
        env_allow: HashSet<String>,
        fetch_config: ScriptFetchConfig,
        hook_timeout: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel(32);

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                let mut js_runtime = build_js_runtime(env_allow.clone(), fetch_config.clone());

                macro_rules! drive_body_hook {
                    ($kind:expr, $flow:expr, $bytes:expr, $truncated:expr, $resp:expr) => {{
                        let timed = tokio::time::timeout(
                            hook_timeout,
                            Self::handle_body_hook(
                                &mut js_runtime,
                                $kind,
                                $flow,
                                $bytes,
                                $truncated,
                            ),
                        )
                        .await;
                        match timed {
                            Ok(res) => {
                                let _ = $resp.send(res);
                            }
                            Err(_) => {
                                // Cancelling the hook leaves this isolate in place. Building a
                                // replacement first and then dropping the cancelled one panics:
                                // V8 requires isolates to be dropped in reverse creation order.
                                let _ = $resp.send(Err("script hook timed out".to_string()));
                            }
                        }
                    }};
                }

                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        DenoCommand::LoadScript(script, resp) => {
                            let res = load_user_script(&mut js_runtime, &script).await;
                            let _ = resp.send(res);
                        }
                        DenoCommand::HasHook(kind, resp) => {
                            let code = match kind {
                                BodyHookKind::Request => {
                                    "typeof globalThis.onRequest === 'function'"
                                }
                                BodyHookKind::Response => {
                                    "typeof globalThis.onResponse === 'function'"
                                }
                            };
                            let exists = match js_runtime.execute_script("check_hook", code) {
                                Ok(val) => {
                                    let scope = &mut js_runtime.handle_scope();
                                    deno_core::v8::Local::new(scope, val).is_true()
                                }
                                Err(_) => false,
                            };
                            let _ = resp.send(exists);
                        }
                        DenoCommand::OnRequestHeaders(flow, resp) => {
                            let res = Self::handle_on_request_headers(&mut js_runtime, flow);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnRequest(flow, bytes, truncated, resp) => {
                            drive_body_hook!(BodyHookKind::Request, flow, bytes, truncated, resp);
                        }
                        DenoCommand::OnResponseHeaders(flow, resp) => {
                            let res = Self::handle_on_response_headers(&mut js_runtime, flow);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnResponse(flow, bytes, truncated, resp) => {
                            drive_body_hook!(BodyHookKind::Response, flow, bytes, truncated, resp);
                        }
                        DenoCommand::OnWebSocketMessage(flow, message, resp) => {
                            let res =
                                Self::handle_on_websocket_message(&mut js_runtime, flow, message);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnConnect(conn, resp) => {
                            let res = Self::handle_on_connect(&mut js_runtime, conn);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnDisconnect(conn, stats, resp) => {
                            let res = Self::handle_on_disconnect(&mut js_runtime, conn, stats);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnWebSocketStart(flow, resp) => {
                            let res = Self::handle_on_websocket_start(&mut js_runtime, flow);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnWebSocketEnd(flow, code, reason, resp) => {
                            let res =
                                Self::handle_on_websocket_end(&mut js_runtime, flow, code, reason);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnWebSocketError(flow, error, resp) => {
                            let res = Self::handle_on_websocket_error(&mut js_runtime, flow, error);
                            let _ = resp.send(res);
                        }
                    }
                }
            });
        });

        Self { tx }
    }

    // ── S2: onError dispatch ─────────────────────────────────

    fn try_call_on_error(runtime: &mut JsRuntime, flow: &Flow, error: &str, stage: &str) {
        let check_code = "typeof globalThis.onError === 'function'";
        let exists = runtime
            .execute_script("check_onError_v2", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                let val = deno_core::v8::Local::new(&mut scope, v);
                val.is_true()
            })
            .unwrap_or(false);

        if !exists {
            return;
        }

        let flow_json = match serde_json::to_string(flow) {
            Ok(j) => j,
            Err(_) => return,
        };
        let error_escaped = error.replace('\\', "\\\\").replace('\'', "\\'");
        let code = format!(
            "globalThis.onError({{}}, {}, '{}', '{}')",
            flow_json, error_escaped, stage
        );

        let _ = runtime.execute_script("call_onError_v2", code);
    }

    fn handle_on_request_headers(
        runtime: &mut JsRuntime,
        flow: Flow,
    ) -> Result<Option<Flow>, String> {
        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let check_code = "typeof globalThis.onRequestHeaders === 'function'";
        let exists = runtime
            .execute_script("check_onRequestHeaders", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onRequestHeaders");
                e.to_string()
            })?;
        {
            let mut scope = runtime.handle_scope();
            let exists_val = deno_core::v8::Local::new(&mut scope, exists);
            if !exists_val.is_true() {
                return Ok(None);
            }
        }
        let code = format!("globalThis.onRequestHeaders({{}}, {})", flow_json);
        let result = runtime
            .execute_script("call_onRequestHeaders", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onRequestHeaders");
                e.to_string()
            })?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Result<Flow, _> = deno_core::serde_v8::from_v8(&mut scope, result_val);
        drop(scope);
        let modified_flow = match deser {
            Ok(f) => f,
            Err(e) => {
                let err_str = format!("Failed to deserialize flow: {}", e);
                Self::try_call_on_error(runtime, &flow, &err_str, "onRequestHeaders");
                return Err(err_str);
            }
        };
        Ok(Some(modified_flow))
    }

    fn handle_on_response_headers(
        runtime: &mut JsRuntime,
        flow: Flow,
    ) -> Result<Option<Flow>, String> {
        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let check_code = "typeof globalThis.onResponseHeaders === 'function'";
        let exists = runtime
            .execute_script("check_onResponseHeaders", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onResponseHeaders");
                e.to_string()
            })?;
        {
            let mut scope = runtime.handle_scope();
            let exists_val = deno_core::v8::Local::new(&mut scope, exists);
            if !exists_val.is_true() {
                return Ok(None);
            }
        }
        let code = format!("globalThis.onResponseHeaders({{}}, {})", flow_json);
        let result = runtime
            .execute_script("call_onResponseHeaders", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onResponseHeaders");
                e.to_string()
            })?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Result<Flow, _> = deno_core::serde_v8::from_v8(&mut scope, result_val);
        drop(scope);
        let modified_flow = match deser {
            Ok(f) => f,
            Err(e) => {
                let err_str = format!("Failed to deserialize flow: {}", e);
                Self::try_call_on_error(runtime, &flow, &err_str, "onResponseHeaders");
                return Err(err_str);
            }
        };
        Ok(Some(modified_flow))
    }

    async fn handle_body_hook(
        runtime: &mut JsRuntime,
        kind: BodyHookKind,
        flow: Flow,
        visible: Bytes,
        truncated: bool,
    ) -> Result<(Option<Flow>, Option<Bytes>), String> {
        let stage = match kind {
            BodyHookKind::Request => "onRequest",
            BodyHookKind::Response => "onResponse",
        };
        let resource = MemoryBodyResource::new(visible);
        let rid = {
            let op_state_rc = runtime.op_state();
            let mut state = op_state_rc.borrow_mut();
            state.resource_table.add(resource)
        };

        let invoked = Self::invoke_body_hook(runtime, stage, &flow, rid, truncated).await;
        discard_body_resource(runtime, rid);
        let modified = invoked?;
        let replacement = modified
            .as_ref()
            .and_then(|flow| authored_body(flow, matches!(kind, BodyHookKind::Response)));
        Ok((modified, replacement))
    }

    async fn invoke_body_hook(
        runtime: &mut JsRuntime,
        stage: &str,
        flow: &Flow,
        rid: ResourceId,
        truncated: bool,
    ) -> Result<Option<Flow>, String> {
        let flow_json = serde_json::to_string(flow).map_err(|e| e.to_string())?;
        let check_code = format!("typeof globalThis.{stage} === 'function'");
        let exists = runtime
            .execute_script("check_body_hook", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, flow, &e.to_string(), stage);
                e.to_string()
            })?;
        let exists_bool = {
            let scope = &mut runtime.handle_scope();
            deno_core::v8::Local::new(scope, exists).is_true()
        };
        if !exists_bool {
            return Ok(None);
        }

        let truncated_lit = if truncated { "true" } else { "false" };
        let code =
            format!("globalThis.{stage}(new RelayBody({rid}, {truncated_lit}), {flow_json})");
        let result = runtime
            .execute_script("call_body_hook", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, flow, &e.to_string(), stage);
                e.to_string()
            })?;
        let result = settle_promise(runtime, result).await.map_err(|e| {
            Self::try_call_on_error(runtime, flow, &e.to_string(), stage);
            e.to_string()
        })?;

        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Result<Flow, _> = deno_core::serde_v8::from_v8(&mut scope, result_val);
        drop(scope);
        match deser {
            Ok(flow) => Ok(Some(flow)),
            Err(e) => {
                let err_str = format!("Failed to deserialize flow: {e}");
                Self::try_call_on_error(runtime, flow, &err_str, stage);
                Err(err_str)
            }
        }
    }

    fn handle_on_websocket_message(
        runtime: &mut JsRuntime,
        flow: Flow,
        message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, String> {
        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let message_json = serde_json::to_string(&message).map_err(|e| e.to_string())?;

        let check_code = "typeof globalThis.onWebSocketMessage === 'function'";
        let exists = runtime
            .execute_script("check_onWebSocketMessage", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onWebSocketMessage");
                e.to_string()
            })?;
        {
            let mut scope = runtime.handle_scope();
            let exists_val = deno_core::v8::Local::new(&mut scope, exists);
            if !exists_val.is_true() {
                return Ok(WebSocketMessageAction::Continue(message));
            }
        }

        let code = format!(
            "globalThis.onWebSocketMessage({{}}, {}, {})",
            flow_json, message_json
        );
        let result = runtime
            .execute_script("call_onWebSocketMessage", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onWebSocketMessage");
                e.to_string()
            })?;

        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);

        if result_val.is_undefined() || result_val.is_null() {
            return Ok(WebSocketMessageAction::Continue(message));
        }

        if result_val.is_string() {
            let s = result_val.to_rust_string_lossy(&mut scope);
            if s == "DROP" {
                return Ok(WebSocketMessageAction::Drop);
            }
        }

        let deser: Result<WebSocketMessage, _> =
            deno_core::serde_v8::from_v8(&mut scope, result_val);
        drop(scope);
        let modified_message = match deser {
            Ok(m) => m,
            Err(e) => {
                let err_str = format!(
                    "onWebSocketMessage must return the message, \"DROP\", or nothing (got {e}). \
                     Returning the flow is the HTTP header-hook shape and tags only this flow"
                );
                Self::try_call_on_error(runtime, &flow, &err_str, "onWebSocketMessage");
                return Err(err_str);
            }
        };

        Ok(WebSocketMessageAction::Continue(modified_message))
    }

    fn handle_on_connect(
        runtime: &mut JsRuntime,
        conn: ConnectionInfo,
    ) -> Result<ConnectAction, String> {
        let check_code = "typeof globalThis.onConnect === 'function'";
        let exists = runtime
            .execute_script("check_onConnect", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                deno_core::v8::Local::new(&mut scope, v).is_true()
            })
            .unwrap_or(false);
        if !exists {
            return Ok(ConnectAction::Allow);
        }

        let conn_json = serde_json::json!({
            "id": conn.id.to_string(),
            "client_addr": conn.client_addr.to_string(),
            "server_addr": conn.server_addr.map(|a| a.to_string()),
            "tls_sni": conn.tls_sni,
        });
        let code = format!(
            "globalThis.onConnect({{}}, {})",
            serde_json::to_string(&conn_json).unwrap_or_default()
        );
        let result = runtime
            .execute_script("call_onConnect", code)
            .map_err(|e| {
                tracing::warn!("onConnect script error: {}", e);
                e.to_string()
            })?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(ConnectAction::Allow);
        }
        if result_val.is_object() {
            let Some(obj) = result_val.to_object(&mut scope) else {
                return Ok(ConnectAction::Allow);
            };
            let drop_key: deno_core::v8::Local<deno_core::v8::Value> =
                deno_core::v8::String::new(&mut scope, "drop")
                    .expect("v8 string")
                    .into();
            if let Some(drop_val) = obj.get(&mut scope, drop_key)
                && drop_val.is_true()
            {
                let reason_key: deno_core::v8::Local<deno_core::v8::Value> =
                    deno_core::v8::String::new(&mut scope, "reason")
                        .expect("v8 string")
                        .into();
                let reason = obj
                    .get(&mut scope, reason_key)
                    .map(|v| v.to_rust_string_lossy(&mut scope))
                    .unwrap_or_else(|| "script onConnect drop".to_string());
                return Ok(ConnectAction::Drop { reason });
            }
        }
        Ok(ConnectAction::Allow)
    }

    fn handle_on_disconnect(
        runtime: &mut JsRuntime,
        conn: ConnectionInfo,
        stats: ConnectionStats,
    ) -> Result<(), String> {
        let check_code = "typeof globalThis.onDisconnect === 'function'";
        let exists = runtime
            .execute_script("check_onDisconnect", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                deno_core::v8::Local::new(&mut scope, v).is_true()
            })
            .unwrap_or(false);
        if !exists {
            return Ok(());
        }

        let conn_json = serde_json::json!({
            "id": conn.id.to_string(),
            "client_addr": conn.client_addr.to_string(),
            "server_addr": conn.server_addr.map(|a| a.to_string()),
            "tls_sni": conn.tls_sni,
        });
        let stats_json = serde_json::json!({
            "duration_ms": stats.duration_ms,
            "bytes_sent": stats.bytes_sent,
            "bytes_received": stats.bytes_received,
            "flows_count": stats.flows_count,
        });
        let code = format!(
            "globalThis.onDisconnect({{}}, {}, {})",
            serde_json::to_string(&conn_json).unwrap_or_default(),
            serde_json::to_string(&stats_json).unwrap_or_default()
        );
        let _ = runtime.execute_script("call_onDisconnect", code);
        Ok(())
    }

    fn handle_on_websocket_start(
        runtime: &mut JsRuntime,
        flow: Flow,
    ) -> Result<Option<Flow>, String> {
        let check_code = "typeof globalThis.onWebSocketStart === 'function'";
        let exists = runtime
            .execute_script("check_onWebSocketStart", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                deno_core::v8::Local::new(&mut scope, v).is_true()
            })
            .unwrap_or(false);
        if !exists {
            return Ok(None);
        }

        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let code = format!("globalThis.onWebSocketStart({{}}, {})", flow_json);
        let result = runtime
            .execute_script("call_onWebSocketStart", code)
            .map_err(|e| e.to_string())?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Flow =
            deno_core::serde_v8::from_v8(&mut scope, result_val).map_err(|e| e.to_string())?;
        Ok(Some(deser))
    }

    fn handle_on_websocket_end(
        runtime: &mut JsRuntime,
        flow: Flow,
        close_code: u16,
        close_reason: String,
    ) -> Result<Option<Flow>, String> {
        let check_code = "typeof globalThis.onWebSocketEnd === 'function'";
        let exists = runtime
            .execute_script("check_onWebSocketEnd", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                deno_core::v8::Local::new(&mut scope, v).is_true()
            })
            .unwrap_or(false);
        if !exists {
            return Ok(None);
        }

        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let reason_json =
            serde_json::to_string(&close_reason).unwrap_or_else(|_| "null".to_string());
        let code = format!(
            "globalThis.onWebSocketEnd({{}}, {}, {}, {})",
            flow_json, close_code, reason_json
        );
        let result = runtime
            .execute_script("call_onWebSocketEnd", code)
            .map_err(|e| e.to_string())?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Flow =
            deno_core::serde_v8::from_v8(&mut scope, result_val).map_err(|e| e.to_string())?;
        Ok(Some(deser))
    }

    fn handle_on_websocket_error(
        runtime: &mut JsRuntime,
        flow: Flow,
        error: String,
    ) -> Result<Option<Flow>, String> {
        let check_code = "typeof globalThis.onWebSocketError === 'function'";
        let exists = runtime
            .execute_script("check_onWebSocketError", check_code)
            .ok()
            .map(|v| {
                let mut scope = runtime.handle_scope();
                deno_core::v8::Local::new(&mut scope, v).is_true()
            })
            .unwrap_or(false);
        if !exists {
            return Ok(None);
        }

        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;
        let error_json = serde_json::to_string(&error).unwrap_or_else(|_| "null".to_string());
        let code = format!(
            "globalThis.onWebSocketError({{}}, {}, {})",
            flow_json, error_json
        );
        let result = runtime
            .execute_script("call_onWebSocketError", code)
            .map_err(|e| e.to_string())?;
        let mut scope = runtime.handle_scope();
        let result_val = deno_core::v8::Local::new(&mut scope, result);
        if result_val.is_undefined() || result_val.is_null() {
            return Ok(None);
        }
        let deser: Flow =
            deno_core::serde_v8::from_v8(&mut scope, result_val).map_err(|e| e.to_string())?;
        Ok(Some(deser))
    }

    async fn hook_defined(&self, kind: BodyHookKind) -> Result<bool, BoxError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DenoCommand::HasHook(kind, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        rx.await.map_err(|e| Box::new(e) as BoxError)
    }

    /// Run a body hook without moving the live stream onto the isolate thread.
    ///
    /// The script sees a bounded in-memory copy. The returned body is what the proxy
    /// should forward: the script's replacement, or the original bytes (including any
    /// tail past the budget) if the hook passes through or fails.
    async fn dispatch_body_hook(
        &self,
        kind: BodyHookKind,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<HttpBody, BoxError> {
        if !self.hook_defined(kind).await? {
            return Ok(body);
        }

        let prepared = streams::prepare_script_body(body, SCRIPT_BODY_BUDGET).await?;
        let (tx, rx) = oneshot::channel();
        let command = match kind {
            BodyHookKind::Request => {
                DenoCommand::OnRequest(flow.clone(), prepared.visible, prepared.truncated, tx)
            }
            BodyHookKind::Response => {
                DenoCommand::OnResponse(flow.clone(), prepared.visible, prepared.truncated, tx)
            }
        };
        self.tx
            .send(command)
            .await
            .map_err(|e| Box::new(e) as BoxError)?;

        let truncated = prepared.truncated;
        let reply = rx.await.map_err(|e| Box::new(e) as BoxError)?;
        let forward = match reply {
            Ok((new_flow, replacement)) => {
                if let Some(new_flow) = new_flow {
                    *flow = new_flow;
                }
                match replacement {
                    Some(bytes) => streams::full_body(bytes),
                    None => prepared.forward,
                }
            }
            Err(error) => {
                tracing::error!("Script execution error ({kind:?}): {error}");
                flow.tags.push("script-error".to_string());
                if let Layer::Http(http) = &mut flow.layer {
                    http.error = Some(format!("Script Error: {error}"));
                }
                prepared.forward
            }
        };
        note_truncated(flow, truncated);
        Ok(forward)
    }
}

#[async_trait]
impl ScriptEngineTrait for DenoScriptEngine {
    async fn load_script(&mut self, script: &str) -> Result<(), BoxError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DenoCommand::LoadScript(script.to_string(), tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        rx.await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)
    }

    async fn on_request_headers(&self, flow: &mut Flow) -> Result<Option<Flow>, BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        self.tx
            .send(DenoCommand::OnRequestHeaders(flow_clone, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(new_flow) = &res {
            *flow = new_flow.clone();
        }
        Ok(res)
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let body = self
            .dispatch_body_hook(BodyHookKind::Request, flow, body)
            .await?;
        Ok(RequestAction::Continue(body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> Result<Option<Flow>, BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        self.tx
            .send(DenoCommand::OnResponseHeaders(flow_clone, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(new_flow) = &res {
            *flow = new_flow.clone();
        }
        Ok(res)
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let body = self
            .dispatch_body_hook(BodyHookKind::Response, flow, body)
            .await?;
        Ok(ResponseAction::Continue(body))
    }

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        message: &mut WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = _flow.clone();
        let message_clone = message.clone();
        self.tx
            .send(DenoCommand::OnWebSocketMessage(
                flow_clone,
                message_clone,
                tx,
            ))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        Ok(res)
    }

    async fn on_connect(&self, conn: &ConnectionInfo) -> Result<ConnectAction, BoxError> {
        let (tx, rx) = oneshot::channel();
        let conn_clone = conn.clone();
        self.tx
            .send(DenoCommand::OnConnect(conn_clone, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        rx.await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)
    }

    async fn on_disconnect(
        &self,
        conn: &ConnectionInfo,
        stats: &ConnectionStats,
    ) -> Result<(), BoxError> {
        let (tx, rx) = oneshot::channel();
        let conn_clone = conn.clone();
        let stats_clone = stats.clone();
        self.tx
            .send(DenoCommand::OnDisconnect(conn_clone, stats_clone, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        rx.await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)
    }

    async fn on_websocket_start(&self, flow: &mut Flow) -> Result<(), BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        self.tx
            .send(DenoCommand::OnWebSocketStart(flow_clone, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(new_flow) = res {
            *flow = new_flow;
        }
        Ok(())
    }

    async fn on_websocket_end(
        &self,
        flow: &mut Flow,
        close_code: u16,
        close_reason: &str,
    ) -> Result<(), BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        let reason_owned = close_reason.to_string();
        self.tx
            .send(DenoCommand::OnWebSocketEnd(
                flow_clone,
                close_code,
                reason_owned,
                tx,
            ))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(new_flow) = res {
            *flow = new_flow;
        }
        Ok(())
    }

    async fn on_websocket_error(&self, flow: &mut Flow, error: &str) -> Result<(), BoxError> {
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        let error_owned = error.to_string();
        self.tx
            .send(DenoCommand::OnWebSocketError(flow_clone, error_owned, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let res = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(new_flow) = res {
            *flow = new_flow;
        }
        Ok(())
    }
}

#[cfg(test)]
mod hook_timeout_tests {
    use super::{DenoScriptEngine, ScriptFetchConfig};
    use crate::engine_trait::ScriptEngineTrait;
    use bytes::Bytes;
    use chrono::Utc;
    use http_body_util::{BodyExt, Full};
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol,
    };
    use relay_core_lib::interceptor::{BoxError, ResponseAction};
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};
    use url::Url;
    use uuid::Uuid;

    fn flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 1,
                server_ip: "127.0.0.1".to_string(),
                server_port: 80,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("http://example.com/").unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    body: None,
                    cookies: vec![],
                    query: vec![],
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
            rule_variables: HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[tokio::test]
    async fn a_hook_that_outlives_the_deadline_releases_the_queue() {
        let mut engine = DenoScriptEngine::with_hook_timeout(
            HashSet::new(),
            ScriptFetchConfig::default(),
            Duration::from_millis(200),
        );
        engine
            .load_script(
                r#"
                globalThis.onResponse = async () => {
                    await Deno.core.ops.op_wait_ms(10000);
                };
                "#,
            )
            .await
            .unwrap();

        let mut flow = flow();
        let body = Full::new(Bytes::from_static(b"kept"))
            .map_err(|e| -> BoxError { e.into() })
            .boxed();
        let started = Instant::now();
        let action = engine.on_response(&mut flow, body).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "hook timeout took {:?}",
            started.elapsed()
        );
        assert!(
            flow.tags.iter().any(|tag| tag == "script-error"),
            "timed-out hook should be tagged, got {:?}",
            flow.tags
        );
        match action {
            ResponseAction::Continue(body) => {
                let bytes = body.collect().await.unwrap().to_bytes();
                assert_eq!(&bytes[..], b"kept");
            }
            other => panic!("expected Continue, got {other:?}"),
        }

        engine
            .load_script("globalThis.onResponse = (body, flow) => flow;")
            .await
            .expect("LoadScript must work after a timed-out hook");
    }
}
