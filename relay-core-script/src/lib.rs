//! Internal Deno/V8 scripting engine for [relay-core](https://crates.io/crates/relay-core).
//! Provides the `ScriptInterceptor` that implements runtime script modification of traffic.
//!
//! **This is a feature backend for `relay-core`.** Enable with `relay-core = { features = ["script"] }`.

pub mod deno_engine;
pub mod engine_trait;
pub mod streams;

use crate::deno_engine::DenoScriptEngine;
use crate::engine_trait::ScriptEngineTrait;
use async_trait::async_trait;
use relay_core_api::flow::{Flow, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, HttpBody, InterceptionResult, Interceptor, RequestAction, ResponseAction,
    WebSocketMessageAction,
};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::RwLock;

/// S4: Per-hook script execution metrics exposed via Prometheus/text.
pub struct ScriptMetrics {
    pub on_request_headers_duration_us: AtomicU64,
    pub on_request_headers_invocations: AtomicU64,
    pub on_request_headers_errors: AtomicU64,

    pub on_request_duration_us: AtomicU64,
    pub on_request_invocations: AtomicU64,
    pub on_request_errors: AtomicU64,

    pub on_response_headers_duration_us: AtomicU64,
    pub on_response_headers_invocations: AtomicU64,
    pub on_response_headers_errors: AtomicU64,

    pub on_response_duration_us: AtomicU64,
    pub on_response_invocations: AtomicU64,
    pub on_response_errors: AtomicU64,

    pub on_websocket_message_duration_us: AtomicU64,
    pub on_websocket_message_invocations: AtomicU64,
    pub on_websocket_message_errors: AtomicU64,
}

impl Default for ScriptMetrics {
    fn default() -> Self {
        Self {
            on_request_headers_duration_us: AtomicU64::new(0),
            on_request_headers_invocations: AtomicU64::new(0),
            on_request_headers_errors: AtomicU64::new(0),
            on_request_duration_us: AtomicU64::new(0),
            on_request_invocations: AtomicU64::new(0),
            on_request_errors: AtomicU64::new(0),
            on_response_headers_duration_us: AtomicU64::new(0),
            on_response_headers_invocations: AtomicU64::new(0),
            on_response_headers_errors: AtomicU64::new(0),
            on_response_duration_us: AtomicU64::new(0),
            on_response_invocations: AtomicU64::new(0),
            on_response_errors: AtomicU64::new(0),
            on_websocket_message_duration_us: AtomicU64::new(0),
            on_websocket_message_invocations: AtomicU64::new(0),
            on_websocket_message_errors: AtomicU64::new(0),
        }
    }
}

impl ScriptMetrics {
    pub fn prometheus_lines(&self) -> String {
        let mut out = String::new();
        macro_rules! push_metric {
            ($name:expr, $dur:expr, $inv:expr, $err:expr) => {
                out.push_str(&format!(
                    "relay_core_script_hook_duration_us{{hook=\"{}\"}} {}\n",
                    $name,
                    $dur.load(Ordering::Relaxed)
                ));
                out.push_str(&format!(
                    "relay_core_script_hook_invocations_total{{hook=\"{}\"}} {}\n",
                    $name,
                    $inv.load(Ordering::Relaxed)
                ));
                out.push_str(&format!(
                    "relay_core_script_hook_errors_total{{hook=\"{}\"}} {}\n",
                    $name,
                    $err.load(Ordering::Relaxed)
                ));
            };
        }
        push_metric!(
            "onRequestHeaders",
            &self.on_request_headers_duration_us,
            &self.on_request_headers_invocations,
            &self.on_request_headers_errors
        );
        push_metric!(
            "onRequest",
            &self.on_request_duration_us,
            &self.on_request_invocations,
            &self.on_request_errors
        );
        push_metric!(
            "onResponseHeaders",
            &self.on_response_headers_duration_us,
            &self.on_response_headers_invocations,
            &self.on_response_headers_errors
        );
        push_metric!(
            "onResponse",
            &self.on_response_duration_us,
            &self.on_response_invocations,
            &self.on_response_errors
        );
        push_metric!(
            "onWebSocketMessage",
            &self.on_websocket_message_duration_us,
            &self.on_websocket_message_invocations,
            &self.on_websocket_message_errors
        );
        out
    }
}

