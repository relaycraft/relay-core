use async_trait::async_trait;
use relay_core_lib::interceptor::{Interceptor, InterceptionResult, RequestAction, ResponseAction, WebSocketMessageAction, HttpBody, BoxError};
use relay_core_lib::proxy::http_utils::mock_to_response;
use relay_core_api::flow::{Flow, Layer, WebSocketMessage, BodyData, FlowUpdate};
use tauri::{AppHandle, Emitter, Runtime};
use std::sync::Arc;
use tokio::sync::{oneshot, mpsc};
use relay_core_runtime::CoreState;
use tokio::time::{timeout, Duration};
use relay_core_lib::rule::{RuleStage, RuleTraceSummary, TerminalReason};
use http_body_util::{BodyExt, Full};
use bytes::Bytes;
use base64::prelude::*;

pub use relay_core_runtime::rule::InterceptRule;

pub struct TauriInterceptor<R: Runtime> {
    pub app_handle: AppHandle<R>,
    pub state: Arc<CoreState>,
    pub flow_sender: mpsc::Sender<FlowUpdate>,
}

#[async_trait]
impl<R: Runtime> Interceptor for TauriInterceptor<R> {
    async fn on_request_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.state.get_rule_engine().await;
        
        let ctx = engine.execute(RuleStage::RequestHeaders, flow).await;
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return self.handle_termination(reason, flow, "request_headers", None).await;
        }
        
        if let RuleTraceSummary::Modified { .. } = &ctx.summary {
             if let Layer::Http(http) = &flow.layer {
                 return InterceptionResult::ModifiedRequest(http.request.clone());
             }
        }
        
        InterceptionResult::Continue
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        
        // 0. Check if we need to intercept body for rules
        if !engine.has_rules_for_stage(RuleStage::RequestBody) {
            // Optimization: If no rules for RequestBody, just continue.
            // The body is already wrapped in TapBody by the proxy, so streaming visualization works.
            return Ok(RequestAction::Continue(body));
        }

        // 1. Collect Body (fallback to buffering if rules exist)
        let body_bytes = body.collect().await?.to_bytes();
        
        // 2. Update Flow
        self.update_flow_request_body(flow, &body_bytes);
        
        // 3. Execute Rule
        let ctx = engine.execute(RuleStage::RequestBody, flow).await;
        
        // 4. Handle Termination
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
             let result = self.handle_termination(reason, flow, "request_body", None).await;
             match result {
                 InterceptionResult::Drop => return Ok(RequestAction::Drop),
                 InterceptionResult::MockResponse(res) => return Ok(RequestAction::MockResponse(mock_to_response(res))),
                 InterceptionResult::ModifiedResponse(res) => return Ok(RequestAction::MockResponse(mock_to_response(res))),
                 InterceptionResult::ModifiedRequest(req) => {
                     // User modified request in UI inspector
                     // Extract body
                     let new_body_bytes = if let Some(bd) = req.body {
                         self.decode_body(&bd)
                     } else {
                         Bytes::new()
                     };
                     let new_body: HttpBody = Full::new(new_body_bytes).map_err(|e| match e {}).boxed();
                     return Ok(RequestAction::Continue(new_body));
                 },
                 InterceptionResult::Continue => {
                     // Fall through to check if flow was modified by rule (not terminated)
                 },
                 _ => return Ok(RequestAction::Drop),
             }
        }
        
        // 5. Handle Modification (RuleEngine modification in place in flow)
        if let RuleTraceSummary::Modified { .. } = &ctx.summary {
            // Reconstruct body from flow
            if let Layer::Http(http) = &flow.layer {
                if let Some(body_data) = &http.request.body {
                    let new_bytes = self.decode_body(body_data);
                    let new_body: HttpBody = Full::new(new_bytes).map_err(|e| match e {}).boxed();
                    return Ok(RequestAction::Continue(new_body));
                }
            }
        }
        
        // Default: Continue with original body
        let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
        Ok(RequestAction::Continue(new_body))
    }

    async fn on_response_headers(&self, flow: &mut Flow) -> InterceptionResult {
        let engine = self.state.get_rule_engine().await;
        
        let ctx = engine.execute(RuleStage::ResponseHeaders, flow).await;
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
            return self.handle_termination(reason, flow, "response_headers", None).await;
        }
        
        if let RuleTraceSummary::Modified { .. } = &ctx.summary {
             if let Layer::Http(http) = &flow.layer {
                 if let Some(res) = &http.response {
                     return InterceptionResult::ModifiedResponse(res.clone());
                 }
             }
        }
        
        InterceptionResult::Continue
    }

    async fn on_response(&self, flow: &mut Flow, body: HttpBody) -> Result<ResponseAction, BoxError> {
        let engine = self.state.get_rule_engine().await;

        // 0. Check if we need to intercept body for rules
        if !engine.has_rules_for_stage(RuleStage::ResponseBody) {
            // Optimization: If no rules for ResponseBody, just continue.
            // The body is already wrapped in TapBody by the proxy, so streaming visualization works.
            return Ok(ResponseAction::Continue(body));
        }

        // 1. Collect Body
        let body_bytes = body.collect().await?.to_bytes();
        
        // 2. Update Flow
        self.update_flow_response_body(flow, &body_bytes);
        
        // 3. Execute Rule
        let ctx = engine.execute(RuleStage::ResponseBody, flow).await;
        
        // 4. Handle Termination
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
             let result = self.handle_termination(reason, flow, "response_body", None).await;
             match result {
                 InterceptionResult::Drop => return Ok(ResponseAction::Drop),
                 InterceptionResult::MockResponse(res) => return Ok(ResponseAction::ModifiedResponse(mock_to_response(res))),
                 InterceptionResult::ModifiedResponse(res) => return Ok(ResponseAction::ModifiedResponse(mock_to_response(res))),
                 InterceptionResult::Continue => {
                     // Fall through
                 },
                 _ => return Ok(ResponseAction::Drop),
             }
        }
        
        // 5. Handle Modification
        if let RuleTraceSummary::Modified { .. } = &ctx.summary {
            if let Layer::Http(http) = &flow.layer {
                if let Some(res) = &http.response {
                    if let Some(_body_data) = &res.body {
                        // We need to return ModifiedResponse because headers might also be modified
                        // But ResponseAction::ModifiedResponse takes Response<HttpBody>.
                        // So we construct full response from flow.
                        return Ok(ResponseAction::ModifiedResponse(mock_to_response(res.clone())));
                    }
                }
            }
        }
        
        // Default: Continue with original body
        let new_body: HttpBody = Full::new(body_bytes).map_err(|e| match e {}).boxed();
        Ok(ResponseAction::Continue(new_body))
    }

    async fn on_websocket_message(&self, flow: &mut Flow, message: WebSocketMessage) -> Result<WebSocketMessageAction, BoxError> {
        let engine = self.state.get_rule_engine().await;
        
        // 1. Temporarily add message to flow for RuleEngine matching
        if let Layer::WebSocket(ws) = &mut flow.layer {
            ws.messages.push(message.clone());
            
            // Cap history at 1000 messages to prevent memory leaks
            if ws.messages.len() > 1000 {
                let excess = ws.messages.len() - 1000;
                ws.messages.drain(0..excess);
            }
        }

        // 2. Execute RuleEngine
        let ctx = engine.execute(RuleStage::WebSocketMessage, flow).await;

        // 3. Extract the (potentially modified) message
        let mut modified_message = None;
        if let Layer::WebSocket(ws) = &mut flow.layer {
            modified_message = ws.messages.pop(); 
        }

        // 4. Check for termination
        if let RuleTraceSummary::Terminated { reason, .. } = &ctx.summary {
             match reason {
                 TerminalReason::Mock => {
                     if let Some(mod_msg) = modified_message {
                         return Ok(WebSocketMessageAction::Continue(mod_msg));
                     }
                 },
                 _ => {
                     let result = self.handle_termination(reason, flow, "ws_msg", Some(&message)).await;
                     match result {
                         InterceptionResult::Drop => return Ok(WebSocketMessageAction::Drop),
                         InterceptionResult::ModifiedMessage(mod_msg) => return Ok(WebSocketMessageAction::Continue(mod_msg)),
                         InterceptionResult::Continue => {
                             if let Some(mod_msg) = modified_message {
                                 return Ok(WebSocketMessageAction::Continue(mod_msg));
                             }
                         },
                         _ => return Ok(WebSocketMessageAction::Continue(message)),
                     }
                 }
             }
        }
        
        // 5. Default: Return modified message if exists, else original
        if let Some(mod_msg) = modified_message {
            Ok(WebSocketMessageAction::Continue(mod_msg))
        } else {
            Ok(WebSocketMessageAction::Continue(message))
        }
    }
}

