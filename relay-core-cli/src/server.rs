use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::{get, post},
};
use relay_core_api::flow::FlowUpdate;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::broadcast;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub flow_tx: broadcast::Sender<FlowUpdate>,
    pub interception_enabled: Arc<AtomicBool>,
}

pub async fn start_server(
    port: u16,
    flow_tx: broadcast::Sender<FlowUpdate>,
    interception_enabled: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let state = AppState {
        flow_tx,
        interception_enabled,
    };

    let app = Router::new()
        .route("/api/flows/ws", get(ws_handler))
        .route("/api/status", get(status_handler))
        .route("/api/intercept", get(intercept_status_handler))
        .route("/api/intercept/pause", post(intercept_pause_handler))
        .route("/api/intercept/resume", post(intercept_resume_handler))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Control API listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind control API to {addr}"))?;
    axum::serve(listener, app)
        .await
        .with_context(|| "Control API server error")?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.flow_tx.subscribe();

    while let Ok(update) = rx.recv().await {
        if let Ok(json) = serde_json::to_string(&update)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            // Client disconnected
            break;
        }
    }
}

async fn status_handler() -> Json<serde_json::Value> {
    Json(json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn intercept_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let enabled = state.interception_enabled.load(Ordering::Relaxed);
    Json(json!({
        "enabled": enabled
    }))
}

async fn intercept_pause_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.interception_enabled.store(false, Ordering::Relaxed);
    info!("Interception PAUSED via Control API");
    Json(json!({
        "status": "paused",
        "enabled": false
    }))
}

async fn intercept_resume_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.interception_enabled.store(true, Ordering::Relaxed);
    info!("Interception RESUMED via Control API");
    Json(json!({
        "status": "resumed",
        "enabled": true
    }))
}
