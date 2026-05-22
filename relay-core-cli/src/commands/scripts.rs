use crate::args::ScriptsAction;
use crate::utils::load_flow;
use anyhow::Result;
use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use relay_core_api::flow::{Flow, Layer};
use relay_core_lib::intercept::types::{BoxError, HttpBody, RequestAction};
use relay_core_script::deno_engine::DenoScriptEngine;
use relay_core_script::engine_trait::ScriptEngineTrait;
use std::path::{Path, PathBuf};
use std::process::Command;

fn body_from_flow(flow: &Flow) -> HttpBody {
    if let Layer::Http(http) = &flow.layer
        && let Some(body_data) = &http.request.body
    {
        let bytes = if body_data.encoding == "base64" {
            base64::engine::general_purpose::STANDARD
                .decode(&body_data.content)
                .unwrap_or_default()
                .into()
        } else {
            Bytes::from(body_data.content.clone())
        };
        return Full::new(bytes)
            .map_err(|e| Box::new(e) as BoxError)
            .boxed();
    }
    Full::new(Bytes::new())
        .map_err(|e| Box::new(e) as BoxError)
        .boxed()
}

const ESBUILD_CONFIG: &str = r#"import * as esbuild from "esbuild";

await esbuild.build({
  entryPoints: ["src/index.ts"],
  bundle: true,
  outfile: "dist/bundle.js",
  platform: "neutral",
  target: "es2022",
  format: "iife",
  globalName: "globalThis",
  footer: { js: "globalThis.userScript = globalThis.userScript || {};" },
});
"#;

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "dist",
    "rootDir": "src"
  },
  "include": ["src"]
}
"#;

const SCRIPT_TEMPLATE: &str = r#"// RelayCore User Script
// Define any of the hook functions below — undefined hooks are ignored.

/**
 * Called when request headers are received (before body).
 * Return a modified flow object, or nothing to pass through.
 */
// globalThis.onRequestHeaders = (ctx, flow) => {
//   flow.tags.push("inspected");
//   return flow;
// };

/**
 * Called when request body is available.
 * `body` is a RelayBody instance with .text(), .json(), .read(limit).
 * Return a modified flow, or nothing to continue with original body.
 */
// globalThis.onRequest = async (body, flow) => {
//   const data = await body.json();
//   console.log("Request body:", JSON.stringify(data));
//   return flow;
// };

/**
 * Called when response headers are received.
 */
// globalThis.onResponseHeaders = (ctx, flow) => {
//   return flow;
// };

/**
 * Called when response body is available.
 */
// globalThis.onResponse = async (body, flow) => {
//   return flow;
// };

/**
 * Called for each WebSocket message.
 * Return "DROP" to drop the message, or a modified message object.
 */
// globalThis.onWebSocketMessage = (ctx, flow, message) => {
//   return message;
// };

/**
 * Called when any hook throws an error.
 */
// globalThis.onError = (ctx, flow, error, stage) => {
//   console.error(`Error in ${stage}: ${error}`);
// };
"#;

fn ensure_dir(dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn write_if_missing(path: &PathBuf, content: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        ensure_dir(&parent.to_path_buf())?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

fn run_esbuild(entry: &Path, out: &Path, watch: bool) -> Result<()> {
    let out_str = format!("--outfile={}", out.to_str().unwrap_or("dist/bundle.js"));
    let entry_str = entry.to_str().unwrap_or("src/index.ts");

    if watch {
        let status = Command::new("npx")
            .args([
                "esbuild",
                entry_str,
                "--bundle",
                &out_str,
                "--platform=neutral",
                "--target=es2022",
                "--format=iife",
                "--watch",
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("esbuild exited with code {}", status.code().unwrap_or(-1));
        }
    } else {
        let status = Command::new("npx")
            .args([
                "esbuild",
                entry_str,
                "--bundle",
                &out_str,
                "--platform=neutral",
                "--target=es2022",
                "--format=iife",
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("esbuild exited with code {}", status.code().unwrap_or(-1));
        }
    }
    Ok(())
}

pub async fn execute(action: ScriptsAction) -> Result<()> {
    match action {
        ScriptsAction::Validate { file } => {
            let content = std::fs::read_to_string(&file)?;
            let mut engine = DenoScriptEngine::new(std::collections::HashSet::new());
            match engine.load_script(&content).await {
                Ok(_) => println!("Script syntax is valid."),
                Err(e) => {
                    eprintln!("Script validation failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        ScriptsAction::RunOnce { file, flow } => {
            let content = std::fs::read_to_string(&file)?;
            let mut flow_data = load_flow(&flow)?;

            let mut engine = DenoScriptEngine::new(std::collections::HashSet::new());
            engine
                .load_script(&content)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            println!("Running script against flow {}...", flow_data.id);
            let body = body_from_flow(&flow_data);
            match engine
                .on_request(&mut flow_data, body)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
            {
                Ok(RequestAction::Continue(_)) => {
                    println!("Script executed successfully.");
                    if let Layer::Http(http) = &flow_data.layer {
                        println!("{}", serde_json::to_string_pretty(&http.request)?);
                    } else {
                        println!("Flow is not HTTP.");
                    }
                }
                Ok(RequestAction::MockResponse(res)) => {
                    println!("Script mocked a response.");
                    println!("Status: {}", res.status());
                    println!("Headers: {:?}", res.headers());
                }
                Ok(RequestAction::Drop) => println!("Script dropped the request."),
                Err(e) => {
                    eprintln!("Script execution error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        ScriptsAction::Init { dir } => {
            ensure_dir(&dir)?;
            let src_dir = dir.join("src");
            let dist_dir = dir.join("dist");
            ensure_dir(&src_dir)?;
            ensure_dir(&dist_dir)?;

            let esbuild_config = dir.join("esbuild.config.mjs");
            let tsconfig = dir.join("tsconfig.json");
            let script_entry = src_dir.join("index.ts");

            let wrote_cfg = write_if_missing(&esbuild_config, ESBUILD_CONFIG)?;
            let wrote_ts = write_if_missing(&tsconfig, TSCONFIG)?;
            let wrote_script = write_if_missing(&script_entry, SCRIPT_TEMPLATE)?;

            if wrote_cfg {
                println!("  created esbuild.config.mjs");
            }
            if wrote_ts {
                println!("  created tsconfig.json");
            }
            if wrote_script {
                println!("  created src/index.ts");
            }

            if wrote_cfg || wrote_ts || wrote_script {
                println!();
                println!("Next steps:");
                println!("  1. npm install --save-dev esbuild typescript");
                println!("  2. Add hooks to src/index.ts");
                println!("  3. relay-core scripts build     (one-shot bundle)");
                println!("  4. relay run --script dist/bundle.js --ui");
            } else {
                println!("Project already initialized in {:?}", dir);
            }
        }
        ScriptsAction::Build { entry, out } => {
            println!("Bundling {} -> {} ...", entry.display(), out.display());
            run_esbuild(&entry, &out, false)?;
            println!("Bundle written to {}", out.display());
            println!("Use with: relay run --script {} --ui", out.display());
        }
        ScriptsAction::Dev { entry, out } => {
            println!(
                "Watching {} -> {} (Ctrl+C to stop)...",
                entry.display(),
                out.display()
            );
            run_esbuild(&entry, &out, true)?;
        }
    }
    Ok(())
}