pub struct ScriptInterceptor {
    engines: Vec<RwLock<Box<dyn ScriptEngineTrait>>>,
    pub metrics: ScriptMetrics,
    env_allow: RwLock<HashSet<String>>,
}

impl ScriptInterceptor {
    pub async fn new() -> Result<Self, BoxError> {
        Self::new_with_env(HashSet::new()).await
    }

    pub async fn new_with_env(env_allow: HashSet<String>) -> Result<Self, BoxError> {
        let pool_size = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut engines = Vec::with_capacity(pool_size);

        for _ in 0..pool_size {
            let engine: Box<dyn ScriptEngineTrait> =
                Box::new(DenoScriptEngine::new(env_allow.clone()));
            engines.push(RwLock::new(engine));
        }

        Ok(Self {
            engines,
            metrics: ScriptMetrics::default(),
            env_allow: RwLock::new(env_allow),
        })
    }

    pub async fn set_env_allow(&self, env_allow: HashSet<String>) {
        let mut guard = self.env_allow.write().await;
        *guard = env_allow;
    }

    pub async fn load_script(&self, script: &str) -> Result<(), BoxError> {
        // Load script into ALL engines to keep them consistent.
        let pool_size = self.engines.len();
        let mut new_engines = Vec::with_capacity(pool_size);

        let env = self.env_allow.read().await.clone();
        for _ in 0..pool_size {
            let mut engine = DenoScriptEngine::new(env.clone());
            engine.load_script(script).await?;
            new_engines.push(Box::new(engine) as Box<dyn ScriptEngineTrait>);
        }

        // We acquire write locks only for the swap.
        // The new engines are already prepared, so the critical section is very short.
        let mut new_engines_iter = new_engines.into_iter();
        for engine_lock in &self.engines {
            if let Some(new_engine) = new_engines_iter.next() {
                let mut guard = engine_lock.write().await;
                *guard = new_engine;
            }
        }

        Ok(())
    }

    fn get_engine_index(&self) -> usize {
        // Optimization: Allow task-local override for engine index (e.g. for testing or specific routing)
        if let Ok(index) = relay_core_lib::interceptor::ENGINE_INDEX.try_with(|i| *i) {
            return index % self.engines.len();
        }

        let thread_id = std::thread::current().id();
        let mut hasher = DefaultHasher::new();
        thread_id.hash(&mut hasher);
        (hasher.finish() as usize) % self.engines.len()
    }
}

#[async_trait]
impl Interceptor for ScriptInterceptor {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let start = Instant::now();
        let index = self.get_engine_index();
        let engine_lock = &self.engines[index];
        let engine = engine_lock.read().await;

