use relay_core_lib::tls::ca::CertificateAuthority;
use std::fs;
use std::sync::Once;
use tempfile::tempdir;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn test_load_without_metadata_fails_after_relay_core_ca_created() {
    init_crypto();

    let dir = tempdir().unwrap();
    let ca_cert_path = dir.path().join("ca.crt");
    let ca_key_path = dir.path().join("ca.key");
    let meta_path = dir.path().join("ca.json");

    CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path).unwrap();

    assert!(ca_cert_path.exists());
    assert!(ca_key_path.exists());
    assert!(meta_path.exists());

    fs::remove_file(&meta_path).unwrap();

    match CertificateAuthority::load_or_create(&ca_cert_path, &ca_key_path) {
        Ok(_) => panic!("expected missing metadata to fail"),
        Err(err) => assert!(
            err.to_string().contains("metadata file is missing"),
            "expected missing metadata error, got: {err}"
        ),
    }
}
