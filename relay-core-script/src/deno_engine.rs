use crate::engine_trait::ScriptEngineTrait;
use crate::streams::HttpBodyResource;
use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use deno_core::{
    Extension, JsRuntime, Op, OpState, ResourceId, RuntimeOptions, error::AnyError, op2,
};
use http_body_util::{BodyExt, Full};
use relay_core_api::flow::{Flow, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, RequestAction,
    ResponseAction, WebSocketMessageAction,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::thread;
use tokio::sync::{mpsc, oneshot};

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

#[op2(async)]
#[serde]
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

    // S7b: relay.fetch HTTP client deferred to 1.x — current V8 isolate uses
    // new_current_thread runtime which is incompatible with reqwest blocking client.
    serde_json::json!({"ok": false, "error": "fetch not yet implemented"}).to_string()
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

enum DenoCommand {
    LoadScript(String, oneshot::Sender<Result<(), String>>),
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
    OnRequest(
        Flow,
        HttpBody,
        oneshot::Sender<Result<(Option<Flow>, RequestAction), String>>,
    ),
    OnResponseHeaders(Flow, oneshot::Sender<Result<Option<Flow>, String>>),
    OnResponse(
        Flow,
        HttpBody,
        oneshot::Sender<Result<(Option<Flow>, ResponseAction), String>>,
    ),
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

impl DenoScriptEngine {
    pub fn new(env_allow: HashSet<String>) -> Self {
        Self::new_with_fetch(env_allow, ScriptFetchConfig::default())
    }

    pub fn new_with_fetch(env_allow: HashSet<String>, fetch_config: ScriptFetchConfig) -> Self {
        let (tx, mut rx) = mpsc::channel(32);

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                let env_allow = env_allow; // capture into async block
                let ext = Extension {
                    name: "relay_core",
                    ops: std::borrow::Cow::Borrowed(&[
                        op_log_level::DECL,
                        op_read_body::DECL,
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
                        constructor(rid) { this.rid = rid; }
                        async read(limit) {
                            return await Deno.core.ops.op_read_body(this.rid, limit || 65536);
                        }
                        close() {
                            Deno.core.ops.op_close_body(this.rid);
                        }
                        async text() {
                            const bytes = await this.read(10 * 1024 * 1024);
                            return new TextDecoder().decode(bytes);
                        }
                        async json() {
                            const txt = await this.text();
                            return JSON.parse(txt);
                        }
                    }
                    globalThis.RelayBody = RelayBody;

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

                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        DenoCommand::LoadScript(script, resp) => {
                            let res = js_runtime.execute_script("<anon>", script);
                            let res = if let Err(e) = res {
                                Err(e.to_string())
                            } else {
                                js_runtime
                                    .run_event_loop(Default::default())
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            };
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnRequestHeaders(flow, resp) => {
                            let res = Self::handle_on_request_headers(&mut js_runtime, flow);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnRequest(flow, body, resp) => {
                            let res = Self::handle_on_request(&mut js_runtime, flow, body).await;
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnResponseHeaders(flow, resp) => {
                            let res = Self::handle_on_response_headers(&mut js_runtime, flow);
                            let _ = resp.send(res);
                        }
                        DenoCommand::OnResponse(flow, body, resp) => {
                            let res = Self::handle_on_response(&mut js_runtime, flow, body).await;
                            let _ = resp.send(res);
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

    async fn handle_on_request(
        runtime: &mut JsRuntime,
        flow: Flow,
        body: HttpBody,
    ) -> Result<(Option<Flow>, RequestAction), String> {
        let resource = HttpBodyResource::new(body);
        let rid = {
            let op_state_rc = runtime.op_state();
            let mut state = op_state_rc.borrow_mut();
            state.resource_table.add(resource)
        };

        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;

        let check_code = "typeof globalThis.onRequest === 'function'";
        let exists = runtime
            .execute_script("check_onRequest", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onRequest");
                e.to_string()
            })?;

        let exists_bool = {
            let scope = &mut runtime.handle_scope();
            let exists_val = deno_core::v8::Local::new(scope, exists);
            exists_val.is_true()
        };

        if !exists_bool {
            let resource = {
                let op_state_rc = runtime.op_state();
                let mut state = op_state_rc.borrow_mut();
                state.resource_table.take::<HttpBodyResource>(rid).ok()
            };
            if let Some(res) = resource {
                let body = crate::streams::create_body_from_resource(&res);
                return Ok((None, RequestAction::Continue(body)));
            } else {
                return Ok((
                    None,
                    RequestAction::Continue(
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed(),
                    ),
                ));
            }
        }

        let code = format!(
            "globalThis.onRequest(new RelayBody({}), {})",
            rid, flow_json
        );
        let result = runtime
            .execute_script("call_onRequest", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onRequest");
                e.to_string()
            })?;

        let result = runtime.resolve(result).await.map_err(|e| {
            Self::try_call_on_error(runtime, &flow, &e.to_string(), "onRequest");
            e.to_string()
        })?;

        let (is_empty, modified_flow) = {
            let mut scope = runtime.handle_scope();
            let result_val = deno_core::v8::Local::new(&mut scope, result);

            if result_val.is_undefined() || result_val.is_null() {
                (true, None)
            } else {
                let deser: Result<Flow, _> = deno_core::serde_v8::from_v8(&mut scope, result_val);
                drop(scope);
                match deser {
                    Ok(f) => (false, Some(f)),
                    Err(e) => {
                        let err_str = format!("Failed to deserialize flow: {}", e);
                        Self::try_call_on_error(runtime, &flow, &err_str, "onRequest");
                        return Err(err_str);
                    }
                }
            }
        };

        if is_empty {
            let resource = {
                let op_state_rc = runtime.op_state();
                let mut state = op_state_rc.borrow_mut();
                state.resource_table.take::<HttpBodyResource>(rid).ok()
            };
            if let Some(res) = resource {
                let body = crate::streams::create_body_from_resource(&res);
                return Ok((None, RequestAction::Continue(body)));
            } else {
                return Ok((
                    None,
                    RequestAction::Continue(
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed(),
                    ),
                ));
            }
        }

        let modified_flow = modified_flow.unwrap();

        let resource = {
            let op_state_rc = runtime.op_state();
            let mut state = op_state_rc.borrow_mut();
            state.resource_table.take::<HttpBodyResource>(rid).ok()
        };

        let new_body: HttpBody = if let Some(res) = resource {
            let has_new_body = if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer
            {
                http.request
                    .body
                    .as_ref()
                    .map(|b| !b.content.is_empty())
                    .unwrap_or(false)
            } else {
                false
            };

            if has_new_body {
                if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer {
                    if let Some(b) = &http.request.body {
                        let bytes: Bytes = if b.encoding == "base64" {
                            base64::engine::general_purpose::STANDARD
                                .decode(&b.content)
                                .unwrap_or_default()
                                .into()
                        } else {
                            Bytes::from(b.content.clone())
                        };
                        Full::new(bytes)
                            .map_err(|e| -> BoxError { e.into() })
                            .boxed()
                    } else {
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed()
                    }
                } else {
                    http_body_util::Empty::new()
                        .map_err(|_| -> BoxError { unreachable!() })
                        .boxed()
                }
            } else {
                crate::streams::create_body_from_resource(&res)
            }
        } else if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer {
            if let Some(b) = &http.request.body {
                let bytes: Bytes = if b.encoding == "base64" {
                    base64::engine::general_purpose::STANDARD
                        .decode(&b.content)
                        .unwrap_or_default()
                        .into()
                } else {
                    Bytes::from(b.content.clone())
                };
                Full::new(bytes)
                    .map_err(|e| -> BoxError { e.into() })
                    .boxed()
            } else {
                http_body_util::Empty::new()
                    .map_err(|_| -> BoxError { unreachable!() })
                    .boxed()
            }
        } else {
            http_body_util::Empty::new()
                .map_err(|_| -> BoxError { unreachable!() })
                .boxed()
        };

        Ok((Some(modified_flow), RequestAction::Continue(new_body)))
    }

    async fn handle_on_response(
        runtime: &mut JsRuntime,
        flow: Flow,
        body: HttpBody,
    ) -> Result<(Option<Flow>, ResponseAction), String> {
        let resource = HttpBodyResource::new(body);
        let rid = {
            let op_state_rc = runtime.op_state();
            let mut state = op_state_rc.borrow_mut();
            state.resource_table.add(resource)
        };

        let flow_json = serde_json::to_string(&flow).map_err(|e| e.to_string())?;

        let check_code = "typeof globalThis.onResponse === 'function'";
        let exists = runtime
            .execute_script("check_onResponse", check_code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onResponse");
                e.to_string()
            })?;

        let exists_bool = {
            let scope = &mut runtime.handle_scope();
            let exists_val = deno_core::v8::Local::new(scope, exists);
            exists_val.is_true()
        };

        if !exists_bool {
            let resource = {
                let op_state_rc = runtime.op_state();
                let mut state = op_state_rc.borrow_mut();
                state.resource_table.take::<HttpBodyResource>(rid).ok()
            };
            if let Some(res) = resource {
                let body = crate::streams::create_body_from_resource(&res);
                return Ok((None, ResponseAction::Continue(body)));
            } else {
                return Ok((
                    None,
                    ResponseAction::Continue(
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed(),
                    ),
                ));
            }
        }

        let code = format!(
            "globalThis.onResponse(new RelayBody({}), {})",
            rid, flow_json
        );
        let result = runtime
            .execute_script("call_onResponse", code)
            .map_err(|e| {
                Self::try_call_on_error(runtime, &flow, &e.to_string(), "onResponse");
                e.to_string()
            })?;
        let result = runtime.resolve(result).await.map_err(|e| {
            Self::try_call_on_error(runtime, &flow, &e.to_string(), "onResponse");
            e.to_string()
        })?;

        let (is_empty, modified_flow) = {
            let mut scope = runtime.handle_scope();
            let result_val = deno_core::v8::Local::new(&mut scope, result);

            if result_val.is_undefined() || result_val.is_null() {
                (true, None)
            } else {
                let deser: Result<Flow, _> = deno_core::serde_v8::from_v8(&mut scope, result_val);
                drop(scope);
                match deser {
                    Ok(f) => (false, Some(f)),
                    Err(e) => {
                        let err_str = format!("Failed to deserialize flow: {}", e);
                        Self::try_call_on_error(runtime, &flow, &err_str, "onResponse");
                        return Err(err_str);
                    }
                }
            }
        };

        if is_empty {
            let resource = {
                let op_state_rc = runtime.op_state();
                let mut state = op_state_rc.borrow_mut();
                state.resource_table.take::<HttpBodyResource>(rid).ok()
            };
            if let Some(res) = resource {
                let body = crate::streams::create_body_from_resource(&res);
                return Ok((None, ResponseAction::Continue(body)));
            } else {
                return Ok((
                    None,
                    ResponseAction::Continue(
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed(),
                    ),
                ));
            }
        }

        let modified_flow = modified_flow.unwrap();

        let resource = {
            let op_state_rc = runtime.op_state();
            let mut state = op_state_rc.borrow_mut();
            state.resource_table.take::<HttpBodyResource>(rid).ok()
        };

        let new_body: HttpBody = if let Some(res) = resource {
            let has_new_body = if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer
            {
                http.response
                    .as_ref()
                    .and_then(|r| r.body.as_ref())
                    .map(|b| !b.content.is_empty())
                    .unwrap_or(false)
            } else {
                false
            };

            if has_new_body {
                if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer {
                    if let Some(resp) = &http.response {
                        if let Some(b) = &resp.body {
                            let bytes: Bytes = if b.encoding == "base64" {
                                base64::engine::general_purpose::STANDARD
                                    .decode(&b.content)
                                    .unwrap_or_default()
                                    .into()
                            } else {
                                Bytes::from(b.content.clone())
                            };
                            Full::new(bytes)
                                .map_err(|e| -> BoxError { e.into() })
                                .boxed()
                        } else {
                            http_body_util::Empty::new()
                                .map_err(|_| -> BoxError { unreachable!() })
                                .boxed()
                        }
                    } else {
                        http_body_util::Empty::new()
                            .map_err(|_| -> BoxError { unreachable!() })
                            .boxed()
                    }
                } else {
                    http_body_util::Empty::new()
                        .map_err(|_| -> BoxError { unreachable!() })
                        .boxed()
                }
            } else {
                crate::streams::create_body_from_resource(&res)
            }
        } else if let relay_core_api::flow::Layer::Http(http) = &modified_flow.layer {
            if let Some(resp) = &http.response {
                if let Some(b) = &resp.body {
                    let bytes: Bytes = if b.encoding == "base64" {
                        base64::engine::general_purpose::STANDARD
                            .decode(&b.content)
                            .unwrap_or_default()
                            .into()
                    } else {
                        Bytes::from(b.content.clone())
                    };
                    Full::new(bytes)
                        .map_err(|e| -> BoxError { e.into() })
                        .boxed()
                } else {
                    http_body_util::Empty::new()
                        .map_err(|_| -> BoxError { unreachable!() })
                        .boxed()
                }
            } else {
                http_body_util::Empty::new()
                    .map_err(|_| -> BoxError { unreachable!() })
                    .boxed()
            }
        } else {
            http_body_util::Empty::new()
                .map_err(|_| -> BoxError { unreachable!() })
                .boxed()
        };

        Ok((Some(modified_flow), ResponseAction::Continue(new_body)))
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
                let err_str = format!("Failed to deserialize message: {}", e);
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
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        self.tx
            .send(DenoCommand::OnRequest(flow_clone, body, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let (new_flow, action) = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(f) = new_flow {
            *flow = f;
        }
        Ok(action)
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
        let (tx, rx) = oneshot::channel();
        let flow_clone = flow.clone();
        self.tx
            .send(DenoCommand::OnResponse(flow_clone, body, tx))
            .await
            .map_err(|e| Box::new(e) as BoxError)?;
        let (new_flow, action) = rx
            .await
            .map_err(|e| Box::new(e) as BoxError)?
            .map_err(|e| Box::new(std::io::Error::other(e)) as BoxError)?;

        if let Some(f) = new_flow {
            *flow = f;
        }
        Ok(action)
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
