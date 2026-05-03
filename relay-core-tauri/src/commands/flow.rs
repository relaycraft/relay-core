use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::mpsc;
use crate::transport::{FlowIndex, FlowDetail};
use crate::RelayCoreState;
use relay_core_api::flow::{BodyData, FlowUpdate, Direction};
use relay_core_api::modification::FlowModification;
use relay_core_runtime::audit::AuditActor;
use serde::Deserialize;
use std::collections::HashMap;

pub struct TauriFlowSink<R: Runtime> {
    pub app_handle: AppHandle<R>,
}

impl<R: Runtime> TauriFlowSink<R> {
    pub async fn run(self, mut rx: mpsc::Receiver<FlowUpdate>) {
        while let Some(update) = rx.recv().await {
            match update {
                FlowUpdate::Full(flow) => {
                    let mut index = FlowIndex::from(*flow);
                    if let Some(state) = self.app_handle.try_state::<RelayCoreState>() {
                        index.is_intercepted = state.core.is_flow_intercepted(index.id.clone()).await;
                    }
                    if let Err(e) = self.app_handle.emit("flow-update", index) {
                        eprintln!("Failed to emit flow-update event: {}", e);
                    }
                },
                FlowUpdate::WebSocketMessage { flow_id, message } => {
                    #[derive(serde::Serialize, Clone)]
                    #[serde(rename_all = "camelCase")]
                    struct WebSocketMessageEvent {
                        flow_id: String,
                        message: relay_core_api::flow::WebSocketMessage,
                    }
                    let event = WebSocketMessageEvent { flow_id, message };
                    if let Err(e) = self.app_handle.emit("flow-ws-update", event) {
                        eprintln!("Failed to emit flow-ws-update event: {}", e);
                    }
                },
                FlowUpdate::HttpBody { flow_id, direction, body } => {
                    #[derive(serde::Serialize, Clone)]
                    #[serde(rename_all = "camelCase")]
                    struct HttpBodyEvent {
                        flow_id: String,
                        direction: Direction,
                        body: BodyData,
                    }
                    let event = HttpBodyEvent { flow_id, direction, body };
                    if let Err(e) = self.app_handle.emit("flow-body-update", event) {
                        eprintln!("Failed to emit flow-body-update event: {}", e);
                    }
                }
            }
        }
    }
}

/// Tauri 前端使用的修改请求，字段命名遵循 camelCase。
/// 转换为协议无关的 FlowModification 后交给 CoreState 处理。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modification {
    pub method: Option<String>,
    pub url: Option<String>,
    pub request_headers: Option<HashMap<String, String>>,
    pub request_body: Option<String>,
    pub status_code: Option<u16>,
    pub response_headers: Option<HashMap<String, String>>,
    pub response_body: Option<String>,
    pub message_content: Option<String>,
}

impl From<Modification> for FlowModification {
    fn from(m: Modification) -> Self {
        FlowModification {
            method: m.method,
            url: m.url,
            request_headers: m.request_headers,
            request_body: m.request_body,
            status_code: m.status_code,
            response_headers: m.response_headers,
            response_body: m.response_body,
            message_content: m.message_content,
        }
    }
}

#[tauri::command]
pub async fn get_flow_detail(state: State<'_, RelayCoreState>, id: String) -> Result<FlowDetail, String> {
    get_flow_detail_impl(&state, id).await
}

pub async fn get_flow_detail_impl(state: &RelayCoreState, id: String) -> Result<FlowDetail, String> {
    if let Some(flow) = state.core.get_flow(id.clone()).await {
        let mut detail = FlowDetail::from(flow);
        detail._rc.intercept.intercepted = state.core.is_flow_intercepted(id).await;
        Ok(detail)
    } else {
        Err(format!("Flow not found: {}", id))
    }
}

#[tauri::command]
pub async fn resume_flow(
    state: State<'_, RelayCoreState>,
    id: String,
    action: String,
    modifications: Option<Modification>,
) -> Result<(), String> {
    resume_flow_impl(&state, id, action, modifications).await
}