impl<R: Runtime> TauriInterceptor<R> {
    fn update_flow_request_body(&self, flow: &mut Flow, bytes: &Bytes) {
        if let Layer::Http(http) = &mut flow.layer {
             let (content, encoding) = if let Ok(s) = String::from_utf8(bytes.to_vec()) {
                 (s, "utf-8".to_string())
             } else {
                 (BASE64_STANDARD.encode(bytes), "base64".to_string())
             };
             
             http.request.body = Some(BodyData {
                 encoding,
                 content,
                 size: bytes.len() as u64,
             });
        }
    }

    fn update_flow_response_body(&self, flow: &mut Flow, bytes: &Bytes) {
        if let Layer::Http(http) = &mut flow.layer {
             if let Some(res) = &mut http.response {
                 let (content, encoding) = if let Ok(s) = String::from_utf8(bytes.to_vec()) {
                     (s, "utf-8".to_string())
                 } else {
                     (BASE64_STANDARD.encode(bytes), "base64".to_string())
                 };
                 
                 res.body = Some(BodyData {
                     encoding,
                     content,
                     size: bytes.len() as u64,
                 });
             }
        }
    }

    fn decode_body(&self, body_data: &BodyData) -> Bytes {
        if body_data.encoding == "base64" {
            BASE64_STANDARD.decode(&body_data.content).unwrap_or_default().into()
        } else {
            Bytes::from(body_data.content.clone())
        }
    }

