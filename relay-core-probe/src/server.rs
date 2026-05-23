use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::broadcast;

use relay_core_api::flow::FlowUpdate;
use relay_core_runtime::CoreState;
use relay_core_runtime::audit::AuditEventKind;
use relay_core_runtime::services::{
    AuditService, FlowEventHub, FlowReadService, InterceptService, PolicyService, RuleService,
    RuntimeStatusService, ScriptService,
};
use rmcp::{
    ErrorData, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorCode, Implementation,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        RawResourceTemplate, ReadResourceRequestParams, ReadResourceResult,
        ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo,
    },
    service::{NotificationContext, RequestContext, RoleServer},
};

use crate::resources;
use crate::tools::{self, ToolError};

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

/// Shared context for probe tools/resources, exposing only narrow-capability traits.
pub struct ProbeContext {
    pub flows: Arc<dyn FlowReadService>,
    pub flow_events: Arc<dyn FlowEventHub>,
    pub rules: Arc<dyn RuleService>,
    pub intercepts: Arc<dyn InterceptService>,
    pub audit: Arc<dyn AuditService>,
    pub status: Arc<dyn RuntimeStatusService>,
    pub policy: Arc<dyn PolicyService>,
    pub script: Arc<dyn ScriptService>,
}

impl ProbeContext {
    pub fn new(core: Arc<CoreState>) -> Self {
        Self {
            flows: core.clone(),
            flow_events: core.clone(),
            rules: core.clone(),
            intercepts: core.clone(),
            audit: core.clone(),
            status: core.clone(),
            policy: core.clone(),
            script: core.clone(),
        }
    }
}

/// MCP 探针服务器。
///
/// Holds `ProbeContext` backed by `CoreState`, implementing `rmcp::ServerHandler` trait.
#[derive(Clone)]
pub struct ProbeServer {
    pub(crate) ctx: Arc<ProbeContext>,
    config: ProbeConfig,
}

impl ProbeServer {
    pub fn new(config: ProbeConfig, state: Arc<CoreState>) -> Self {
        let ctx = Arc::new(ProbeContext::new(state));
        Self { ctx, config }
    }

