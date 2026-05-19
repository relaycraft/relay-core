use async_trait::async_trait;
use relay_core_api::flow::{Flow, WebSocketMessage};
use relay_core_lib::interceptor::{
    BoxError, HttpBody, RequestAction, ResponseAction, WebSocketMessageAction,
};

#[async_trait]
pub trait ScriptEngineTrait: Send + Sync {
    async fn load_script(&mut self, script: &str) -> Result<(), BoxError>;

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
}
