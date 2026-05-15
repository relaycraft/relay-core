use clap::Parser;
use anyhow::Result;

pub mod args;
pub mod commands;
mod ui;
pub mod server;
pub mod utils;

use args::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install default crypto provider
    rustls::crypto::ring::default_provider().install_default()
        .expect("Failed to install rustls crypto provider");

    let cli = Cli::parse();

    // Determine if TUI is enabled
    let is_tui = if let Commands::Run { ui, .. } = &cli.command {
        *ui
    } else {
        false
    };

    if is_tui {
        // In TUI mode, log to a file "relay-core.log"
        // We use a file writer to avoid polluting stdout
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("relay-core.log")?;
            
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    } else {
        // Normal mode: log to stdout
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
            .init();
    }
    
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
            ui,
            transparent,
            output,
            save_stream,
            api_port,
            api_bind,
            api_token,
            api_cors,
        } => {
             #[cfg(feature = "script")]
             commands::run::execute(listen, control_port, udp_tproxy_port, ca_cert, ca_key, rules, script, script_watch, ui, transparent, output, save_stream, api_port, api_bind, api_token, api_cors).await?;
             #[cfg(not(feature = "script"))]
             commands::run::execute(listen, control_port, udp_tproxy_port, ca_cert, ca_key, rules, ui, transparent, output, save_stream, api_port, api_bind, api_token, api_cors).await?;
        },
        #[cfg(any(feature = "transparent-linux", feature = "transparent-macos"))]
        Commands::Proxy { action } => {
            if let Err(e) = commands::proxy::handle_transparent_command(action) {
                eprintln!("Proxy command failed: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Rules { action } => {
             commands::rules::execute(action)?;
        },
        #[cfg(feature = "script")]
        Commands::Scripts { action } => {
            commands::scripts::execute(action).await?;
        },
        Commands::Ca { action } => {
             commands::ca::execute(action)?;
        },
        Commands::Flows { control_url, output } => {
            commands::flows::execute(control_url, output).await?;
        },
        Commands::Intercept { action, control_url } => {
            commands::flows::execute_intercept(action, control_url).await?;
        },
        Commands::Metrics { proxy_url, output } => {
            commands::metrics::execute(proxy_url, output).await?;
        }
    }

    Ok(())
}
