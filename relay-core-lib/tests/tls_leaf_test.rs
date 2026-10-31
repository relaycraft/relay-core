//! Leaf-certificate coverage for wildcard and IDN hosts (roadmap §24.9).
//!
//! These are handshake tests rather than proxy tests: the question is whether the certificate the
//! CA mints for a host is one a standard rustls client accepts for that host. Running the proxy
//! would add CONNECT and DNS steps without testing anything new about the certificate.

use relay_core_lib::tls::CertificateAuthority;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn client_config(ca_pem: &str) -> ClientConfig {
    let mut roots = RootCertStore::empty();
    let mut reader = std::io::Cursor::new(ca_pem.as_bytes());
    for cert in rustls_pemfile::certs(&mut reader) {
        roots
            .add(cert.expect("CA PEM certificate"))
            .expect("add root");
    }
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Serve `server_config` once and try a client handshake with `sni`.
async fn handshake(ca: &CertificateAuthority, cert_host: &str, sni: &str) -> Result<(), String> {
    let server_config = ca
        .gen_server_config(cert_host)
        .await
        .map_err(|e| format!("gen_server_config({cert_host}): {e}"))?;
    let acceptor = TlsAcceptor::from(server_config);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind TLS listener");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        if let Ok((socket, _)) = listener.accept().await {
            let _ = acceptor.accept(socket).await;
        }
    });

    let connector = TlsConnector::from(Arc::new(client_config(&ca.get_ca_cert_pem())));
    let name = ServerName::try_from(sni.to_string()).map_err(|e| e.to_string())?;
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect TLS listener");
    let result = connector.connect(name, tcp).await.map(|_| ());
    server.abort();
    result.map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_wildcard_leaf_matches_a_subdomain_but_not_the_apex() {
    init_crypto();
    let ca = CertificateAuthority::new().expect("CA");

    handshake(&ca, "*.example.com", "api.example.com")
        .await
        .expect("a wildcard leaf must satisfy a subdomain");

    let apex = handshake(&ca, "*.example.com", "example.com").await;
    assert!(
        apex.is_err(),
        "a wildcard must not match its own apex, got {apex:?}"
    );
}

#[tokio::test]
async fn an_idn_host_is_minted_and_verified_by_its_punycode_name() {
    init_crypto();
    let ca = CertificateAuthority::new().expect("CA");

    // Punycode is what rustls sees on the wire; the CA must mint the SAN in the same form.
    handshake(&ca, "xn--bcher-kva.example", "xn--bcher-kva.example")
        .await
        .expect("an IDN leaf must verify against its punycode SNI");
}
