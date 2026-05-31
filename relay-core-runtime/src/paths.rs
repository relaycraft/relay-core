use std::path::PathBuf;

pub const RELAY_DATA_DIR_ENV: &str = "RELAY_DATA_DIR";
pub const RELAY_CA_CERT_ENV: &str = "RELAY_CA_CERT";
pub const RELAY_CA_KEY_ENV: &str = "RELAY_CA_KEY";
pub const DEFAULT_DATA_DIR_NAME: &str = ".relay-core";
pub const DEFAULT_CA_CERT_FILE: &str = "ca_cert.pem";
pub const DEFAULT_CA_KEY_FILE: &str = "ca_key.pem";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl CaPaths {
    pub fn default_from_data_dir() -> Self {
        let data_dir = resolve_data_dir();
        Self {
            cert: data_dir.join(DEFAULT_CA_CERT_FILE),
            key: data_dir.join(DEFAULT_CA_KEY_FILE),
        }
    }

    /// Resolve CA paths with a single precedence chain:
    /// 1) CLI args, 2) environment variables, 3) config directory defaults.
    pub fn resolve(
        ca_cert_arg: Option<PathBuf>,
        ca_key_arg: Option<PathBuf>,
    ) -> Result<Self, String> {
        match (ca_cert_arg, ca_key_arg) {
            (Some(cert), Some(key)) => return Ok(Self { cert, key }),
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "CA path arguments must be provided as a pair: --ca-cert and --ca-key"
                        .to_string(),
                );
            }
            (None, None) => {}
        }

        let env_cert = std::env::var(RELAY_CA_CERT_ENV).ok().map(PathBuf::from);
        let env_key = std::env::var(RELAY_CA_KEY_ENV).ok().map(PathBuf::from);
        match (env_cert, env_key) {
            (Some(cert), Some(key)) => Ok(Self { cert, key }),
            (Some(_), None) | (None, Some(_)) => Err(format!(
                "Environment variables must be provided as a pair: {} and {}",
                RELAY_CA_CERT_ENV, RELAY_CA_KEY_ENV
            )),
            (None, None) => Ok(Self::default_from_data_dir()),
        }
    }
}

pub fn resolve_data_dir() -> PathBuf {
    std::env::var(RELAY_DATA_DIR_ENV)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(default_data_dir)
}

pub fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_DATA_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolve_prefers_args_over_env_and_default() {
        let _guard = env_lock().lock().expect("lock");
        unsafe {
            std::env::set_var(RELAY_DATA_DIR_ENV, "/tmp/relay-default");
            std::env::set_var(RELAY_CA_CERT_ENV, "/tmp/env-cert.pem");
            std::env::set_var(RELAY_CA_KEY_ENV, "/tmp/env-key.pem");
        }
        let resolved = CaPaths::resolve(
            Some(PathBuf::from("/tmp/arg-cert.pem")),
            Some(PathBuf::from("/tmp/arg-key.pem")),
        )
        .expect("resolve");
        assert_eq!(resolved.cert, PathBuf::from("/tmp/arg-cert.pem"));
        assert_eq!(resolved.key, PathBuf::from("/tmp/arg-key.pem"));
        unsafe {
            std::env::remove_var(RELAY_DATA_DIR_ENV);
            std::env::remove_var(RELAY_CA_CERT_ENV);
            std::env::remove_var(RELAY_CA_KEY_ENV);
        }
    }

    #[test]
    fn resolve_prefers_env_over_default_dir() {
        let _guard = env_lock().lock().expect("lock");
        unsafe {
            std::env::set_var(RELAY_DATA_DIR_ENV, "/tmp/relay-default");
            std::env::set_var(RELAY_CA_CERT_ENV, "/tmp/env-cert.pem");
            std::env::set_var(RELAY_CA_KEY_ENV, "/tmp/env-key.pem");
        }
        let resolved = CaPaths::resolve(None, None).expect("resolve");
        assert_eq!(resolved.cert, PathBuf::from("/tmp/env-cert.pem"));
        assert_eq!(resolved.key, PathBuf::from("/tmp/env-key.pem"));
        unsafe {
            std::env::remove_var(RELAY_DATA_DIR_ENV);
            std::env::remove_var(RELAY_CA_CERT_ENV);
            std::env::remove_var(RELAY_CA_KEY_ENV);
        }
    }

    #[test]
    fn resolve_falls_back_to_data_dir_defaults() {
        let _guard = env_lock().lock().expect("lock");
        unsafe {
            std::env::set_var(RELAY_DATA_DIR_ENV, "/tmp/relay-default");
            std::env::remove_var(RELAY_CA_CERT_ENV);
            std::env::remove_var(RELAY_CA_KEY_ENV);
        }
        let resolved = CaPaths::resolve(None, None).expect("resolve");
        assert_eq!(
            resolved.cert,
            PathBuf::from("/tmp/relay-default/ca_cert.pem")
        );
        assert_eq!(resolved.key, PathBuf::from("/tmp/relay-default/ca_key.pem"));
        unsafe {
            std::env::remove_var(RELAY_DATA_DIR_ENV);
        }
    }

    #[test]
    fn resolve_rejects_single_arg_override() {
        let _guard = env_lock().lock().expect("lock");
        let err = CaPaths::resolve(Some(PathBuf::from("/tmp/only-cert.pem")), None)
            .expect_err("should fail");
        assert!(err.contains("--ca-cert"));
    }

    #[test]
    fn resolve_rejects_single_env_override() {
        let _guard = env_lock().lock().expect("lock");
        unsafe {
            std::env::set_var(RELAY_CA_CERT_ENV, "/tmp/env-cert.pem");
            std::env::remove_var(RELAY_CA_KEY_ENV);
        }
        let err = CaPaths::resolve(None, None).expect_err("should fail");
        assert!(err.contains(RELAY_CA_CERT_ENV));
        unsafe {
            std::env::remove_var(RELAY_CA_CERT_ENV);
        }
    }
}
