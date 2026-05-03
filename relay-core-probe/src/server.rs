use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::broadcast;

use relay_core_api::flow::FlowUpdate;
use relay_core_runtime::CoreState;
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Implementation,
        ListResourcesResult, ListResourceTemplatesResult, ListToolsResult,
        PaginatedRequestParams, RawResourceTemplate, ReadResourceRequestParams,
        ReadResourceResult, ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    ErrorData,
};

use crate::resources;
use crate::tools;

/// Probe server 的传输方式
#[derive(Debug, Clone)]
pub enum ProbeTransport {
    /// 通过 stdin/stdout 与 MCP 客户端通信（本地 AI 助手，如 Claude Desktop）
    Stdio,
    /// 通过 TCP 端口监听 SSE 连接（远程访问，需启用 transport-sse feature）
    Sse { port: u16, bind: IpAddr },
}

/// Probe server 配置
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    pub transport: ProbeTransport,
}

/// MCP 探针服务器。
///
/// 持有 `Arc<CoreState>`，实现 `rmcp::ServerHandler` trait，
/// 将 relay-core 的流量管理能力以 MCP Resources + Tools 形式暴露。
#[derive(Clone)]
pub struct ProbeServer {
    pub(crate) state: Arc<CoreState>,
    config: ProbeConfig,
}

impl ProbeServer {
    pub fn new(config: ProbeConfig, state: Arc<CoreState>) -> Self {
        Self { state, config }
    }

    /// 启动 MCP 服务，阻塞直到连接断开或 shutdown。
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        match &self.config.transport.clone() {
            ProbeTransport::Stdio => {
                let (stdin, stdout) = rmcp::transport::io::stdio();
                let running = rmcp::serve_server(self, (stdin, stdout)).await?;
                running.waiting().await?;
            }
            ProbeTransport::Sse { port, bind } => {
                let addr = SocketAddr::new(*bind, *port);
                tracing::warn!("SSE transport not compiled in; ignoring port {}", addr);
            }
        }
        Ok(())
    }

    /// 启动实时订阅循环：监听 broadcast，在有新流量时通知 MCP 客户端刷新资源。
    pub async fn run_subscription_loop(
        state: Arc<CoreState>,
        peer: rmcp::Peer<RoleServer>,
    ) {
        let mut rx: broadcast::Receiver<FlowUpdate> = state.subscribe_flow_updates();
        loop {
            match rx.recv().await {
                Ok(FlowUpdate::Full(flow)) => {
                    let flow_id = flow.id.to_string();
                    let _ = peer.notify_resource_updated(
                        ResourceUpdatedNotificationParam::new(format!("flows://{}", flow_id))
                    ).await;
                    let _ = peer.notify_resource_list_changed().await;
                }
                Ok(FlowUpdate::WebSocketMessage { flow_id, .. }) |
                Ok(FlowUpdate::HttpBody { flow_id, .. }) => {
                    let _ = peer.notify_resource_updated(
                        ResourceUpdatedNotificationParam::new(format!("flows://{}", flow_id))
                    ).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("probe subscriber lagged, dropped {} updates", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

impl ServerHandler for ProbeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("relay-core-probe", env!("CARGO_PKG_VERSION")))
        .with_instructions(format!(
            "relay-core traffic proxy probe (tool-contract-version: {}). \
            Use tools to search/inspect flows, manage interception rules, \
            and debug network traffic. \
            Tool contract is stable; new optional parameters may be added \
            without a version bump — ignore unknown fields.",
            crate::TOOL_CONTRACT_VERSION,
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(resources::static_resource_list()))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        use rmcp::model::AnnotateAble;
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            RawResourceTemplate::new("flows://{id}", "Flow Detail")
                .with_description("Full details of a specific flow by ID")
                .with_mime_type("application/json")
                .no_annotation(),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let contents = resources::read_resource(&self.state, &request.uri).await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        Ok(ReadResourceResult::new(contents))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        use serde_json::Value;
        let args = request.arguments
            .map(Value::Object)
            .unwrap_or(Value::Object(Default::default()));
        let content = tools::dispatch(&self.state, &request.name, args).await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        Ok(CallToolResult::success(content))
    }

    async fn on_initialized(&self, _ctx: NotificationContext<RoleServer>) {
        tracing::info!("relay-core-probe: MCP client connected");
    }
}
