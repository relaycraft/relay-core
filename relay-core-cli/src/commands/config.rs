//! `relay config` — read, show and seed the configuration file.
//!
//! The file is what makes a setting survive across commands; the commands are how you find it
//! without reading documentation. See [`relay_core_http::control::config`].

use anyhow::{Context, Result};
use relay_core_http::control::config::{
    CONFIG_FILE_NAME, RelayConfig, config_path, load, write_default,
};
use relay_core_runtime::paths;

#[derive(Debug, Clone, clap::Subcommand)]
pub enum ConfigAction {
    /// Print the path of the configuration file
    Path,
    /// Print the configuration in effect (file values merged over the defaults)
    Show {
        /// Output format: toml (default) or json
        #[arg(long, default_value = "toml")]
        format: String,
    },
    /// Write a commented default configuration file
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
}

pub fn execute(action: ConfigAction) -> Result<()> {
    let data_dir = paths::resolve_data_dir();

    match action {
        ConfigAction::Path => {
            println!("{}", config_path(&data_dir).display());
            Ok(())
        }
        ConfigAction::Show { format } => {
            let config = load(&data_dir).with_context(|| {
                format!(
                    "reading {} (fix it, or remove it to fall back to the defaults)",
                    config_path(&data_dir).display()
                )
            })?;
            // The effective config, not the file: a reader should not have to know which keys the
            // file omitted.
            let rendered = match format.as_str() {
                "json" => serde_json::to_string_pretty(&config)?,
                "toml" => toml::to_string_pretty(&config)
                    .context("rendering the configuration as TOML")?,
                other => anyhow::bail!("unknown --format {other:?} (expected toml or json)"),
            };
            print!("{rendered}");
            Ok(())
        }
        ConfigAction::Init { force } => {
            let path = write_default(&data_dir, force)?;
            println!("Wrote {}", path.display());
            println!("Edit it to change ports, the idle timeout, or MCP behaviour.");
            Ok(())
        }
    }
}

/// Load the config for a command that is about to act, reporting a broken file loudly.
///
/// Every caller that resolves settings goes through here so that a typo cannot be mistaken for
/// "the user did not configure anything".
pub fn load_or_fail() -> Result<RelayConfig> {
    let data_dir = paths::resolve_data_dir();
    load(&data_dir).with_context(|| {
        format!(
            "reading {} (fix it, or remove it to fall back to the defaults)",
            config_path(&data_dir).display()
        )
    })
}

/// Where the config file lives, for messages that point users at it.
pub fn described_path() -> String {
    config_path(&paths::resolve_data_dir())
        .display()
        .to_string()
}

/// File name, re-exported so tests can assert on it.
pub const FILE_NAME: &str = CONFIG_FILE_NAME;
