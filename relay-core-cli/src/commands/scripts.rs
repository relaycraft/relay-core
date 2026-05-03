use crate::args::ScriptsAction;
use crate::utils::load_flow;
use anyhow::Result;
use relay_core_script::deno_engine::DenoScriptEngine;
use relay_core_script::engine_trait::ScriptEngineTrait;
use relay_core_lib::intercept::types::{RequestAction, HttpBody, BoxError};
use relay_core_api::flow::{Flow, Layer};
use http_body_util::{Full, BodyExt};
use bytes::Bytes;
use base64::Engine as _;

fn body_from_flow(flow: &Flow) -> HttpBody {
    if let Layer::Http(http) = &flow.layer {
        if let Some(body_data) = &http.request.body {
            let bytes = if body_data.encoding == "base64" {
                base64::engine::general_purpose::STANDARD.decode(&body_data.content).unwrap_or_default().into()
            } else {
                Bytes::from(body_data.content.clone())
            };
            return Full::new(bytes).map_err(|e| Box::new(e) as BoxError).boxed();
        }
    }
    Full::new(Bytes::new()).map_err(|e| Box::new(e) as BoxError).boxed()
}

pub async fn execute(action: ScriptsAction) -> Result<()> {
    match action {
        ScriptsAction::Validate { file } => {
            let content = std::fs::read_to_string(&file)?;
            let mut engine = DenoScriptEngine::new();
            match engine.load_script(&content).await {
                Ok(_) => println!("Script syntax is valid."),
                Err(e) => {
                    eprintln!("Script validation failed: {}", e);
                    std::process::exit(1);
                }
            }
        },
        ScriptsAction::RunOnce { file, flow } => {
            let content = std::fs::read_to_string(&file)?;
            let mut flow_data = load_flow(&flow)?;
            
            let mut engine = DenoScriptEngine::new();
            engine.load_script(&content).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            
            println!("Running script against flow {}...", flow_data.id);
            let body = body_from_flow(&flow_data);
            match engine.on_request(&mut flow_data, body).await.map_err(|e| anyhow::anyhow!("{}", e)) {
                Ok(RequestAction::Continue(_)) => {
                    println!("Script executed successfully.");
                    // The flow object (headers, url, etc.) might have been modified in place.
                    if let Layer::Http(http) = &flow_data.layer {
                         println!("{}", serde_json::to_string_pretty(&http.request)?);
                    } else {
                         println!("Flow is not HTTP.");
                    }
                },
                Ok(RequestAction::MockResponse(res)) => {
                    println!("Script mocked a response.");
                    println!("Status: {}", res.status());
                    println!("Headers: {:?}", res.headers());
                },
                Ok(RequestAction::Drop) => println!("Script dropped the request."),
                Err(e) => {
                    eprintln!("Script execution error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}
