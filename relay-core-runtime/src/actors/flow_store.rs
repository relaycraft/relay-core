use lru::LruCache;
use relay_core_api::event::CloseReason;
use relay_core_api::flow::{BodyData, Direction, Flow, Layer, ResilienceTrace, WebSocketMessage};
use relay_core_api::modification::{FlowQuery, FlowSummary, flow_matches_query};
use relay_core_api::policy::RedactionPolicy;
use relay_core_storage::store::Store;
use std::num::NonZeroUsize;
use std::sync::{Arc, RwLock};
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
    /// Trailers observed after the response body.
    SetResponseTrailers {
        flow_id: String,
        trailers: Vec<(String, String)>,
    },
    /// Tag flow as budget-exceeded (body too large for full rule inspection)
    TagBudgetExceeded {
        flow_id: String,
        direction: Direction,
    },
    /// The HTTP/2 stream was reset. Recorded apart from a body that simply ended short.
    NoteStreamReset {
        flow_id: String,
        detail: String,
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
    /// Delete captured flows from memory and from the database. The counts are
    /// `(flows, summaries)` removed.
    ClearCaptured {
        respond_to: oneshot::Sender<Result<(u64, u64), String>>,
    },
}

pub struct FlowStoreActor {
    flows: LruCache<String, Flow>,
    receiver: mpsc::Receiver<FlowStoreMessage>,
    store: Option<Store>,
    total_processed: usize,
    /// Redaction is applied **before** the bytes reach the database.
    ///
    /// Redaction used to exist only on the output path, so a configured policy still wrote raw
    /// headers, URLs and bodies to disk: the file held secrets the API was busy hiding. The handle
    /// is shared with `CoreState`, so a runtime policy change applies to subsequent writes without
    /// restarting the actor.
    redaction: Arc<RwLock<RedactionPolicy>>,
}

impl FlowStoreActor {
    pub fn new(
        receiver: mpsc::Receiver<FlowStoreMessage>,
        store: Option<Store>,
        redaction: Arc<RwLock<RedactionPolicy>>,
    ) -> Self {
        Self {
            flows: LruCache::new(NonZeroUsize::new(200).expect("cache size > 0")),
            receiver,
            store,
            total_processed: 0,
            redaction,
        }
    }

    /// Apply the current redaction policy to a flow before it is written.
    fn redact(&self, flow: &Flow) -> Flow {
        let policy = self.redaction.read().map(|p| p.clone()).unwrap_or_default();
        crate::redact_flow(flow.clone(), &policy)
    }

    fn redaction_policy(&self) -> RedactionPolicy {
        self.redaction.read().map(|p| p.clone()).unwrap_or_default()
    }

