use relay_core_api::flow::WebSocketMessage;
use relay_core_lib::InterceptionResult;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum InterceptBrokerMessage {
    RegisterIntercept {
        key: String,
        tx: oneshot::Sender<InterceptionResult>,
    },
    ResolveIntercept {
        key: String,
        result: InterceptionResult,
        respond_to: oneshot::Sender<Result<(), String>>,
    },
    GetPendingIntercept {
        key: String,
        respond_to: oneshot::Sender<bool>,
    },
    GetPendingInterceptByFlowId {
        flow_id: String,
        respond_to: oneshot::Sender<bool>,
    },
    GetPendingWebSocketMessage {
        key: String,
        respond_to: oneshot::Sender<Option<WebSocketMessage>>,
    },
    SetPendingWebSocketMessage {
        key: String,
        message: WebSocketMessage,
    },
    GetMetrics {
        respond_to: oneshot::Sender<(usize, usize, Option<u64>, Option<u64>)>, // pending_intercepts, pending_ws_messages, oldest_intercept_age_ms, oldest_ws_message_age_ms
    },
    ListPendingIntercepts {
        respond_to: oneshot::Sender<Vec<(String, u64)>>,
    },
}

pub struct InterceptBrokerActor {
    pending_intercepts: HashMap<String, (oneshot::Sender<InterceptionResult>, Instant)>,
    pending_ws_messages: HashMap<String, (WebSocketMessage, Instant)>,
    receiver: mpsc::Receiver<InterceptBrokerMessage>,
    ttl: Duration,
}

impl InterceptBrokerActor {
    pub fn new(receiver: mpsc::Receiver<InterceptBrokerMessage>) -> Self {
        Self {
            pending_intercepts: HashMap::new(),
            pending_ws_messages: HashMap::new(),
            receiver,
            ttl: Duration::from_secs(300), // 5 minutes TTL
        }
    }

    fn cleanup(&mut self) {
        let now = Instant::now();
        self.pending_intercepts
            .retain(|_, (_, time)| now.duration_since(*time) < self.ttl);
        self.pending_ws_messages
            .retain(|_, (_, time)| now.duration_since(*time) < self.ttl);
    }

    pub async fn run(mut self) {
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = cleanup_interval.tick() => {
                    self.cleanup();
                }
                msg = self.receiver.recv() => {
                    match msg {
                        Some(msg) => match msg {
                            InterceptBrokerMessage::RegisterIntercept { key, tx } => {
                                self.pending_intercepts.insert(key, (tx, Instant::now()));
                            },
                            InterceptBrokerMessage::ResolveIntercept { key, result, respond_to } => {
                                // Remove related pending messages
                                self.pending_ws_messages.remove(&key);

                                if let Some((tx, _)) = self.pending_intercepts.remove(&key) {
                                    let _ = tx.send(result);
                                    let _ = respond_to.send(Ok(()));
                                } else {
                                    let _ = respond_to.send(Err(format!("Interception not found for {}", key)));
                                }
                            },
                            InterceptBrokerMessage::GetPendingIntercept { key, respond_to } => {
                                let _ = respond_to.send(self.pending_intercepts.contains_key(&key));
                            },
                            InterceptBrokerMessage::GetPendingInterceptByFlowId { flow_id, respond_to } => {
                                let prefix = format!("{}:", flow_id);
                                let found = self.pending_intercepts.keys().any(|k| k.starts_with(&prefix));
                                let _ = respond_to.send(found);
                            },
                            InterceptBrokerMessage::GetPendingWebSocketMessage { key, respond_to } => {
                                 let _ = respond_to.send(self.pending_ws_messages.get(&key).map(|(m, _)| m.clone()));
                            },
                            InterceptBrokerMessage::SetPendingWebSocketMessage { key, message } => {
                                self.pending_ws_messages.insert(key, (message, Instant::now()));
                            },
                            InterceptBrokerMessage::GetMetrics { respond_to } => {
                                let now = Instant::now();
                                let oldest_intercept_age_ms = self
                                    .pending_intercepts
                                    .values()
                                    .map(|(_, created_at)| now.duration_since(*created_at).as_millis() as u64)
                                    .max();
                                let oldest_ws_message_age_ms = self
                                    .pending_ws_messages
                                    .values()
                                    .map(|(_, created_at)| now.duration_since(*created_at).as_millis() as u64)
                                    .max();
                                let _ = respond_to.send((
                                    self.pending_intercepts.len(),
                                    self.pending_ws_messages.len(),
                                    oldest_intercept_age_ms,
                                    oldest_ws_message_age_ms,
                                ));
                            }
                            InterceptBrokerMessage::ListPendingIntercepts { respond_to } => {
                                let now = Instant::now();
                                let items: Vec<(String, u64)> = self
                                    .pending_intercepts
                                    .iter()
                                    .map(|(key, (_, created_at))| {
                                        (
                                            key.clone(),
                                            now.duration_since(*created_at).as_millis() as u64,
                                        )
                                    })
                                    .collect();
                                let _ = respond_to.send(items);
                            }
                        },
                        None => break,
                    }
                }
            }
        }
    }
}
