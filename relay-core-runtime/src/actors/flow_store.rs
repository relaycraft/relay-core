use lru::LruCache;
use relay_core_api::flow::{BodyData, Direction, Flow, Layer, WebSocketMessage};
use relay_core_api::modification::{flow_matches_query, FlowQuery, FlowSummary};
use relay_core_storage::store::Store;
use std::num::NonZeroUsize;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum FlowStoreMessage {
    UpsertFlow(Box<Flow>),
    AppendWebSocketMessage {
        flow_id: String,
        message: WebSocketMessage,
    },
    UpdateHttpBody {
        flow_id: String,
        body: BodyData,
        direction: Direction,
    },
    GetFlow {
        id: String,
        respond_to: oneshot::Sender<Option<Flow>>,
    },
    GetMetrics(oneshot::Sender<(usize, usize)>), // total_processed, current_in_memory
    SearchFlows {
        query: FlowQuery,
        respond_to: oneshot::Sender<Vec<FlowSummary>>,
    },
}

pub struct FlowStoreActor {
    flows: LruCache<String, Flow>,
    receiver: mpsc::Receiver<FlowStoreMessage>,
    store: Option<Store>,
    total_processed: usize,
}

impl FlowStoreActor {
    pub fn new(receiver: mpsc::Receiver<FlowStoreMessage>, store: Option<Store>) -> Self {
        Self {
            flows: LruCache::new(NonZeroUsize::new(200).expect("cache size > 0")),
            receiver,
            store,
            total_processed: 0,
        }
    }

    async fn persist_flow(&self, flow: &Flow) {
        if let Some(store) = &self.store {
            let flow_json = serde_json::to_value(flow).unwrap_or_default();
            if let Err(e) = store.upsert_flow(&flow.id.to_string(), &flow_json).await {
                tracing::error!("Failed to persist flow {}: {}", flow.id, e);
            }
            let summary = flow_to_summary(flow);
            if let Err(e) = store.upsert_flow_summary(&summary).await {
                tracing::error!("Failed to persist flow summary {}: {}", flow.id, e);
            }
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.receiver.recv().await {
            match msg {
                FlowStoreMessage::UpsertFlow(flow) => {
                    self.total_processed += 1;
                    self.persist_flow(&flow).await;
                    self.flows.put(flow.id.to_string(), *flow);
                }
                FlowStoreMessage::AppendWebSocketMessage { flow_id, message } => {
                    let updated = if let Some(flow) = self.flows.get_mut(&flow_id) {
                        if let relay_core_api::flow::Layer::WebSocket(ws) = &mut flow.layer {
                            if ws.messages.len() >= 2000 {
                                ws.messages.remove(0);
                            }
                            ws.messages.push(message);
                        }
                        Some(flow.clone())
                    } else {
                        None
                    };
                    if let Some(flow) = updated {
                        self.persist_flow(&flow).await;
                    }
                }
                FlowStoreMessage::UpdateHttpBody {
                    flow_id,
                    body,
                    direction,
                } => {
                    let updated = if let Some(flow) = self.flows.get_mut(&flow_id) {
                        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
                            match direction {
                                Direction::ClientToServer => {
                                    http.request.body = Some(body);
                                }
                                Direction::ServerToClient => {
                                    if let Some(res) = &mut http.response {
                                        res.body = Some(body);
                                    }
                                }
                            }
                        }
                        Some(flow.clone())
                    } else {
                        None
                    };
                    if let Some(flow) = updated {
                        self.persist_flow(&flow).await;
                    }
                }
                FlowStoreMessage::GetFlow { id, respond_to } => {
                    let _ = respond_to.send(self.flows.get(&id).cloned());
                }
                FlowStoreMessage::GetMetrics(respond_to) => {
                    let _ = respond_to.send((self.total_processed, self.flows.len()));
                }
                FlowStoreMessage::SearchFlows { query, respond_to } => {
                    let limit = query.limit.unwrap_or(50).min(200);
                    let offset = query.offset.unwrap_or(0);
                    let mut results: Vec<FlowSummary> = self
                        .flows
                        .iter()
                        .filter(|(_, flow)| flow_matches_query(flow, &query))
                        .map(|(_, flow)| flow_to_summary(flow))
                        .collect();
                    results.sort_by_key(|r| std::cmp::Reverse(r.start_time_ms));
                    let results: Vec<FlowSummary> =
                        results.into_iter().skip(offset).take(limit).collect();
                    let _ = respond_to.send(results);
                }
            }
        }
    }
}

fn flow_to_summary(flow: &Flow) -> FlowSummary {
    let (method, url, host, path, status, is_ws) = match &flow.layer {
        Layer::Http(h) => (
            h.request.method.clone(),
            h.request.url.to_string(),
            h.request.url.host_str().unwrap_or("").to_string(),
            h.request.url.path().to_string(),
            h.response.as_ref().map(|r| r.status),
            false,
        ),
        Layer::WebSocket(ws) => (
            ws.handshake_request.method.clone(),
            ws.handshake_request.url.to_string(),
            ws.handshake_request
                .url
                .host_str()
                .unwrap_or("")
                .to_string(),
            ws.handshake_request.url.path().to_string(),
            Some(ws.handshake_response.status),
            true,
        ),
        _ => (
            "UNKNOWN".to_string(),
            String::new(),
            String::new(),
            String::new(),
            None,
            false,
        ),
    };

    let duration_ms = flow
        .end_time
        .map(|e| (e - flow.start_time).num_milliseconds().max(0) as u64);

    let has_error = status.is_some_and(|s| s >= 500) || flow.tags.iter().any(|t| t == "error");

    FlowSummary {
        id: flow.id.to_string(),
        method,
        url,
        host,
        path,
        status,
        duration_ms,
        tags: flow.tags.clone(),
        start_time_ms: flow.start_time.timestamp_millis(),
        has_error,
        is_websocket: is_ws,
    }
}
