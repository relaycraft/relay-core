//! The RelayCore configuration file: the place settings live when nobody wants to remember flags.
//!
//! Decision [`0007`](../../../docs/decisions/0007-daemon-control-plane.md). Precedence is
//! **flag > environment > config file > built-in default**, so a one-off override never requires
//! editing the file, and the file is what makes a setting survive across commands.
//!
//! A malformed config is an error, not a fallback: silently ignoring a typo produces a daemon that
//! runs with settings the user did not ask for, which is the failure mode this project keeps
//! removing elsewhere.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Every setting, grouped by who reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    pub daemon: DaemonSection,
    pub proxy: ProxySection,
    pub client: ClientSection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonSection {
    /// Control API / Web UI port. Falls back to an ephemeral port when taken.
    pub api_port: u16,
    /// MCP endpoint port.
    pub mcp_port: u16,
    /// Serve the MCP endpoint from the daemon.
    pub mcp: bool,
    /// Stop the proxy after this many seconds without captured traffic; `0` never stops it.
    pub idle_timeout: u64,
    /// Write every flow update to this file as JSONL. Empty means nothing is written.
    pub save_stream: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySection {
    /// Port the proxy listens on when a command does not name one.
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientSection {
    /// Whether the MCP bridge may start a daemon when none is running.
    pub autostart_daemon: bool,
}

impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            api_port: crate::control::DEFAULT_API_PORT,
            mcp_port: crate::control::DEFAULT_MCP_PORT,
            mcp: true,
            idle_timeout: 0,
            save_stream: String::new(),
        }
    }
}

impl DaemonSection {
    /// The configured stream file, or `None` when the setting is empty.
    ///
    /// An empty string is how TOML says "unset" for a path, so it is normalized here instead of at
    /// every call site.
    pub fn save_stream_path(&self) -> Option<PathBuf> {
        let trimmed = self.save_stream.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }
}

impl Default for ProxySection {
    fn default() -> Self {
        Self {
            // One definition of the standard proxy port, in the crate that binds it.
            port: relay_core_runtime::services::DEFAULT_PROXY_PORT,
        }
    }
}

impl Default for ClientSection {
    fn default() -> Self {
        // Auto-start keeps "install the package and it works" true; the config file and
        // `RELAY_MCP_NO_AUTOSTART` are how someone turns it off on a shared machine.
        Self {
            autostart_daemon: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE_NAME)
}

/// Load the config, or defaults when the file does not exist.
///
/// Absence is normal (nobody has to write one); a file that exists and cannot be understood is an
/// error the caller must surface.
pub fn load(data_dir: &Path) -> Result<RelayConfig, ConfigError> {
    let path = config_path(data_dir);
    if !path.exists() {
        return Ok(RelayConfig::default());
    }

    let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;
    toml::from_str(&raw).map_err(|source| ConfigError::Parse { path, source })
}

/// Write a commented default, so the settings are discoverable without documentation.
///
/// Refuses to overwrite: a config file is user state, and silently replacing it would discard the
/// settings someone tuned.
pub fn write_default(data_dir: &Path, force: bool) -> Result<PathBuf, ConfigError> {
    let path = config_path(data_dir);
    if path.exists() && !force {
        return Err(ConfigError::Write {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "config already exists (pass --force to overwrite)",
            ),
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })?;
    }
    std::fs::write(&path, DEFAULT_CONFIG_TEMPLATE).map_err(|source| ConfigError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The file `relay config init` writes.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# RelayCore configuration.
#
# Every key is optional; an unset key uses the built-in default. Precedence is
# command-line flag > environment variable > this file > built-in default.

[daemon]
# Control API (and Web UI) port. Falls back to an ephemeral port when taken.
api_port = 8082
# MCP endpoint port served by the daemon.
mcp_port = 18083
# Serve the MCP endpoint from the daemon.
mcp = true
# Stop the proxy after this many seconds without captured traffic. 0 = never.
idle_timeout = 0
# Write every flow update to this file as JSONL (empty = do not write).
save_stream = ""

[proxy]
# Port the proxy listens on when a command does not name one.
port = 8080

[client]
# Whether the MCP bridge may start a daemon when none is running.
autostart_daemon = true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_yields_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = load(dir.path()).expect("defaults");
        assert_eq!(config, RelayConfig::default());
        assert!(config.daemon.mcp, "MCP is on by default");
        assert!(
            config.client.autostart_daemon,
            "the bridge auto-starts by default"
        );
        assert_eq!(
            config.daemon.idle_timeout, 0,
            "the proxy is never stopped automatically unless asked"
        );
    }

    #[test]
    fn a_partial_file_only_overrides_what_it_names() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(config_path(dir.path()), "[daemon]\nidle_timeout = 900\n")
            .expect("write config");

        let config = load(dir.path()).expect("config");
        assert_eq!(config.daemon.idle_timeout, 900);
        assert_eq!(config.daemon.api_port, DaemonSection::default().api_port);
        assert_eq!(config.proxy, ProxySection::default());
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_fallback() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            config_path(dir.path()),
            "[daemon]\napi_port = \"not a port\"\n",
        )
        .expect("write config");

        let error = load(dir.path()).expect_err("a typo must be reported");
        assert!(
            matches!(error, ConfigError::Parse { .. }),
            "expected a parse error, got {error:?}"
        );
    }

    #[test]
    fn the_default_template_round_trips() {
        let parsed: RelayConfig =
            toml::from_str(DEFAULT_CONFIG_TEMPLATE).expect("the shipped template must be valid");
        assert_eq!(
            parsed,
            RelayConfig::default(),
            "the template documents the defaults, so it must parse to them"
        );
    }

    #[test]
    fn an_empty_save_stream_setting_means_do_not_write() {
        let mut section = DaemonSection::default();
        assert_eq!(section.save_stream_path(), None);

        section.save_stream = "   ".to_string();
        assert_eq!(section.save_stream_path(), None, "whitespace is not a path");

        section.save_stream = "/tmp/flows.jsonl".to_string();
        assert_eq!(
            section.save_stream_path(),
            Some(PathBuf::from("/tmp/flows.jsonl"))
        );
    }

    #[test]
    fn writing_the_default_refuses_to_clobber_existing_settings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_default(dir.path(), false).expect("first write");
        assert!(path.exists());

        std::fs::write(&path, "[daemon]\nidle_timeout = 60\n").expect("user edit");

        assert!(
            write_default(dir.path(), false).is_err(),
            "an existing config must not be overwritten by accident"
        );
        assert_eq!(
            load(dir.path()).expect("config").daemon.idle_timeout,
            60,
            "the user's settings survived"
        );

        write_default(dir.path(), true).expect("--force overwrites");
        assert_eq!(
            load(dir.path()).expect("config").daemon.idle_timeout,
            0,
            "--force restores the template"
        );
    }
}
