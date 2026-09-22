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

/// Data directory for this process.
///
/// `RELAY_DATA_DIR` wins when it is non-empty. A relative value is anchored to
/// the working directory once, so the daemon and every later client agree on
/// one absolute path. Otherwise the directory is the per-user
/// [`default_data_dir`], never a `.relay-core` folder in whatever directory
/// the process was started from.
pub fn resolve_data_dir() -> PathBuf {
    if let Some(configured) = std::env::var_os(RELAY_DATA_DIR_ENV).filter(|value| !value.is_empty())
    {
        return make_absolute(PathBuf::from(configured));
    }
    default_data_dir()
}

/// `$HOME/.relay-core` (decision 0007): one directory per user, shared across
/// projects and working directories.
///
/// A missing, empty, or relative `HOME` is not a reason to fall back to the
/// process working directory. On Unix the account database supplies the home
/// directory instead. Launching the daemon from another folder must not create
/// a fresh `.relay-core` there.
pub fn default_data_dir() -> PathBuf {
    let home = user_home_dir().unwrap_or_else(|| {
        panic!(
            "could not find a home directory for {DEFAULT_DATA_DIR_NAME}; set {RELAY_DATA_DIR_ENV} to an absolute path"
        )
    });
    home.join(DEFAULT_DATA_DIR_NAME)
}

fn make_absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path,
    }
}

fn env_absolute(key: &str) -> Option<PathBuf> {
    let raw = std::env::var_os(key)?;
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

fn user_home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        return env_absolute("HOME").or_else(passwd_home);
    }
    #[cfg(windows)]
    {
        return env_absolute("USERPROFILE").or_else(|| env_absolute("HOME"));
    }
    #[cfg(not(any(unix, windows)))]
    {
        env_absolute("HOME")
    }
}

/// Home directory from the passwd database, ignoring `HOME`.
///
/// `HOME` is often unset or empty for a process started outside a login shell
/// (a service manager, `sudo` without preserving the environment, an IDE run
/// configuration). The working directory is not a substitute.
#[cfg(unix)]
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let mut capacity = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    if capacity < 1024 {
        capacity = 16 * 1024;
    }
    let mut buf = vec![0u8; capacity as usize];
    // SAFETY: `pwd` is only read after `getpwuid_r` succeeds, and `pw_dir`
    // points into `buf`, which outlives that read.
    let dir = unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() {
            return None;
        }
        let dir_ptr = (*result).pw_dir;
        if dir_ptr.is_null() {
            return None;
        }
        std::ffi::OsStr::from_bytes(std::ffi::CStr::from_ptr(dir_ptr).to_bytes()).to_os_string()
    };
    let path = PathBuf::from(dir);
    if path.as_os_str().is_empty() || !path.is_absolute() {
        None
    } else {
        Some(path)
    }
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

    /// Restores `HOME` and `RELAY_DATA_DIR` even when an assertion panics.
    struct HomeEnv {
        home: Option<std::ffi::OsString>,
        data_dir: Option<std::ffi::OsString>,
    }

    impl HomeEnv {
        fn capture() -> Self {
            Self {
                home: std::env::var_os("HOME"),
                data_dir: std::env::var_os(RELAY_DATA_DIR_ENV),
            }
        }

        fn set_home(&self, value: Option<&str>) {
            unsafe {
                match value {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
        }

        fn set_data_dir(&self, value: Option<&str>) {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(RELAY_DATA_DIR_ENV, value),
                    None => std::env::remove_var(RELAY_DATA_DIR_ENV),
                }
            }
        }
    }

    impl Drop for HomeEnv {
        fn drop(&mut self) {
            unsafe {
                match self.home.clone() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.data_dir.clone() {
                    Some(value) => std::env::set_var(RELAY_DATA_DIR_ENV, value),
                    None => std::env::remove_var(RELAY_DATA_DIR_ENV),
                }
            }
        }
    }

    #[test]
    fn default_data_dir_is_home_dot_relay_core() {
        let _guard = env_lock().lock().expect("lock");
        let env = HomeEnv::capture();
        env.set_data_dir(None);
        env.set_home(Some("/tmp/relay-home-fixed"));

        assert_eq!(
            default_data_dir(),
            PathBuf::from("/tmp/relay-home-fixed/.relay-core")
        );
        assert_eq!(
            resolve_data_dir(),
            PathBuf::from("/tmp/relay-home-fixed/.relay-core")
        );
    }

    #[test]
    fn empty_data_dir_env_falls_back_to_home() {
        let _guard = env_lock().lock().expect("lock");
        let env = HomeEnv::capture();
        env.set_home(Some("/tmp/relay-home-fixed"));
        env.set_data_dir(Some(""));

        assert_eq!(
            resolve_data_dir(),
            PathBuf::from("/tmp/relay-home-fixed/.relay-core")
        );
    }

    #[test]
    fn relative_data_dir_env_is_anchored_once() {
        let _guard = env_lock().lock().expect("lock");
        let env = HomeEnv::capture();
        env.set_data_dir(Some("custom-relay-data"));

        let resolved = resolve_data_dir();
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert_eq!(
            resolved,
            std::env::current_dir()
                .expect("cwd")
                .join("custom-relay-data")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unusable_home_does_not_follow_the_working_directory() {
        let _guard = env_lock().lock().expect("lock");
        let env = HomeEnv::capture();
        env.set_data_dir(None);
        let cwd_data_dir = std::env::current_dir()
            .expect("cwd")
            .join(DEFAULT_DATA_DIR_NAME);

        for home in [None, Some(""), Some("."), Some("relative/home")] {
            env.set_home(home);
            let resolved = resolve_data_dir();
            assert!(
                resolved.is_absolute(),
                "HOME={home:?} produced {}",
                resolved.display()
            );
            assert_eq!(
                resolved.file_name().and_then(|name| name.to_str()),
                Some(DEFAULT_DATA_DIR_NAME)
            );
            assert_ne!(
                resolved, cwd_data_dir,
                "HOME={home:?} followed the working directory"
            );
            assert_ne!(resolved, PathBuf::from("./.relay-core"));
            assert_ne!(resolved, PathBuf::from(".relay-core"));
        }
    }
}
