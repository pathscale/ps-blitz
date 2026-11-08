//! One ChaCha20-Poly1305 offer and ECC-only authentication for HTTPS and WSS.

use std::sync::{Arc, OnceLock};

use ps_rustcrypto::policy::{EccOnlyVerifier, ct::CtVerifier};
use rustls::{RootCertStore, client::WebPkiServerVerifier, crypto::CryptoProvider};

pub fn chacha_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| {
        let mut provider = ps_rustcrypto::provider();
        provider.cipher_suites = vec![
            ps_rustcrypto::TLS13_CHACHA20_POLY1305_SHA256,
            ps_rustcrypto::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
        ];
        Arc::new(provider)
    }))
}

/// Normal path/hostname/time validation with CT on public trust anchors.
pub fn certificate_verifier(
    roots: RootCertStore,
    public: RootCertStore,
) -> Result<Arc<CtVerifier>, String> {
    let inner = EccOnlyVerifier::new(roots).map_err(|error| error.to_string())?;
    if public.is_empty() {
        return Ok(CtVerifier::without_public_roots(inner));
    }
    // CT checks this subset only after the ECC-only verifier accepts the chain.
    // The base provider cannot validate a chain under an RSA public anchor.
    let public = WebPkiServerVerifier::builder_with_provider(Arc::new(public), chacha_provider())
        .build()
        .map_err(|error| error.to_string())?;
    Ok(CtVerifier::public_only(inner, public))
}

/// Load trusted platform configuration before the process loses file rights.
pub fn prepare_native_tls() -> Result<(), String> {
    super::h1::prepare_native_tls()
}

#[cfg(all(feature = "sandbox2-witness", debug_assertions))]
pub(super) fn add_witness_roots(roots: &mut RootCertStore) -> Result<(), String> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    if let Some(path) = std::env::var_os("SANDBOX2_TLS_CA_FILE") {
        for certificate in CertificateDer::pem_file_iter(path).map_err(|error| error.to_string())? {
            roots
                .add(certificate.map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
