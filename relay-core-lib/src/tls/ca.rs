use rcgen::{
    Certificate, CertificateParams, DnType, Ia5String, IsCa, KeyPair, KeyUsagePurpose, SanType,
    SerialNumber,
};

use moka::future::Cache;
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};
use tracing::warn;

#[derive(Serialize, Deserialize)]
struct CaMetadata {
    serial_number: u64,
    not_before_unix: i64,
    not_after_unix: i64,
}

#[derive(Clone)]
pub struct CertificateAuthority {
    ca_cert: Arc<Certificate>,
    ca_key_pair: Arc<KeyPair>,
    // Cache: domain -> Arc<ServerConfig>
    pub(crate) cache: Cache<String, Arc<ServerConfig>>,
}

/// Make an existing private key owner-only.
///
/// A CA key is worth as much as the certificates it can sign, and one created before this existed is
/// still on disk at whatever mode the process umask gave it. Loading is when that gets noticed, so it
/// is where it gets fixed.
#[cfg(unix)]
fn tighten_key_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)?.permissions();
    if perms.mode() & 0o077 == 0 {
        return Ok(()); // already owner-only
    }
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn tighten_key_permissions(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Write a private key so that only its owner can read it.
///
/// The CA key can mint a certificate for any host, so the process default (commonly `0644`, world
/// readable) leaves it exposed to every other account on the machine. The file is *created* with the
/// mode rather than written and then corrected, so there is no window in which it is readable by
/// others; the explicit `set_permissions` afterwards covers a key file that already exists from an
/// earlier version and would otherwise keep its old mode.
#[cfg(unix)]
fn write_private_key(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;

    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn write_private_key(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    // Windows has no equivalent mode bit here; access is governed by ACLs on the containing directory.
    std::fs::write(path, contents)
}

impl CertificateAuthority {
    // Keep cache TTL strictly lower than leaf validity to avoid serving stale configs.
    const LEAF_CERT_VALIDITY_DAYS: i64 = 365;
    const CACHE_TTL_SECS: u64 = 60 * 60 * 24 * 180; // 180 days

    fn create_ca_params(
        key_pair: &KeyPair,
        meta: &CaMetadata,
    ) -> crate::error::Result<Certificate> {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "RelayCraft CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "RelayCraft");
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

        // Restore validity
        let not_before = OffsetDateTime::from_unix_timestamp(meta.not_before_unix)?;
        let not_after = OffsetDateTime::from_unix_timestamp(meta.not_after_unix)?;
        params.not_before = not_before;
        params.not_after = not_after;

        // Restore serial number
        params.serial_number = Some(SerialNumber::from(meta.serial_number));

        // Create certificate
        let cert = params.self_signed(key_pair)?;
        Ok(cert)
    }

    fn build_cert_cache() -> Cache<String, Arc<ServerConfig>> {
        Cache::builder()
            .max_capacity(1_000)
            .time_to_live(std::time::Duration::from_secs(Self::CACHE_TTL_SECS))
            .build()
    }

    fn validate_ca_certificate(cert_pem: &str, ca_cert_path: &Path) -> crate::error::Result<()> {
        use x509_parser::prelude::{FromDer, X509Certificate};

        let pem = x509_parser::pem::Pem::iter_from_buffer(cert_pem.as_bytes())
            .next()
            .ok_or_else(|| {
                crate::error::RelayError::Config(format!(
                    "Failed to parse CA PEM at {:?}: empty content",
                    ca_cert_path
                ))
            })?
            .map_err(|e| {
                crate::error::RelayError::Config(format!(
                    "Failed to parse CA PEM at {:?}: {}",
                    ca_cert_path, e
                ))
            })?;

        let (_, x509) = X509Certificate::from_der(&pem.contents).map_err(|e| {
            crate::error::RelayError::Config(format!(
                "Failed to parse CA X509 at {:?}: {}",
                ca_cert_path, e
            ))
        })?;

        let basic_constraints = x509.basic_constraints().map_err(|e| {
            crate::error::RelayError::Config(format!(
                "Failed to read BasicConstraints for {:?}: {}",
                ca_cert_path, e
            ))
        })?;
        let is_ca = basic_constraints.is_some_and(|bc| bc.value.ca);
        if !is_ca {
            return Err(crate::error::RelayError::Config(format!(
                "Certificate at {:?} is not a CA certificate (BasicConstraints CA=true is required)",
                ca_cert_path
            )));
        }

        let key_usage = x509.key_usage().map_err(|e| {
            crate::error::RelayError::Config(format!(
                "Failed to read KeyUsage for {:?}: {}",
                ca_cert_path, e
            ))
        })?;
        let key_usage = key_usage.ok_or_else(|| {
            crate::error::RelayError::Config(format!(
                "Certificate at {:?} is missing KeyUsage extension (requires keyCertSign and cRLSign)",
                ca_cert_path
            ))
        })?;
        if !key_usage.value.key_cert_sign() || !key_usage.value.crl_sign() {
            return Err(crate::error::RelayError::Config(format!(
                "Certificate at {:?} must include KeyUsage keyCertSign and cRLSign",
                ca_cert_path
            )));
        }

        Ok(())
    }

    fn load_from_persistent_files(
        ca_key_path: &Path,
        meta_path: &Path,
    ) -> crate::error::Result<Self> {
        let key_pem = std::fs::read_to_string(ca_key_path)?;

        // Parse key
        let key_pair = KeyPair::from_pem(&key_pem)?;

        // Load metadata produced by relay-core and reconstruct deterministic CA cert params.
        // If metadata is invalid, fail fast instead of silently changing behavior.
        let meta_json = std::fs::read_to_string(meta_path)?;
        let meta = match serde_json::from_str::<CaMetadata>(&meta_json) {
            Ok(meta) => meta,
            Err(e) => {
                warn!(
                    meta_path = ?meta_path,
                    error = %e,
                    "Failed to parse CA metadata JSON"
                );
                return Err(crate::error::RelayError::Config(format!(
                    "Failed to parse CA metadata at {:?}: {}",
                    meta_path, e
                )));
            }
        };
        let cert = Self::create_ca_params(&key_pair, &meta)?;

        Ok(Self {
            ca_cert: Arc::new(cert),
            ca_key_pair: Arc::new(key_pair),
            cache: Self::build_cert_cache(),
        })
    }

    /// Create an in-memory CA that is not written to disk.
    pub fn new_ephemeral() -> crate::error::Result<Self> {
        Self::create_fresh_ca()
    }

    pub fn new() -> crate::error::Result<Self> {
        Self::new_ephemeral()
    }

    fn create_fresh_ca() -> crate::error::Result<Self> {
        // Generate new metadata
        let now = OffsetDateTime::now_utc();
        let not_after = now + Duration::days(365 * 10);
        // Use nanoseconds as serial number (good enough for local CA)
        let serial = (now.unix_timestamp_nanos() & 0xFFFFFFFFFFFFFFFF) as u64;

        let meta = CaMetadata {
            serial_number: serial,
            not_before_unix: now.unix_timestamp(),
            not_after_unix: not_after.unix_timestamp(),
        };

        let key_pair = KeyPair::generate()?;
        let cert = Self::create_ca_params(&key_pair, &meta)?;

        Ok(Self {
            ca_cert: Arc::new(cert),
            ca_key_pair: Arc::new(key_pair),
            cache: Self::build_cert_cache(),
        })
    }

    /// Load existing CA from files, or create and persist a new one when files are missing.
    pub fn load_or_create_persistent(
        ca_cert_path: &Path,
        ca_key_path: &Path,
    ) -> crate::error::Result<Self> {
        Self::load_or_create(ca_cert_path, ca_key_path)
    }

    /// Load existing CA from files or create new one if not exists
    pub fn load_or_create(ca_cert_path: &Path, ca_key_path: &Path) -> crate::error::Result<Self> {
        let meta_path = ca_cert_path.with_extension("json");

        if ca_cert_path.exists() && ca_key_path.exists() {
            if !meta_path.exists() {
                return Err(crate::error::RelayError::Config(format!(
                    "CA metadata file is missing: {meta_path:?}. This relay-core version requires metadata to load persistent CA. Run `{cmd} ca generate --force` to regenerate.",
                    cmd = relay_core_api::CLI_COMMAND,
                )));
            }

            if let Err(err) =
                Self::validate_ca_certificate(&std::fs::read_to_string(ca_cert_path)?, ca_cert_path)
            {
                warn!(
                    ca_cert_path = ?ca_cert_path,
                    error = %err,
                    "CA certificate validation failed before loading metadata"
                );
                return Err(err);
            }

            // Tighten a key that an earlier version created with the process umask. Loading is the
            // only moment this code sees such a file, and a key that stays world-readable is the
            // whole problem this guards against.
            tighten_key_permissions(ca_key_path)?;

            return Self::load_from_persistent_files(ca_key_path, &meta_path);
        }

        // Create new
        let now = OffsetDateTime::now_utc();
        let not_after = now + Duration::days(365 * 10);
        let serial = (now.unix_timestamp_nanos() & 0xFFFFFFFFFFFFFFFF) as u64;

        let meta = CaMetadata {
            serial_number: serial,
            not_before_unix: now.unix_timestamp(),
            not_after_unix: not_after.unix_timestamp(),
        };

        let key_pair = KeyPair::generate()?;
        let cert = Self::create_ca_params(&key_pair, &meta)?;

        // Save to disk (PEM for general use, DER .cer for Windows native import)
        std::fs::write(ca_cert_path, cert.pem())?;
        let cer_path = ca_cert_path.with_extension("cer");
        std::fs::write(&cer_path, cert.der())?;
        write_private_key(ca_key_path, &key_pair.serialize_pem())?;
        std::fs::write(meta_path, serde_json::to_string_pretty(&meta)?)?;

        Ok(Self {
            ca_cert: Arc::new(cert),
            ca_key_pair: Arc::new(key_pair),
            cache: Self::build_cert_cache(),
        })
    }

    pub async fn gen_server_config(&self, domain: &str) -> crate::error::Result<Arc<ServerConfig>> {
        let domain = domain.to_string();
        let domain_for_error = domain.clone();
        let ca_cert = self.ca_cert.clone();
        let ca_key_pair = self.ca_key_pair.clone();

        self.cache
            .try_get_with(domain.clone(), async move {
                let key_pair = KeyPair::generate().map_err(|e| {
                    std::io::Error::other(format!(
                        "failed to generate leaf key pair for domain {}: {}",
                        domain, e
                    ))
                })?;

                let mut params = CertificateParams::default();
                params.distinguished_name.push(DnType::CommonName, &domain);
                params.subject_alt_names = vec![SanType::DnsName(
                    Ia5String::try_from(domain.as_str()).map_err(|e| {
                        std::io::Error::other(format!(
                            "failed to encode SAN DNS name for domain {}: {}",
                            domain, e
                        ))
                    })?,
                )];
                params.not_before = OffsetDateTime::now_utc();
                params.not_after =
                    OffsetDateTime::now_utc() + Duration::days(Self::LEAF_CERT_VALIDITY_DAYS);

                let cert = params
                    .signed_by(&key_pair, &ca_cert, &ca_key_pair)
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "failed to sign leaf certificate for domain {}: {}",
                            domain, e
                        ))
                    })?;

                let mut server_config = ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(
                        vec![cert.der().clone()],
                        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())),
                    )
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "failed to build rustls server config for domain {}: {}",
                            domain, e
                        ))
                    })?;

                server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

                Ok(Arc::new(server_config)) as Result<Arc<ServerConfig>, std::io::Error>
            })
            .await
            .map_err(|e| {
                crate::error::RelayError::Proxy(format!(
                    "failed to get/generate cached TLS config for domain {}: {}",
                    domain_for_error, e
                ))
            })
    }

    pub fn get_ca_cert_pem(&self) -> String {
        self.ca_cert.pem()
    }

    pub fn get_ca_cert_der(&self) -> Vec<u8> {
        self.ca_cert.der().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Once;
    use tempfile::tempdir;

    static INIT: Once = Once::new();

    fn init_crypto() {
        INIT.call_once(|| {
            // install_default() returns Err if another test thread already installed the
            // provider — that is fine, the provider is already available.
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn test_ca_persistence() {
        init_crypto();
        let dir = tempdir().unwrap();
        let ca_cert_path = dir.path().join("ca.crt");
        let ca_key_path = dir.path().join("ca.key");
        let meta_path = dir.path().join("ca.json");

        // 1. Create new CA
        let ca1 = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();
        let _pem1 = ca1.get_ca_cert_pem();

        assert!(ca_cert_path.exists());
        assert!(ca_key_path.exists());
        assert!(meta_path.exists());

        // 2. Load existing CA
        let ca2 = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();
        let _pem2 = ca2.get_ca_cert_pem();

        // PEMs might differ slightly due to encoding, but let's check if they are functionally equivalent
        // Or check if serial matches if we could access it.
        // For now, let's check if metadata serial matches.

        let _meta1: CaMetadata =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();

        // In the second load, we used the metadata to reconstruct parameters.
        // So the generated certificate should be very similar.

        // Ideally, we should check if ca2's cert has the same serial number as meta1.serial_number.
        // rcgen doesn't expose serial number getter easily on Certificate.
        // But we can parse the PEM with x509-parser if we wanted to be sure.
        // For now, let's just assert that load_or_create didn't fail and didn't overwrite files (check mtime?)

        let mtime1 = fs::metadata(&ca_cert_path).unwrap().modified().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let _ca3 = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();
        let mtime2 = fs::metadata(&ca_cert_path).unwrap().modified().unwrap();

        assert_eq!(mtime1, mtime2, "File should not be modified if it exists");

        // 3. Corrupt metadata and check if it recovers (optional, currently it would fail)
    }

    #[tokio::test]
    async fn test_concurrent_cert_generation() {
        init_crypto();
        let ca = CertificateAuthority::new().unwrap();
        let domain = "example.com";

        let mut handles = vec![];
        for _ in 0..20 {
            let ca = ca.clone();
            handles.push(tokio::spawn(async move {
                ca.gen_server_config(domain).await.unwrap()
            }));
        }

        let mut configs = vec![];
        for handle in handles {
            configs.push(handle.await.unwrap());
        }

        // Check that all configs are the same instance (Arc pointer equality)
        let first = &configs[0];
        for config in &configs[1..] {
            assert!(
                Arc::ptr_eq(first, config),
                "All concurrent requests should return the same Arc<ServerConfig>"
            );
        }
    }

    #[tokio::test]
    async fn test_cert_expiration_and_regeneration() {
        init_crypto();
        let ca = CertificateAuthority::new().unwrap();
        let domain = "example.org";

        // 1. Generate initial config
        let config1 = ca.gen_server_config(domain).await.unwrap();

        // 2. Invalidate cache entry manually (simulating TTL expiry)
        ca.cache.invalidate(domain).await;

        // 3. Generate again
        let config2 = ca.gen_server_config(domain).await.unwrap();

        // 4. Verify that we got a new instance (regeneration happened)
        assert!(
            !Arc::ptr_eq(&config1, &config2),
            "Expired entry should trigger new generation"
        );
    }

    #[tokio::test]
    async fn test_load_without_metadata_fails() {
        init_crypto();
        // 1. Create a temp directory
        let temp_dir = tempfile::tempdir().unwrap();
        let cert_path = temp_dir.path().join("test_ca.crt");
        let key_path = temp_dir.path().join("test_ca.key");

        // 2. Generate a standard CA certificate using rcgen directly (simulating external tool)
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "External CA Test");
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationName, "External Org");
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        // 3. Save as PEM without metadata file
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

        // 4. Strict mode: persistent load requires metadata and should fail.
        let err = match CertificateAuthority::load_or_create(&cert_path, &key_path) {
            Ok(_) => panic!("expected missing metadata to fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("metadata file is missing"),
            "error should clearly explain missing metadata, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_load_ca_without_signing_keyusage_fails() {
        init_crypto();
        let temp_dir = tempfile::tempdir().unwrap();
        let cert_path = temp_dir.path().join("test_ca.crt");
        let key_path = temp_dir.path().join("test_ca.key");
        let meta_path = cert_path.with_extension("json");

        // Create CA cert with invalid KeyUsage for a signing CA.
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "InvalidKeyUsage CA");
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();
        // Keep metadata present so the failure is strictly from KeyUsage validation.
        std::fs::write(
            &meta_path,
            serde_json::to_string_pretty(&CaMetadata {
                serial_number: 1,
                not_before_unix: OffsetDateTime::now_utc().unix_timestamp(),
                not_after_unix: (OffsetDateTime::now_utc() + Duration::days(1)).unix_timestamp(),
            })
            .unwrap(),
        )
        .unwrap();

        let err = match CertificateAuthority::load_or_create(&cert_path, &key_path) {
            Ok(_) => panic!("expected KeyUsage validation to fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("keyCertSign and cRLSign"),
            "error should point to KeyUsage requirement, got: {}",
            err
        );
    }

    /// The CA private key must not be readable by anyone but its owner.
    ///
    /// It is the key that can impersonate any host, and it was created with the process umask, which
    /// commonly means `0644` — readable by every other account on the machine.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_ca_private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("relay-core-ca-perms-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("ca.key");
        let cert_path = dir.join("ca.pem");

        let _ca = CertificateAuthority::load_or_create(&cert_path, &key_path).unwrap();

        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the CA key must be owner-only, found {mode:o}");

        // And a key left behind by an older version is tightened rather than left as it was.
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ca = CertificateAuthority::load_or_create(&cert_path, &key_path).unwrap();
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "an existing key must not stay world-readable, found {mode:o}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