    async fn persist_flow(&self, flow: &Flow) {
        if let Some(store) = &self.store {
            // Never write what the policy says must be hidden.
            let flow = self.redact(flow);
            let flow_json = serde_json::to_value(&flow).unwrap_or_default();
            if let Err(e) = store.upsert_flow(&flow.id.to_string(), &flow_json).await {
                tracing::error!("Failed to persist flow {}: {}", flow.id, e);
            }
            let summary =
                crate::redact_flow_summary(flow_to_summary(&flow), &self.redaction_policy());
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
                    let mut flow = *flow;
                    // A tapped body is reported as an incremental update, but the finished exchange
                    // arrives as a `Full` snapshot built from the proxy's local flow, which never
                    // held that body. Replacing the stored flow wholesale would erase it — most
                    // visibly the request body, since it is captured before the response and the
                    // final snapshot therefore lands on top of it. Keep what the snapshot omits.
                    if let Some(existing) = self.flows.peek(&flow.id.to_string()) {
                        merge_incremental(existing, &mut flow);
                    }
                    self.persist_flow(&flow).await;
                    self.flows.put(flow.id.to_string(), flow);
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
                FlowStoreMessage::SetResponseTrailers { flow_id, trailers } => {
                    let updated = if let Some(flow) = self.flows.get_mut(&flow_id) {
                        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer
                            && let Some(res) = &mut http.response
                        {
                            res.trailers = trailers;
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
                FlowStoreMessage::ClearCaptured { respond_to } => {
                    self.flows.clear();
                    let result = if let Some(store) = &self.store {
                        match store.clear_captured_flows().await {
                            Ok(counts) => Ok((counts.flows, counts.flow_summaries)),
                            Err(error) => Err(error.to_string()),
                        }
                    } else {
                        Ok((0, 0))
                    };
                    let _ = respond_to.send(result);
                }
                FlowStoreMessage::NoteStreamReset { flow_id, detail } => {
                    let updated = if let Some(flow) = self.flows.get_mut(&flow_id) {
                        flow.close_reason = Some(CloseReason::Reset);
                        if !flow.tags.iter().any(|tag| tag == "stream-reset") {
                            flow.tags.push("stream-reset".to_string());
                        }
                        if let relay_core_api::flow::Layer::Http(http) = &mut flow.layer {
                            http.error = Some(format!("stream reset: {detail}"));
                        }
                        Some(flow.clone())
                    } else {
                        None
                    };
                    if let Some(flow) = updated {
                        self.persist_flow(&flow).await;
                    }
                }
                FlowStoreMessage::TagBudgetExceeded { flow_id, direction } => {
                    let direction_tag = match direction {
                        Direction::ClientToServer => "budget-exceeded:client-to-server",
                        Direction::ServerToClient => "budget-exceeded:server-to-client",
                    };
                    let updated = if let Some(flow) = self.flows.get_mut(&flow_id) {
                        if !flow.tags.contains(&"budget-exceeded".to_string()) {
                            flow.tags.push("budget-exceeded".to_string());
                        }
                        if !flow.tags.contains(&direction_tag.to_string()) {
                            flow.tags.push(direction_tag.to_string());
                        }
                        flow.resilience_trace = Some(ResilienceTrace {
                            budget_exceeded: true,
                            ..flow.resilience_trace.clone().unwrap_or_default()
                        });
                        Some(flow.clone())
                    } else {
                        None
                    };
                    if let Some(flow) = updated {
                        self.persist_flow(&flow).await;
                    }
                }
            }
        }
    }
}

/// Carry incremental fields from the stored flow into an incoming `Full` snapshot that omits them.
///
/// The proxy's `Full` snapshot is built from its local `Flow`, which does not hold bodies the
/// streaming tap wrote straight to the store. Applying that snapshot verbatim erased them. Bodies
/// and trailers are only ever *added* incrementally (a rule replacement is present on the proxy's
/// local flow and therefore in the snapshot), so an omitted field means "not carried by this
/// update", never "clear it".
fn merge_incremental(existing: &Flow, incoming: &mut Flow) {
    let (Layer::Http(existing_http), Layer::Http(incoming_http)) =
        (&existing.layer, &mut incoming.layer)
    else {
        return;
    };

    if incoming_http.request.body.is_none() {
        incoming_http.request.body = existing_http.request.body.clone();
    }

    let (Some(existing_res), Some(incoming_res)) = (
        existing_http.response.as_ref(),
        incoming_http.response.as_mut(),
    ) else {
        return;
    };

    if incoming_res.body.is_none() {
        incoming_res.body = existing_res.body.clone();
    }
    if incoming_res.trailers.is_empty() {
        incoming_res.trailers = existing_res.trailers.clone();
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

    // Both ends are truncated to milliseconds *before* subtracting, which is what a client does when
    // it parses the two RFC3339 timestamps — so the same row cannot read `1 ms` while it is live and
    // `0 ms` after a refresh. Subtracting first and truncating the difference does that.
    let duration_ms = flow
        .end_time
        .map(|e| (e.timestamp_millis() - flow.start_time.timestamp_millis()).max(0) as u64);

    let (grpc_status, grpc_message) = match &flow.layer {
        Layer::Http(http) => http
            .response
            .as_ref()
            .map(|response| relay_core_api::grpc::outcome_from_trailers(&response.trailers))
            .unwrap_or((None, None)),
        _ => (None, None),
    };
    let has_error = relay_core_api::flow::flow_is_error(flow);

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
        grpc_status,
        grpc_message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use relay_core_api::flow::{
        HttpLayer, HttpRequest, HttpResponse, NetworkInfo, ResponseTiming, TransportProtocol,
    };
    use url::Url;
    use uuid::Uuid;

    /// A summary must be a faithful, host-independent description of the flow.
    ///
    /// The web UI draws a row twice — once from this summary after a refresh, once from the `Flow`
    /// itself while the capture is live — and the two renderings have to agree, or the same row
    /// changes appearance when the user reloads the page. That is what these assertions pin down.
    fn flow(url: &str, start_ms: i64, end_ms: Option<i64>) -> Flow {
        let request = HttpRequest {
            method: "GET".into(),
            url: Url::parse(url).unwrap(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            cookies: vec![],
            query: vec![],
            body: None,
        };
        Flow {
            id: Uuid::new_v4(),
            start_time: Utc.timestamp_millis_opt(start_ms).unwrap(),
            end_time: end_ms.map(|ms| Utc.timestamp_millis_opt(ms).unwrap()),
            close_reason: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".into(),
                client_port: 1,
                server_ip: "127.0.0.1".into(),
                server_port: 18090,
                server_host: None,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request,
                response: Some(HttpResponse {
                    status: 200,
                    status_text: "OK".into(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    body: None,
                    trailers: vec![],
                    timing: ResponseTiming {
                        time_to_first_byte: None,
                        time_to_last_byte: None,
                        connect_time_ms: None,
                        ssl_time_ms: None,
                    },
                }),
                error: None,
            }),
            tags: vec![],
            meta: std::collections::HashMap::new(),
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[test]
    fn host_is_the_host_and_path_is_the_path() {
        // The query string lives in `url` and nowhere else: `host` and `path` stay what every other
        // matcher in the engine means by those words (policy, rules, `flow_query`), and the interface
        // renders the full target from `url`. The client side follows the same rule — see
        // `webui/src/lib/flowSummary.ts`.
        let summary = flow_to_summary(&flow(
            "http://example.com:8080/a/b?token=secret",
            1_000,
            Some(1_002),
        ));
        assert_eq!(summary.host, "example.com");
        assert_eq!(summary.path, "/a/b");
        assert_eq!(summary.url, "http://example.com:8080/a/b?token=secret");
    }

    #[test]
    fn duration_truncates_both_ends_before_subtracting() {
        // A browser parses the two timestamps into milliseconds and subtracts, so a sub-millisecond
        // `start_time` has to be truncated on this side too: 0.6 ms → 1.4 ms is `0 ms` to the browser
        // and was `1 ms` here, which made the duration column change on refresh.
        let summary = flow_to_summary(&flow("http://example.com/x", 1_000, Some(1_000)));
        assert_eq!(summary.duration_ms, Some(0));

        let mut f = flow("http://example.com/x", 1_000, Some(1_001));
        // 0.9 ms of a millisecond: the difference is 0.2 ms, which truncates to zero.
        f.start_time =
            Utc.timestamp_millis_opt(1_000).unwrap() + chrono::Duration::microseconds(900);
        f.end_time =
            Some(Utc.timestamp_millis_opt(1_001).unwrap() + chrono::Duration::microseconds(100));
        assert_eq!(flow_to_summary(&f).duration_ms, Some(1));
    }

    #[test]
    fn a_flow_with_no_end_time_has_no_duration() {
        assert_eq!(
            flow_to_summary(&flow("http://example.com/x", 1_000, None)).duration_ms,
            None
        );
    }

    #[test]
    fn an_upstream_failure_is_flagged_as_an_error() {
        let mut f = flow("http://example.com/x", 1_000, Some(1_001));
        if let Layer::Http(http) = &mut f.layer {
            http.response = None;
            http.error = Some("Upstream Error: connection refused".into());
        }
        let summary = flow_to_summary(&f);
        assert!(summary.has_error, "the error badge must survive a refresh");
        assert_eq!(summary.status, None);
    }
}