    /// 启动 MCP 服务，阻塞直到连接断开或 shutdown。
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        match &self.config.transport.clone() {
            ProbeTransport::Stdio => {
                let (stdin, stdout) = rmcp::transport::io::stdio();
                let ctx = self.ctx.clone();
                let running = rmcp::serve_server(self, (stdin, stdout)).await?;
                let peer = running.peer().clone();
                tokio::spawn(async move {
                    ProbeServer::run_subscription_loop(ctx, peer).await;
                });
                running.waiting().await?;
            }
            #[cfg(feature = "transport-sse")]
            ProbeTransport::Sse { port, bind } => {
                use rmcp::transport::streamable_http_server::{
                    StreamableHttpServerConfig, StreamableHttpService,
                    session::local::LocalSessionManager,
                };

                let addr = SocketAddr::new(*bind, *port);
                let config = StreamableHttpServerConfig {
                    sse_keep_alive: Some(std::time::Duration::from_secs(15)),
                    ..Default::default()
                };

                let srv = self.clone();

                let service = StreamableHttpService::new(
                    move || Ok(srv.clone()),
                    Arc::new(LocalSessionManager::default()),
                    config,
                );

                tokio::spawn(async move {
                    tracing::info!(
                        "relay-core MCP SSE transport: resource change notifications \
                         (K3) are not supported in SSE mode with the current rmcp session API. \
                         MCP clients can poll resources directly."
                    );
                });

                let router = axum::Router::new().nest_service("/mcp", service);
                let listener = tokio::net::TcpListener::bind(addr).await?;
                tracing::info!("relay-core MCP SSE listening on http://{}/mcp", addr);
                axum::serve(listener, router).await?;
            }
            #[cfg(not(feature = "transport-sse"))]
            ProbeTransport::Sse { port, bind } => {
                let addr = SocketAddr::new(*bind, *port);
                eprintln!(
                    "Error: SSE transport was requested (--transport=sse, port {}) \
                     but relay-core-probe was compiled without the 'transport-sse' feature.\n\
                     Rebuild with: cargo build --release --features transport-sse",
                    addr
                );
                std::process::exit(1);
            }
        }
        Ok(())
    }

    /// K3 extended: subscribes to flow + audit events and notifies peers.
    /// Used by Stdio transport (single peer).
    pub async fn run_subscription_loop(ctx: Arc<ProbeContext>, peer: rmcp::Peer<RoleServer>) {
        let mut flow_rx: broadcast::Receiver<FlowUpdate> = ctx.flow_events.subscribe_flow_updates();
        let mut audit_rx = ctx.audit.subscribe_audit_events();

        loop {
            tokio::select! {
                flow_event = flow_rx.recv() => {
                    match flow_event {
                        Ok(FlowUpdate::Full(flow)) => {
                            let flow_id = flow.id.to_string();
                            let _ = peer.notify_resource_updated(
                                ResourceUpdatedNotificationParam::new(
                                    format!("flows://{}", flow_id),
                                ),
                            ).await;
                            let _ = peer.notify_resource_list_changed().await;
                        }
                        Ok(FlowUpdate::WebSocketMessage { flow_id, .. })
                        | Ok(FlowUpdate::HttpBody { flow_id, .. })
                        | Ok(FlowUpdate::BodyBudgetExceeded { flow_id, .. }) => {
                            let _ = peer.notify_resource_updated(
                                ResourceUpdatedNotificationParam::new(
                                    format!("flows://{}", flow_id),
                                ),
                            ).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("probe flow subscriber lagged, dropped {} updates", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                audit_event = audit_rx.recv() => {
                    match audit_event {
                        Ok(event) => {
                            match event.kind {
                                AuditEventKind::RuleChanged | AuditEventKind::InterceptResolved => {
                                    let _ = peer.notify_resource_updated(
                                        ResourceUpdatedNotificationParam::new("rules://".to_string()),
                                    ).await;
                                }
                                AuditEventKind::PolicyUpdated => {
                                    let _ = peer.notify_resource_updated(
                                        ResourceUpdatedNotificationParam::new("proxy://status".to_string()),
                                    ).await;
                                }
                                AuditEventKind::ScriptReloaded => {
                                    let _ = peer.notify_resource_list_changed().await;
                                }
                            }
                            let _ = peer.notify_resource_updated(
                                ResourceUpdatedNotificationParam::new("audit://recent".to_string()),
                            ).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("probe audit subscriber lagged, dropped {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
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
        .with_server_info(Implementation::new(
            "relay-core-probe",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(format!(
            "relay-core traffic proxy probe (tool-contract-version: {}). \
            Use tools to search/inspect flows, manage interception rules, \
            and debug network traffic. \
            \
            To intercept HTTPS traffic, the relay-core CA certificate must be \
            trusted by the system (I cannot do this — it requires sudo). \
            Read the ca://install resource for platform-specific one-liner commands. \
            \
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
        Ok(ListResourcesResult::with_all_items(
            resources::static_resource_list(),
        ))
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
        let contents = resources::read_resource(&self.ctx, &request.uri)
            .await
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;
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
        let args = request
            .arguments
            .map(Value::Object)
            .unwrap_or(Value::Object(Default::default()));
        let content = tools::dispatch(&self.ctx, &request.name, args)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                match &e {
                    ToolError::NotFound(_) => {
                        ErrorData::new(ErrorCode::METHOD_NOT_FOUND, msg, None)
                    }
                    ToolError::InvalidArgument(_) => {
                        ErrorData::new(ErrorCode::INVALID_REQUEST, msg, None)
                    }
                    ToolError::Internal(_) => ErrorData::new(ErrorCode::INTERNAL_ERROR, msg, None),
                }
            })?;
        Ok(CallToolResult::success(content))
    }

    async fn on_initialized(&self, _ctx: NotificationContext<RoleServer>) {
        tracing::info!("relay-core-probe: MCP client connected");
    }
}