        let result = match engine.on_request_headers(flow).await {
            Ok(Some(modified_flow)) => {
                *flow = modified_flow;
                InterceptionResult::ModifiedRequest(match &flow.layer {
                    relay_core_api::flow::Layer::Http(h) => h.request.clone(),
                    relay_core_api::flow::Layer::WebSocket(w) => w.handshake_request.clone(),
                    _ => {
                        self.metrics
                            .on_request_headers_errors
                            .fetch_add(1, Ordering::Relaxed);
                        return InterceptionResult::Continue;
                    }
                })
            }
            Ok(None) => InterceptionResult::Continue,
            Err(e) => {
                tracing::error!("Script execution error (on_request_headers): {}", e);
                flow.tags.push("script-error".to_string());
                if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
                    http.error = Some(format!("Script Error: {}", e));
                }
                self.metrics
                    .on_request_headers_errors
                    .fetch_add(1, Ordering::Relaxed);
                InterceptionResult::Continue
            }
        };
        let dur_us = start.elapsed().as_micros() as u64;
        self.metrics
            .on_request_headers_duration_us
            .fetch_add(dur_us, Ordering::Relaxed);
        self.metrics
            .on_request_headers_invocations
            .fetch_add(1, Ordering::Relaxed);
        result
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let start = Instant::now();
        let index = self.get_engine_index();
        let engine_lock = &self.engines[index];
        let engine = engine_lock.read().await;

        match engine.on_request(flow, body).await {
            Ok(action) => {
                let dur_us = start.elapsed().as_micros() as u64;
                self.metrics
                    .on_request_duration_us
                    .fetch_add(dur_us, Ordering::Relaxed);
                self.metrics
                    .on_request_invocations
                    .fetch_add(1, Ordering::Relaxed);
                Ok(action)
            }
            Err(e) => {
                self.metrics
                    .on_request_errors
                    .fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let start = Instant::now();
        let index = self.get_engine_index();
        let engine_lock = &self.engines[index];
        let engine = engine_lock.read().await;

        let result = match engine.on_response_headers(flow).await {
            Ok(Some(modified_flow)) => {
                *flow = modified_flow;
                InterceptionResult::ModifiedResponse(match &flow.layer {
                    relay_core_api::flow::Layer::Http(h) => h.response.clone().unwrap(),
                    relay_core_api::flow::Layer::WebSocket(w) => w.handshake_response.clone(),
                    _ => {
                        self.metrics
                            .on_response_headers_errors
                            .fetch_add(1, Ordering::Relaxed);
                        return InterceptionResult::Continue;
                    }
                })
            }
            Ok(None) => InterceptionResult::Continue,
            Err(e) => {
                tracing::error!("Script execution error (on_response_headers): {}", e);
                flow.tags.push("script-error".to_string());
                if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
                    http.error = Some(format!("Script Error: {}", e));
                }
                self.metrics
                    .on_response_headers_errors
                    .fetch_add(1, Ordering::Relaxed);
                InterceptionResult::Continue
            }
        };
        let dur_us = start.elapsed().as_micros() as u64;
        self.metrics
            .on_response_headers_duration_us
            .fetch_add(dur_us, Ordering::Relaxed);
        self.metrics
            .on_response_headers_invocations
            .fetch_add(1, Ordering::Relaxed);
        result
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError> {
        let start = Instant::now();
        let index = self.get_engine_index();
        let engine_lock = &self.engines[index];
        let engine = engine_lock.read().await;

        match engine.on_response(flow, body).await {
            Ok(action) => {
                let dur_us = start.elapsed().as_micros() as u64;
                self.metrics
                    .on_response_duration_us
                    .fetch_add(dur_us, Ordering::Relaxed);
                self.metrics
                    .on_response_invocations
                    .fetch_add(1, Ordering::Relaxed);
                Ok(action)
            }
            Err(e) => {
                self.metrics
                    .on_response_errors
                    .fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    async fn on_websocket_message(
        &self,
        flow: &mut Flow,
        mut message: WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        let start = Instant::now();
        let index = self.get_engine_index();
        let engine_lock = &self.engines[index];
        let engine = engine_lock.read().await;

        match engine.on_websocket_message(flow, &mut message).await {
            Ok(action) => {
                let dur_us = start.elapsed().as_micros() as u64;
                self.metrics
                    .on_websocket_message_duration_us
                    .fetch_add(dur_us, Ordering::Relaxed);
                self.metrics
                    .on_websocket_message_invocations
                    .fetch_add(1, Ordering::Relaxed);
                Ok(action)
            }
            Err(e) => {
                tracing::error!("Script execution error (on_websocket_message): {}", e);
                flow.tags.push("script-error".to_string());
                self.metrics
                    .on_websocket_message_errors
                    .fetch_add(1, Ordering::Relaxed);
                Ok(WebSocketMessageAction::Continue(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use chrono::Utc;
    use http_body_util::{BodyExt, Empty};
    use relay_core_api::flow::{
        BodyData, Direction, Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol,
        WebSocketMessage,
    };
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn create_test_flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 12345,
                server_ip: "1.1.1.1".to_string(),
                server_port: 80,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "GET".to_string(),
                    url: Url::parse("http://example.com").unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
            resilience_trace: None,
        }
    }

    #[tokio::test]
    async fn test_script_error_propagation() {
        let interceptor = ScriptInterceptor::new().await.unwrap();

        let script = r#"
            globalThis.onRequestHeaders = (flow) => {
                throw new Error("Test Error 123");
            };
        "#;
        interceptor.load_script(script).await.unwrap();

        let mut flow = create_test_flow();

        let result = interceptor.on_request_headers(&mut flow).await;

        match result {
            InterceptionResult::Continue => {}
            _ => panic!("Expected Continue"),
        }

        assert!(flow.tags.contains(&"script-error".to_string()));

        if let Layer::Http(http) = &flow.layer {
            assert!(http.error.is_some());
            let err = http.error.as_ref().unwrap();
            assert!(err.contains("Test Error 123"));
        } else {
            panic!("Expected Http layer");
        }
    }

    #[tokio::test]
    async fn test_script_api_relay_body() {
        let interceptor = ScriptInterceptor::new().await.unwrap();

        let script = r#"
            globalThis.onRequest = (body, flow) => {
                if (!(body instanceof RelayBody)) {
                    throw new Error("First argument is not RelayBody");
                }
                // We return nothing (undefined), which means "continue with original body"
                // But we successfully verified the type.
            };
        "#;
        interceptor.load_script(script).await.unwrap();

        let mut flow = create_test_flow();
        let body = Empty::<Bytes>::new()
            .map_err(|_| -> BoxError { unreachable!() })
            .boxed();

        let result = interceptor.on_request(&mut flow, body).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_load_script_failure_does_not_replace_existing_engines() {
        let interceptor = ScriptInterceptor::new().await.unwrap();

        let good_script = r#"
            globalThis.onRequestHeaders = (_context, flow) => {
                if (flow.layer.type === "Http") {
                    flow.layer.data.request.headers.push(["X-Good-Script", "1"]);
                }
                return flow;
            };
        "#;
        interceptor
            .load_script(good_script)
            .await
            .expect("good script should load");

        let bad_script = "globalThis.onRequestHeaders = () => { invalid javascript !!!";
        let bad = interceptor.load_script(bad_script).await;
        assert!(bad.is_err(), "bad script should be rejected");

        let mut flow = create_test_flow();
        let result = interceptor.on_request_headers(&mut flow).await;
        assert!(
            matches!(result, InterceptionResult::ModifiedRequest(_)),
            "existing good script should still be active after failed reload"
        );
        if let Layer::Http(http) = &flow.layer {
            assert!(
                http.request
                    .headers
                    .iter()
                    .any(|(k, v)| k == "X-Good-Script" && v == "1")
            );
        } else {
            panic!("Expected Http layer");
        }
    }

    #[tokio::test]
    async fn test_websocket_script_error_falls_back_continue_and_tags_flow() {
        let interceptor = ScriptInterceptor::new().await.unwrap();
        let script = r#"
            globalThis.onWebSocketMessage = function(_context, _flow, _message) {
                throw new Error("ws explode");
            };
        "#;
        interceptor
            .load_script(script)
            .await
            .expect("script should load");

        let mut flow = create_test_flow();
        let msg = WebSocketMessage {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            direction: Direction::ClientToServer,
            content: BodyData {
                encoding: "utf-8".to_string(),
                content: "hello".to_string(),
                size: 5,
            },
            opcode: "Text".to_string(),
        };

        let result = interceptor
            .on_websocket_message(&mut flow, msg.clone())
            .await
            .expect("websocket interception should not return hard error");
        match result {
            WebSocketMessageAction::Continue(forwarded) => {
                assert_eq!(forwarded.content.content, "hello");
            }
            other => panic!("expected Continue fallback, got {:?}", other),
        }
        assert!(
            flow.tags.iter().any(|t| t == "script-error"),
            "script error should be tagged for observability"
        );
    }
}