    async fn handle_termination(&self, reason: &TerminalReason, flow: &Flow, phase: &str, ws_message: Option<&WebSocketMessage>) -> InterceptionResult {
        match reason {
            TerminalReason::Drop | TerminalReason::Abort | TerminalReason::RateLimited => InterceptionResult::Drop,
            TerminalReason::Mock | TerminalReason::Redirect => {
                match &flow.layer {
                    Layer::Http(http) => {
                        if let Some(res) = &http.response {
                            InterceptionResult::MockResponse(res.clone())
                        } else {
                            // If mocking request, we construct a response
                            // Wait, if redirect/mock happens in request phase, we need a response to return.
                            // But `http.response` might be None if we are in request phase.
                            // RuleEngine usually sets `http.response` when Mock action is taken.
                            if let Some(res) = &http.response {
                                InterceptionResult::MockResponse(res.clone())
                            } else {
                                InterceptionResult::Drop // Should not happen if rule engine is correct
                            }
                        }
                    }
                    Layer::WebSocket(ws) => {
                        if ws.handshake_response.status != 0 && ws.handshake_response.status != 101 {
                            InterceptionResult::MockResponse(ws.handshake_response.clone())
                        } else {
                            InterceptionResult::Drop
                        }
                    }
                    _ => InterceptionResult::Drop,
                }
            }
            TerminalReason::Inspect => {
                let (tx, rx) = oneshot::channel();
                let key = if let Some(msg) = ws_message {
                     format!("{}:ws_msg:{}", flow.id, msg.id)
                } else {
                     format!("{}:{}", flow.id, phase)
                };
                
                self.state.register_intercept(key.clone(), tx).await;
                
                if let Some(msg) = ws_message {
                    self.state.set_pending_ws_message(key.clone(), msg.clone()).await;
                }
                
                if let Err(e) = self.app_handle.emit("flow-intercept", &key) {
                    eprintln!("Failed to emit flow-intercept event: {}", e);
                }
                
                println!("Intercepted {}: {}", phase, key);

                match timeout(Duration::from_secs(30), rx).await {
                    Ok(Ok(result)) => {
                        println!("Resumed {}: {} with result {:?}", phase, key, result);
                        result
                    },
                    Ok(Err(_)) => {
                        eprintln!("Interception channel dropped for {}", key);
                        InterceptionResult::Continue
                    },
                    Err(_) => {
                        eprintln!("Interception timeout for {}", key);
                        InterceptionResult::Continue
                    }
                }
            },
        }
    }
}