pub async fn resume_flow_impl(
    state: &RelayCoreState,
    id: String,
    action: String,
    modifications: Option<Modification>,
) -> Result<(), String> {
    let mods = modifications
        .map(FlowModification::from)
        .and_then(FlowModification::into_option);
    state
        .core
        .resolve_intercept_with_modifications_from(AuditActor::Tauri, id, &action, mods)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core_api::flow::{Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol};
    use relay_core_lib::InterceptionResult;
    use chrono::Utc;
    use uuid::Uuid;
    use url::Url;
    use std::collections::HashMap;

    fn create_test_http_flow() -> Flow {
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
                    url: Url::parse("http://example.com/api").unwrap(),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![("User-Agent".to_string(), "TestBot".to_string())],
                    body: None,
                    cookies: vec![],
                    query: vec![],
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_get_flow_detail_impl_marks_intercepted_flag() {
        let state = crate::RelayCoreState::new_async().await;
        let flow = create_test_http_flow();
        let flow_id = flow.id.to_string();
        state.core.upsert_flow(Box::new(flow));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let intercept_key = format!("{}:request_headers", flow_id);
        state.core.register_intercept(intercept_key.clone(), tx).await;

        let detail = get_flow_detail_impl(&state, flow_id.clone())
            .await
            .expect("flow detail should exist");
        assert!(detail._rc.intercept.intercepted, "should be intercepted=true while pending");

        state
            .core
            .resolve_intercept(intercept_key, InterceptionResult::Continue)
            .await
            .expect("resolve should succeed");

        let detail_after = get_flow_detail_impl(&state, flow_id)
            .await
            .expect("flow detail should still exist");
        assert!(!detail_after._rc.intercept.intercepted, "should be intercepted=false after resolve");
    }

    #[tokio::test]
    async fn test_get_flow_detail_impl_returns_not_found() {
        let state = crate::RelayCoreState::new_async().await;
        let result = get_flow_detail_impl(&state, "missing-flow".to_string()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resume_flow_impl_returns_error_when_intercept_missing() {
        let state = crate::RelayCoreState::new_async().await;
        let result = resume_flow_impl(
            &state,
            "missing-flow:request".to_string(),
            "continue".to_string(),
            None,
        )
        .await;
        assert!(result.is_err(), "resume should fail when interception key does not exist");
    }

    #[tokio::test]
    async fn test_resume_flow_impl_missing_flow_with_mods_falls_back_continue() {
        let state = crate::RelayCoreState::new_async().await;
        let intercept_key = "missing-flow:request".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.core.register_intercept(intercept_key.clone(), tx).await;

        let mods = Modification {
            method: Some("PATCH".to_string()),
            url: Some("http://example.com/new".to_string()),
            request_headers: None,
            request_body: Some("payload".to_string()),
            status_code: None,
            response_headers: None,
            response_body: None,
            message_content: None,
        };
        let result = resume_flow_impl(&state, intercept_key.clone(), "continue".to_string(), Some(mods)).await;
        assert!(result.is_ok(), "registered intercept should be resolvable");

        let recv = rx.await.expect("should receive interception decision");
        assert!(
            matches!(recv, InterceptionResult::Continue),
            "when flow is absent, modifications should degrade to Continue"
        );
        assert!(!state.core.is_intercept_pending(intercept_key).await, "key should be cleared");
    }

    #[tokio::test]
    async fn test_resume_flow_impl_drop_action_resolves_to_drop() {
        let state = crate::RelayCoreState::new_async().await;
        let intercept_key = "flow-drop-1:request".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.core.register_intercept(intercept_key.clone(), tx).await;

        let result = resume_flow_impl(
            &state,
            intercept_key.clone(),
            "drop".to_string(),
            Some(Modification {
                method: Some("PATCH".to_string()),
                url: Some("http://example.com/ignored".to_string()),
                request_headers: None,
                request_body: None,
                status_code: None,
                response_headers: None,
                response_body: None,
                message_content: None,
            }),
        )
        .await;
        assert!(result.is_ok(), "drop should resolve registered intercept");

        let recv = rx.await.expect("should receive interception decision");
        assert!(matches!(recv, InterceptionResult::Drop));
        assert!(!state.core.is_intercept_pending(intercept_key).await, "key should be cleared after drop");
    }

    #[tokio::test]
    async fn test_resume_flow_impl_ws_missing_pending_message_falls_back_continue() {
        let state = crate::RelayCoreState::new_async().await;
        let intercept_key = "flow-ws-missing:ws_msg:msg-1".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.core.register_intercept(intercept_key.clone(), tx).await;

        let result = resume_flow_impl(
            &state,
            intercept_key.clone(),
            "continue".to_string(),
            Some(Modification {
                method: None,
                url: None,
                request_headers: None,
                request_body: None,
                status_code: None,
                response_headers: None,
                response_body: None,
                message_content: Some("new-content".to_string()),
            }),
        )
        .await;
        assert!(result.is_ok(), "registered ws intercept should resolve");

        let recv = rx.await.expect("should receive interception decision");
        assert!(
            matches!(recv, InterceptionResult::Continue),
            "without pending ws message, resume should degrade to Continue"
        );
        assert!(!state.core.is_intercept_pending(intercept_key).await, "key should be cleared");
    }

    #[tokio::test]
    async fn test_resume_flow_impl_malformed_intercept_key_falls_back_continue() {
        let state = crate::RelayCoreState::new_async().await;
        let intercept_key = "flow-malformed-key".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.core.register_intercept(intercept_key.clone(), tx).await;

        let mods = Modification {
            method: Some("PATCH".to_string()),
            url: Some("http://example.com/new".to_string()),
            request_headers: None,
            request_body: Some("payload".to_string()),
            status_code: None,
            response_headers: None,
            response_body: None,
            message_content: None,
        };
        let result = resume_flow_impl(&state, intercept_key.clone(), "continue".to_string(), Some(mods)).await;
        assert!(result.is_ok(), "registered intercept should be resolvable");

        let recv = rx.await.expect("should receive interception decision");
        assert!(
            matches!(recv, InterceptionResult::Continue),
            "malformed intercept key should degrade to Continue"
        );
        assert!(!state.core.is_intercept_pending(intercept_key).await, "key should be cleared");
    }
}
