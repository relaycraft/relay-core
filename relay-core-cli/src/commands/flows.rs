use crate::args::InterceptAction;
use anyhow::Result;
use futures_util::StreamExt;
use relay_core_api::flow::{FlowUpdate, Layer};
use tokio_tungstenite::connect_async;
use tracing::info;

pub async fn execute(control_url: String, output: String) -> Result<()> {
    let ws_url = if control_url.starts_with("https") {
        control_url.replace("https", "wss") + "/api/flows/ws"
    } else if control_url.starts_with("http") {
        control_url.replace("http", "ws") + "/api/flows/ws"
    } else {
        control_url + "/api/flows/ws"
    };

    info!("Connecting to {}", ws_url);
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to control server: {}", e))?;
    let (_, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to read message from control server: {}",
                    e
                ));
            }
        };

        if !msg.is_text() {
            continue;
        }

        let text = match msg.to_text() {
            Ok(text) => text,
            Err(_) => continue,
        };

        if output == "jsonl" {
            println!("{}", text);
            continue;
        }

        let update = match serde_json::from_str::<FlowUpdate>(text) {
            Ok(update) => update,
            Err(_) => continue,
        };

        match update {
            FlowUpdate::Full(flow) => {
                if output == "json" {
                    if let Ok(json) = serde_json::to_string_pretty(&flow) {
                        println!("{}", json);
                    }
                } else {
                    let url = match &flow.layer {
                        Layer::Http(h) => h.request.url.to_string(),
                        Layer::WebSocket(w) => w.handshake_request.url.to_string(),
                        _ => "unknown".to_string(),
                    };
                    let method = match &flow.layer {
                        Layer::Http(h) => h.request.method.clone(),
                        Layer::WebSocket(w) => w.handshake_request.method.clone(),
                        _ => "".to_string(),
                    };
                    info!("[Flow] {} {} {}", flow.id, method, url);
                }
            }
            FlowUpdate::WebSocketMessage { flow_id, message } => {
                if output == "table" {
                    info!("[WS] [{}] {} bytes", flow_id, message.content.size);
                }
            }
            FlowUpdate::HttpBody {
                flow_id,
                direction,
                body,
            } => {
                if output == "table" {
                    info!("[Body] [{}] {:?} {} bytes", flow_id, direction, body.size);
                }
            }
        }
    }

    Ok(())
}

pub async fn execute_intercept(action: InterceptAction, control_url: String) -> Result<()> {
    let client = reqwest::Client::new();
    let url = match action {
        InterceptAction::Pause => format!("{}/api/intercept/pause", control_url),
        InterceptAction::Resume => format!("{}/api/intercept/resume", control_url),
    };

    let res = client.post(&url).send().await?;
    if res.status().is_success() {
        let json: serde_json::Value = res.json().await?;
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        eprintln!("Request failed: {}", res.status());
    }
    Ok(())
}
