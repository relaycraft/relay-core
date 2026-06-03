use anyhow::Result;
use clap::Parser;

pub mod args;
pub mod commands;
pub mod server;
pub mod sse_client;
mod logging;
mod ui;
pub mod utils;

use args::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install default crypto provider
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let cli = Cli::parse();

    // Determine if TUI is enabled
    let is_tui = if let Commands::Run { ui, .. } = &cli.command {
        *ui
    } else {
        false
    };

    let _log_guard = if is_tui {
        // In TUI mode, log to a file "relay-core.log"
        // We use a file writer to avoid polluting stdout
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("relay-core.log")?;

        Some(logging::init_file(file))
    } else {
        logging::init_stdout();
        None
    };

    match cli.command {
        Commands::Run {
            listen,
            control_port,
            udp_tproxy_port,
            ca_cert,
            ca_key,
            rules,
            #[cfg(feature = "script")]
            script,
            #[cfg(feature = "script")]
            script_watch,
            #[cfg(feature = "script")]
            script_env_allow,
            ui,
            theme,
            transparent,
            output,
            save_stream,
            api_port,
            api_bind,
            api_token,
            api_cors,
            upstream,
            upstream_auth_user,
            upstream_bypass,
            upstream_fail_open,
        } => {
            #[cfg(feature = "script")]
            commands::run::execute(
                listen,
                control_port,
                udp_tproxy_port,
                ca_cert,
                ca_key,
                rules,
                script,
                script_watch,
                script_env_allow,
                ui,
                theme,
                transparent,
                output,
                save_stream,
                api_port,
                api_bind,
                api_token,
                api_cors,
                upstream,
                upstream_auth_user,
                upstream_bypass,
                upstream_fail_open,
            )
            .await?;
            #[cfg(not(feature = "script"))]
            commands::run::execute(
                listen,
                control_port,
                udp_tproxy_port,
                ca_cert,
                ca_key,
                rules,
                ui,
                theme,
                transparent,
                output,
                save_stream,
                api_port,
                api_bind,
                api_token,
                api_cors,
                upstream,
                upstream_auth_user,
                upstream_bypass,
                upstream_fail_open,
            )
            .await?;
        }
        #[cfg(any(feature = "transparent-linux", feature = "transparent-macos"))]
        Commands::Proxy { action } => {
            if let Err(e) = commands::proxy::handle_transparent_command(action) {
                eprintln!("Proxy command failed: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Rules { action } => {
            commands::rules::execute(action)?;
        }
        #[cfg(feature = "script")]
        Commands::Scripts { action } => {
            commands::scripts::execute(action).await?;
        }
        Commands::Ca { action } => {
            commands::ca::execute(action)?;
        }
        Commands::Flows {
            control_url,
            api_url,
            output,
            filter,
            host,
            path,
            method,
            status_min,
            status_max,
            has_error,
            websocket,
            limit,
        } => {
            commands::flows::execute(commands::flows::FlowsOptions {
                control_url,
                api_url,
                output,
                filter,
                host,
                path,
                method,
                status_min,
                status_max,
                has_error,
                websocket,
                limit,
            })
            .await?;
        }
        Commands::Intercept {
            action,
            control_url,
        } => {
            commands::flows::execute_intercept(action, control_url).await?;
        }
        Commands::Metrics { proxy_url, output } => {
            commands::metrics::execute(proxy_url, output).await?;
        }
        Commands::Analyze {
            file,
            format,
            output,
            top_n,
        } => {
            commands::analyze::execute(commands::analyze::AnalyzeOptions {
                file,
                format,
                output,
                top_n,
            })?;
        }
    }

    Ok(())
}
