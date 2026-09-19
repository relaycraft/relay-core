use anyhow::Result;
use clap::Parser;

pub mod args;
pub mod commands;
mod logging;
pub mod sse_client;
mod ui;
pub mod utils;

use args::{Cli, Commands};
use commands::lifecycle;
use relay_core_http::control::RelayConfig;

/// The proxy listen address a command should use when the user did not name one.
fn proxy_listen(config: &RelayConfig) -> String {
    format!("127.0.0.1:{}", config.proxy.port)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install default crypto provider
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let cli = Cli::parse();

    // `run` and `daemon` both host the engine, and the daemon decides where its logs go: the
    // terminal when it runs in the foreground, its own file when a full-screen UI owns the
    // terminal. Installing a second subscriber here would panic, so those commands skip this.
    let hosts_the_daemon = matches!(cli.command, Commands::Daemon { .. } | Commands::Run { .. });

    if !hosts_the_daemon {
        logging::init_stdout();
    }

    match cli.command {
        Commands::Config { action } => {
            commands::config::execute(action)?;
        }
        Commands::Start {
            listen,
            api_port,
            mcp_port,
            no_mcp,
            no_proxy,
            transparent,
            udp_tproxy_port,
            ca_cert,
            ca_key,
            in_memory,
            save_stream,
            idle_timeout,
            json,
        } => {
            let config = commands::config::load_or_fail()?;
            lifecycle::start(lifecycle::StartOptions {
                listen: listen.unwrap_or_else(|| proxy_listen(&config)),
                // Resolved here, once: the daemon a command spawns is told the effective ports
                // rather than re-deriving them, so flag, environment and config cannot disagree.
                api_port: Some(api_port.unwrap_or(config.daemon.api_port)),
                mcp_port,
                no_mcp,
                no_proxy,
                transparent,
                udp_tproxy_port,
                ca_cert,
                ca_key,
                in_memory,
                save_stream: save_stream.or_else(|| config.daemon.save_stream_path()),
                idle_timeout_secs: idle_timeout.unwrap_or(config.daemon.idle_timeout),
                json,
            })
            .await?;
        }
        Commands::Stop { json } => {
            lifecycle::stop(json).await?;
        }
        Commands::Restart {
            listen,
            transparent,
            udp_tproxy_port,
            ca_cert,
            ca_key,
            json,
        } => {
            let config = commands::config::load_or_fail()?;
            lifecycle::restart(lifecycle::StartOptions {
                listen: listen.unwrap_or_else(|| proxy_listen(&config)),
                transparent,
                udp_tproxy_port,
                ca_cert,
                ca_key,
                json,
                ..Default::default()
            })
            .await?;
        }
        Commands::Status { json } => {
            lifecycle::status(lifecycle::StatusOptions { json }).await?;
        }
        Commands::Shutdown { json } => {
            lifecycle::shutdown(json).await?;
        }
        Commands::Daemon {
            api_port,
            proxy_port,
            mcp_port,
            no_mcp,
            start_proxy,
            transparent,
            udp_tproxy_port,
            ca_cert,
            ca_key,
            in_memory,
            db_url,
            save_stream,
            idle_timeout,
        } => {
            let config = commands::config::load_or_fail()?;
            commands::daemon::run(commands::daemon::DaemonOptions {
                api_port: api_port.unwrap_or(config.daemon.api_port),
                api_bind: "127.0.0.1".to_string(),
                api_token: None,
                api_cors: None,
                serve_webui: false,
                proxy_port: proxy_port.unwrap_or(config.proxy.port),
                udp_tproxy_port,
                transparent,
                mcp_port: if no_mcp || !config.daemon.mcp {
                    None
                } else {
                    Some(mcp_port.unwrap_or(config.daemon.mcp_port))
                },
                start_proxy,
                ca_cert,
                ca_key,
                db_url,
                in_memory,
                log: commands::daemon::DaemonLog::Inherit,
                idle_timeout_secs: idle_timeout.unwrap_or(config.daemon.idle_timeout),
                save_stream: save_stream.or_else(|| config.daemon.save_stream_path()),
                rules: None,
                script: None,
                script_watch: false,
                script_env_allow: None,
                script_fetch_allow: None,
                upstream: None,
                upstream_auth_user: None,
                upstream_bypass: None,
                upstream_fail_open: false,
            })
            .await?;
        }
        Commands::Run {
            listen,
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
            #[cfg(feature = "script")]
            script_fetch_allow,
            ui,
            web,
            mcp_port,
            theme,
            transparent,
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
            commands::run::execute(commands::run::RunOptions {
                listen,
                ui,
                web,
                theme,
                rules,
                #[cfg(feature = "script")]
                script,
                #[cfg(feature = "script")]
                script_watch,
                #[cfg(feature = "script")]
                script_env_allow,
                #[cfg(feature = "script")]
                script_fetch_allow,
                #[cfg(not(feature = "script"))]
                script: None,
                #[cfg(not(feature = "script"))]
                script_watch: false,
                #[cfg(not(feature = "script"))]
                script_env_allow: None,
                #[cfg(not(feature = "script"))]
                script_fetch_allow: None,
                transparent,
                udp_tproxy_port,
                ca_cert,
                ca_key,
                api_port,
                api_bind,
                api_token,
                api_cors,
                mcp_port,
                save_stream,
                upstream,
                upstream_auth_user,
                upstream_bypass,
                upstream_fail_open,
            })
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
            api_url,
            follow,
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
                api_url,
                follow,
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
