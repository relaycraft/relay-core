use relay_core_lib::tls::ca::CertificateAuthority;
use tempfile::tempdir;
use std::fs;
use std::sync::Arc;
use rustls::ServerConfig;

#[tokio::test]
async fn test_ca_load_from_pem_fallback() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let dir = tempdir().unwrap();
    let ca_cert_path = dir.path().join("ca.crt");
    let ca_key_path = dir.path().join("ca.key");
    let meta_path = dir.path().join("ca.json");

    // 1. Create a new CA with JSON metadata
    let ca1 = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();
    let pem1 = ca1.get_ca_cert_pem();
    
    assert!(ca_cert_path.exists());
    assert!(ca_key_path.exists());
    assert!(meta_path.exists());

    // 2. Delete the JSON metadata to force fallback to PEM parsing
    fs::remove_file(&meta_path).unwrap();
    assert!(!meta_path.exists());

    // 3. Load again - should use load_from_pem fallback
    let ca2 = CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();
    let pem2 = ca2.get_ca_cert_pem();

    println!("PEM1:\n{}", pem1);
    println!("PEM2:\n{}", pem2);
    
    // Check if we can generate a server cert from the loaded CA
    let server_config: Result<Arc<ServerConfig>, _> = ca2.gen_server_config("example.com").await;
    assert!(server_config.is_ok());
}
