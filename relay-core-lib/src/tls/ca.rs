use rcgen::{
    Certificate, CertificateParams, DnType, Ia5String, IsCa, KeyPair, SanType, SerialNumber,
};

use moka::future::Cache;
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

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

impl CertificateAuthority {
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
            .time_to_live(std::time::Duration::from_secs(60 * 60 * 24 * 180)) // 180 days
            .build()
    }

    fn load_from_pem(ca_cert_path: &Path, ca_key_path: &Path) -> crate::error::Result<Self> {
        let cert_pem = std::fs::read_to_string(ca_cert_path)?;
        let key_pem = std::fs::read_to_string(ca_key_path)?;

        // Parse Key
        let key_pair = KeyPair::from_pem(&key_pem)?;

        // Parse Cert
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).map_err(|e| {
            crate::error::RelayError::Config(format!("Failed to parse CA PEM: {}", e))
        })?;
        let x509 = pem.parse_x509().map_err(|e| {
            crate::error::RelayError::Config(format!("Failed to parse CA X509: {}", e))
        })?;

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);

        // Copy Validity
        params.not_before =
            OffsetDateTime::from_unix_timestamp(x509.validity().not_before.timestamp())?;
        params.not_after =
            OffsetDateTime::from_unix_timestamp(x509.validity().not_after.timestamp())?;

        // Copy Serial Number
        let serial = &x509.tbs_certificate.serial;
        params.serial_number = Some(SerialNumber::from(serial.to_bytes_be()));

        // Copy Subject DN
        for rdn in x509.subject().iter() {
            for attr in rdn.iter() {
                let oid = attr.attr_type();
                let content = attr.attr_value().as_str().unwrap_or_default();

                let dn_type = if oid == &x509_parser::oid_registry::OID_X509_COMMON_NAME {
                    Some(DnType::CommonName)
                } else if oid == &x509_parser::oid_registry::OID_X509_ORGANIZATION_NAME {
                    Some(DnType::OrganizationName)
                } else if oid == &x509_parser::oid_registry::OID_X509_ORGANIZATIONAL_UNIT {
                    Some(DnType::OrganizationalUnitName)
                } else if oid == &x509_parser::oid_registry::OID_X509_COUNTRY_NAME {
                    Some(DnType::CountryName)
                } else if oid == &x509_parser::oid_registry::OID_X509_LOCALITY_NAME {
                    Some(DnType::LocalityName)
                } else if oid == &x509_parser::oid_registry::OID_X509_STATE_OR_PROVINCE_NAME {
                    Some(DnType::StateOrProvinceName)
                } else {
                    None
                };

                if let Some(t) = dn_type {
                    params.distinguished_name.push(t, content);
                }
            }
        }

        let cert = params.self_signed(&key_pair)?;

        Ok(Self {
            ca_cert: Arc::new(cert),
            ca_key_pair: Arc::new(key_pair),
            cache: Self::build_cert_cache(),
        })
    }

    pub fn new() -> crate::error::Result<Self> {
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

    /// Load existing CA from files or create new one if not exists
    pub fn load_or_create(ca_cert_path: &Path, ca_key_path: &Path) -> crate::error::Result<Self> {
        let meta_path = ca_cert_path.with_extension("json");

        if ca_cert_path.exists() && ca_key_path.exists() {
            // Try loading from JSON first (legacy/native mode)
            if meta_path.exists() {
                let key_pem = std::fs::read_to_string(ca_key_path)?;
                let key_pair = KeyPair::from_pem(&key_pem)?;

                let meta_json = std::fs::read_to_string(&meta_path)?;
                match serde_json::from_str::<CaMetadata>(&meta_json) {
                    Ok(meta) => {
                        let cert = Self::create_ca_params(&key_pair, &meta)?;
                        return Ok(Self {
                            ca_cert: Arc::new(cert),
                            ca_key_pair: Arc::new(key_pair),
                            cache: Self::build_cert_cache(),
                        });
                    }
                    Err(_) => {
                        // JSON might be corrupted or incompatible, fallback to PEM parsing
                    }
                }
            }

            // Fallback: Try loading directly from PEM (import mode)
            match Self::load_from_pem(ca_cert_path, ca_key_path) {
                Ok(ca) => return Ok(ca),
                Err(e) => {
                    // If both fail, and we have partial files, return error
                    return Err(crate::error::RelayError::Config(format!(
                        "Failed to load CA from {:?}: {}. If you want to regenerate, please remove existing files manually.",
                        ca_cert_path, e
                    )));
                }
            }
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

        // Save to disk
        std::fs::write(ca_cert_path, cert.pem())?;
        std::fs::write(ca_key_path, key_pair.serialize_pem())?;
        std::fs::write(meta_path, serde_json::to_string_pretty(&meta)?)?;

        Ok(Self {
            ca_cert: Arc::new(cert),
            ca_key_pair: Arc::new(key_pair),
            cache: Self::build_cert_cache(),
        })
    }

    pub async fn gen_server_config(&self, domain: &str) -> crate::error::Result<Arc<ServerConfig>> {
        let domain = domain.to_string();
        let ca_cert = self.ca_cert.clone();
        let ca_key_pair = self.ca_key_pair.clone();

        self.cache
            .try_get_with(domain.clone(), async move {
                let key_pair = KeyPair::generate().map_err(std::io::Error::other)?;

                let mut params = CertificateParams::default();
                params.distinguished_name.push(DnType::CommonName, &domain);
                params.subject_alt_names = vec![SanType::DnsName(
                    Ia5String::try_from(domain.as_str()).map_err(std::io::Error::other)?,
                )];
                params.not_before = OffsetDateTime::now_utc();
                params.not_after = OffsetDateTime::now_utc() + Duration::days(365);

                let cert = params
                    .signed_by(&key_pair, &ca_cert, &ca_key_pair)
                    .map_err(std::io::Error::other)?;

                let mut server_config = ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(
                        vec![cert.der().clone()],
                        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())),
                    )
                    .map_err(std::io::Error::other)?;

                server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

                Ok(Arc::new(server_config)) as Result<Arc<ServerConfig>, std::io::Error>
            })
            .await
            .map_err(|e| crate::error::RelayError::Proxy(e.to_string()))
    }

    pub fn get_ca_cert_pem(&self) -> String {
        self.ca_cert.pem()
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
    async fn test_load_from_pem_without_json() {
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

        // 3. Save as PEM (without the .json metadata file that RelayCore usually creates)
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

        // 4. Try to load it using load_or_create
        let ca = CertificateAuthority::load_or_create(&cert_path, &key_path)
            .expect("Should load from PEM");

        // 5. Verify the loaded CA
        // We check if we can generate a leaf cert
        let server_config = ca.gen_server_config("example.com").await;
        assert!(
            server_config.is_ok(),
            "Should generate server config successfully"
        );
    }
}
