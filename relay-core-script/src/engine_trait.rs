use async_trait::async_trait;
use relay_core_api::flow::{Flow, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, ConnectAction, ConnectionInfo, ConnectionStats, HttpBody, RequestAction,
    ResponseAction, WebSocketMessageAction,
};

#[async_trait]
pub trait ScriptEngineTrait: Send + Sync {
    async fn load_script(&mut self, script: &str) -> Result<(), BoxError>;

    async fn on_connect(&self, _conn: &ConnectionInfo) -> Result<ConnectAction, BoxError> {
        Ok(ConnectAction::Allow)
    }

    async fn on_disconnect(
        &self,
        _conn: &ConnectionInfo,
        _stats: &ConnectionStats,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    async fn on_request_headers(&self, _flow: &mut Flow) -> Result<Option<Flow>, BoxError> {
        Ok(None)
    }

    async fn on_request(&self, flow: &mut Flow, body: HttpBody) -> Result<RequestAction, BoxError>;

    async fn on_response_headers(&self, _flow: &mut Flow) -> Result<Option<Flow>, BoxError> {
        Ok(None)
    }

    async fn on_response(
        &self,
        flow: &mut Flow,
        body: HttpBody,
    ) -> Result<ResponseAction, BoxError>;

    async fn on_websocket_message(
        &self,
        _flow: &mut Flow,
        _message: &mut WebSocketMessage,
    ) -> Result<WebSocketMessageAction, BoxError> {
        Ok(WebSocketMessageAction::Continue(_message.clone()))
    }

    async fn on_websocket_start(&self, _flow: &mut Flow) -> Result<(), BoxError> {
        Ok(())
    }

    async fn on_websocket_end(
        &self,
        _flow: &mut Flow,
        _close_code: u16,
        _close_reason: &str,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    async fn on_websocket_error(&self, _flow: &mut Flow, _error: &str) -> Result<(), BoxError> {
        Ok(())
    }
}
