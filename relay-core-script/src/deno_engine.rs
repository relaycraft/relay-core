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
    BoxError, HttpBody, RequestAction, ResponseAction, WebSocketMessageAction,
};
use std::cell::RefCell;
use std::collections::HashMap;
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
    shared_state(state).insert(key, value);
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

enum DenoCommand {
    LoadScript(String, oneshot::Sender<Result<(), String>>),
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
}

#[derive(Clone)]
pub struct DenoScriptEngine {
    tx: mpsc::Sender<DenoCommand>,
}

impl Default for DenoScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DenoScriptEngine {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel(32);

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
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
                    ]),
                    op_state_fn: Some(Box::new(|state| {
                        state.put(HashMap::<String, serde_json::Value>::new());
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
                    };

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
}
