//! Building the HTTP client that dials out, from [`UpstreamTlsPolicy`].
//!
//! One place owns this because the verification mode must be the same for every consumer of the
//! outbound client, and because "which roots are trusted" is a security property: a second builder
//! that defaulted to native roots would quietly undo a deployment's custom CA.

use crate::proxy::http_utils::HttpsClient;
use hyper_rustls::{ConfigBuilderExt, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use relay_core_api::policy::{UpstreamTlsPolicy, UpstreamTlsVerification};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::sync::Arc;
use std::time::Duration;

/// Build the client every outbound HTTPS/TLS connection goes through.
///
/// Fails instead of falling back: a custom CA bundle that cannot be read is an operator error, and
/// substituting the native roots for it would make the proxy appear to work while trusting a
/// different set of peers than the policy says.
pub fn build_upstream_client(policy: &UpstreamTlsPolicy) -> Result<HttpsClient, String> {
    let config = build_client_config(policy)?;
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Ok(Client::builder(hyper_util::rt::TokioExecutor::new())
        .timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(32)
        .http2_initial_stream_window_size(2 * 1024 * 1024)
        .http2_initial_connection_window_size(4 * 1024 * 1024)
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .build(https))
}

/// The TLS configuration itself, for callers that make their own connector.
///
/// The HTTPS upstream-proxy connector needs the same trust decisions as the direct client: an
/// internal CA or an explicit insecure mode must not stop at the target and silently not apply to
/// the proxy in front of it. One function therefore owns the decision for both.
pub fn build_client_config(policy: &UpstreamTlsPolicy) -> Result<ClientConfig, String> {
    validate(policy)?;

    // `ClientConfig::builder()` panics when no process default is installed. Installing ring here
    // is idempotent, and it makes this function safe to call from a test or an embedding that never
    // constructed a TLS server first.
    let _ = rustls::crypto::ring::default_provider().install_default();

    Ok(match &policy.verification {
        UpstreamTlsVerification::NativeRoots => ClientConfig::builder()
            .with_native_roots()
            .map_err(|e| format!("could not load the native trust store: {e}"))?
            .with_no_client_auth(),
        UpstreamTlsVerification::CustomCa { path } => ClientConfig::builder()
            .with_root_certificates(load_custom_ca(path)?)
            .with_no_client_auth(),
        UpstreamTlsVerification::Insecure { reason } => {
            tracing::warn!(
                reason = %reason,
                "upstream certificate verification is DISABLED; this engine will accept any \
                 certificate presented by a target or by an HTTPS parent proxy"
            );
            let mut config = ClientConfig::builder()
                .with_root_certificates(RootCertStore::empty())
                .with_no_client_auth();
            config
                .dangerous()
                .set_certificate_verifier(Arc::new(AcceptAnyCertificate));
            config
        }
    })
}

/// Trust exactly the certificates in `path`.
///
/// The bundle is the root set, not an addition to the native roots: a deployment that installs an
/// internal CA expects the public roots to stop applying, and mixing them would hide a chain that
/// is only complete because an unrelated public root happened to be present.
/// Check a policy without building a client.
///
/// The runtime rejects a policy it cannot honour **before** storing it. That ordering matters when
/// an operator tightens verification: if the new bundle is unreadable and the policy were stored
/// anyway, the API would report `custom_ca` while the running client kept the previous, possibly
/// insecure, trust settings.
pub fn validate(policy: &UpstreamTlsPolicy) -> Result<(), String> {
    match &policy.verification {
        UpstreamTlsVerification::NativeRoots => Ok(()),
        UpstreamTlsVerification::CustomCa { path } => load_custom_ca(path).map(|_| ()),
        UpstreamTlsVerification::Insecure { reason } => {
            if reason.trim().is_empty() {
                Err(
                    "insecure upstream verification requires a non-empty reason; refusing to \
                     disable certificate checking silently"
                        .to_string(),
                )
            } else {
                Ok(())
            }
        }
    }
}

fn load_custom_ca(path: &std::path::Path) -> Result<RootCertStore, String> {
    let pem = std::fs::read(path).map_err(|e| {
        format!(
            "could not read the custom CA bundle {}: {e}",
            path.display()
        )
    })?;
    let mut reader = std::io::Cursor::new(pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{} is not a PEM certificate bundle: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!(
            "{} contains no certificates; refusing to trust an empty store",
            path.display()
        ));
    }

    let mut store = RootCertStore::empty();
    for cert in certs {
        store
            .add(cert)
            .map_err(|e| format!("{} contains an unusable certificate: {e}", path.display()))?;
    }
    Ok(store)
}

/// Accepts every certificate. Only reachable through an explicit `Insecure { reason }`.
#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core_api::policy::UpstreamTlsVerification;

    fn policy(verification: UpstreamTlsVerification) -> UpstreamTlsPolicy {
        UpstreamTlsPolicy { verification }
    }

    #[test]
    fn native_roots_build_succeeds() {
        build_upstream_client(&UpstreamTlsPolicy::default()).expect("native roots must build");
    }

    #[test]
    fn an_insecure_policy_without_a_reason_is_refused() {
        let err = build_upstream_client(&policy(UpstreamTlsVerification::Insecure {
            reason: "   ".to_string(),
        }))
        .expect_err("an empty reason must not disable verification");
        assert!(err.contains("requires a non-empty reason"), "got {err}");
    }

    #[test]
    fn a_missing_custom_ca_is_an_error_not_a_fallback() {
        let err = build_upstream_client(&policy(UpstreamTlsVerification::CustomCa {
            path: "/nonexistent/relaycore-ca.pem".into(),
        }))
        .expect_err("a missing bundle must fail the build");
        assert!(err.contains("could not read"), "got {err}");
    }

    #[test]
    fn a_bundle_without_certificates_is_refused() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("relaycore-empty-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&path, "# no certificates here\n").expect("write fixture");
        let err = build_upstream_client(&policy(UpstreamTlsVerification::CustomCa {
            path: path.clone(),
        }))
        .expect_err("an empty bundle must be refused");
        let _ = std::fs::remove_file(&path);
        assert!(err.contains("no certificates"), "got {err}");
    }

    #[test]
    fn a_valid_custom_ca_builds() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate a certificate");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("relaycore-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&path, cert.cert.pem()).expect("write fixture");
        let built = build_upstream_client(&policy(UpstreamTlsVerification::CustomCa {
            path: path.clone(),
        }));
        let _ = std::fs::remove_file(&path);
        built.expect("a valid PEM bundle must build");
    }
}
