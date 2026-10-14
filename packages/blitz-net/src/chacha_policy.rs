//! One cipher offer for HTTPS and WSS. RSA is verification only.

use std::sync::{Arc, OnceLock};

use ps_rustcrypto::policy::ct::CtVerifier;
use rustls::pki_types::SignatureVerificationAlgorithm;
use rustls::{
    CipherSuite, RootCertStore, SignatureScheme, SupportedCipherSuite, Tls12CipherSuite,
    client::{WebPkiServerVerifier, danger::ServerCertVerifier},
    crypto::{CipherSuiteCommon, CryptoProvider},
};

pub fn chacha_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| {
        let mut provider = ps_rustcrypto::provider_with_rsa_pss();
        static CERT_ALGORITHMS: OnceLock<Vec<&'static dyn SignatureVerificationAlgorithm>> =
            OnceLock::new();
        let algorithms = CERT_ALGORITHMS.get_or_init(|| {
            let mut all = provider.signature_verification_algorithms.all.to_vec();
            // Include RSA-PSS certificate signatures as well as the provider's
            // RSA PKCS#1 v1.5 chain signatures. No RSA private-key operation.
            for (scheme, verifiers) in provider.signature_verification_algorithms.mapping {
                if matches!(
                    scheme,
                    SignatureScheme::RSA_PSS_SHA256
                        | SignatureScheme::RSA_PSS_SHA384
                        | SignatureScheme::RSA_PSS_SHA512
                ) {
                    all.extend_from_slice(verifiers);
                }
            }
            all
        });
        provider.signature_verification_algorithms.all = algorithms;
        static RSA_SUITE: OnceLock<Tls12CipherSuite> = OnceLock::new();
        let rsa = RSA_SUITE.get_or_init(|| {
            let SupportedCipherSuite::Tls12(base) =
                ps_rustcrypto::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
            else {
                panic!("the TLS 1.2 provider constant changed protocol");
            };
            Tls12CipherSuite {
                common: CipherSuiteCommon {
                    suite: CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
                    hash_provider: base.common.hash_provider,
                    confidentiality_limit: base.common.confidentiality_limit,
                },
                prf_provider: base.prf_provider,
                kx: base.kx,
                // Verify the server certificate's proof of possession; never
                // RSA key exchange, encryption, private keys or client auth.
                sign: &[
                    SignatureScheme::RSA_PSS_SHA256,
                    SignatureScheme::RSA_PSS_SHA384,
                    SignatureScheme::RSA_PSS_SHA512,
                ],
                aead_alg: base.aead_alg,
            }
        });
        provider.cipher_suites = vec![
            ps_rustcrypto::TLS13_CHACHA20_POLY1305_SHA256,
            ps_rustcrypto::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
            SupportedCipherSuite::Tls12(rsa),
        ];
        Arc::new(provider)
    }))
}

/// Normal path/hostname/time validation with CT on public trust anchors.
pub fn certificate_verifier(
    roots: RootCertStore,
    public: RootCertStore,
) -> Result<Arc<CtVerifier>, String> {
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), chacha_provider())
        .build()
        .map_err(|error| error.to_string())?;
    let inner: Arc<dyn ServerCertVerifier> = inner;
    if public.is_empty() {
        return Ok(CtVerifier::without_public_roots(inner));
    }
    let public = WebPkiServerVerifier::builder_with_provider(Arc::new(public), chacha_provider())
        .build()
        .map_err(|error| error.to_string())?;
    Ok(CtVerifier::public_only(inner, public))
}

/// Load trusted platform configuration before the process loses file rights.
pub fn prepare_native_tls() -> Result<(), String> {
    super::h1::prepare_native_tls()
}

#[cfg(feature = "sandbox2-witness")]
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
